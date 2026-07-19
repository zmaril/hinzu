//! The porting dependency graph: a serializable dependency graph of a codebase,
//! shaped for an AI-assisted port. Where [`crate::effects`] reasons *up* the call
//! graph (which callers a root reaches), this module reasons *down* it (what each
//! symbol depends on) and answers a different question: **in what order should
//! a porting agent move code, so that whenever it ports a symbol, everything
//! that symbol depends on is already ported?**
//!
//! The graph itself is **not** acyclic — mutual recursion and back-and-forth
//! calls are real, so it may contain cycles. The order is well-defined anyway
//! because we compute it over the **condensation**: each strongly-connected
//! component (a call cycle) is collapsed to a single node, and *that* graph is
//! acyclic, so a dependencies-first (leaves-first) topological order exists. The
//! members of a cycle are emitted contiguously and must be ported together. The
//! output is a plain data structure ([`GraphOutput`]) so it can be emitted as
//! JSON and walked by a tool that knows nothing about hinzu's internals.
//!
//! ## Fidelity, stated honestly
//!
//! The graph is **call-only**: an edge means "caller calls or references
//! callee", derived from the same call/use facts the effect engine consumes.
//! Higher-order calls, dynamic dispatch, and callbacks the adapter could not
//! resolve are approximated or missed; an unresolved target is marked
//! `provenance = "unknown"` rather than silently dropped. File-level edges are
//! *inferred* by projecting symbol call edges onto their files — there is no
//! separate imports/implementation table — so a file dependency that flows only
//! through types (never a call) is not represented. These caveats are carried
//! in [`Fidelity`] so a consumer sees them next to the data.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::facts::{EdgeResolution, FactSet, SymbolId};

/// The schema version embedded in every emitted graph, so a consumer can branch
/// on shape changes.
pub const HINZU_GRAPH_VERSION: u32 = 1;

/// A node in the symbol dependency graph: one per local definition, plus one
/// per external call target (a callee with no local definition).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SymbolNode {
    /// The stable symbol id (the graph key).
    pub id: String,
    /// The short human name (for an external node, the id itself).
    pub display: String,
    /// The defining file, or `null` for an external node.
    pub file: Option<String>,
    /// The source language, or `null` for an external node.
    pub language: Option<String>,
    /// First source line, or `null` for an external node.
    pub line_start: Option<u32>,
    /// Last source line, or `null` for an external node.
    pub line_end: Option<u32>,
    /// Lines of code (`line_end - line_start + 1`), or `null` externally.
    pub loc: Option<u32>,
    /// Whether this is an external target (no local definition). External nodes
    /// are always leaves — library boundaries, not port targets.
    pub external: bool,
    /// Distinct callers (in-degree over the full call graph).
    pub fan_in: usize,
    /// Distinct callees (out-degree over the full call graph, external included).
    pub fan_out: usize,
    /// Distinct symbols reachable downward (transitive callees, external
    /// included), excluding self — a rough size for "porting this pulls in".
    pub transitive_dep_count: usize,
    /// Whether this symbol has no *internal* (non-external) dependency: it can be
    /// ported first. External callees do not count against leaf status.
    pub is_leaf: bool,
    /// The effect categories this symbol transitively reaches, via the
    /// propagation engine over the facts' effect roots. Best-effort: empty when
    /// no effect roots are seeded. Always empty for an external node.
    pub effect_roots: Vec<String>,
    /// Distinct package prefixes of the external callees this symbol calls
    /// (the leading `::`-delimited segment), sorted.
    pub external_packages: Vec<String>,
    /// The strongly-connected-component group id when this symbol is in a
    /// non-trivial call cycle (`"scc:N"`), else `null`.
    pub scc: Option<String>,
}

/// An edge in the symbol dependency graph: "from calls/references to".
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SymbolEdge {
    /// The caller symbol id.
    pub from: String,
    /// The callee symbol id.
    pub to: String,
    /// The edge kind (`"call"` or `"reference"`).
    pub kind: String,
    /// The adapter's resolution provenance (`"call"`, `"reference"`,
    /// `"value-flow"`, `"unresolved"`).
    pub resolution: String,
    /// How the endpoint resolves: `"resolved"` (to a local definition),
    /// `"external"` (to an external package target), or `"unknown"` (an
    /// unresolved target, or one seeded as `Unknown` — fail closed).
    pub provenance: String,
    /// The file the edge was observed in.
    pub evidence_file: String,
    /// The line the edge was observed at.
    pub evidence_line: u32,
}

/// A file-rollup node: the symbol graph aggregated onto its defining files.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileNode {
    /// The file path (the graph key).
    pub path: String,
    /// How many local definitions live in this file.
    pub symbol_count: usize,
    /// Total lines of code across those definitions.
    pub loc: u32,
    /// Distinct files that depend on this one.
    pub fan_in: usize,
    /// Distinct files this one depends on.
    pub fan_out: usize,
    /// Distinct files reachable downward, excluding self.
    pub transitive_dep_count: usize,
    /// Whether this file depends on no other file: it can be ported first.
    pub is_leaf: bool,
    /// The union of its symbols' reachable effect categories, sorted.
    pub effect_roots: Vec<String>,
    /// The union of its symbols' external package prefixes, sorted.
    pub external_packages: Vec<String>,
    /// The file-level SCC group id when this file is in a dependency cycle, else
    /// `null`.
    pub scc: Option<String>,
}

/// A file-rollup edge: the aggregate of the symbol call edges that cross from
/// one file into another (self-loops dropped).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileEdge {
    /// The depending file.
    pub from: String,
    /// The depended-on file.
    pub to: String,
    /// How many symbol call edges project onto this file pair.
    pub call_edge_count: usize,
    /// Whether any contributing symbol edge was itself unresolved.
    pub has_unknown: bool,
}

/// One strongly-connected component (a call cycle) reported to the consumer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SccGroup {
    /// The group id (`"scc:N"`), matching the `scc` field on its members.
    pub id: String,
    /// The member ids, sorted. Port these together.
    pub members: Vec<String>,
}

/// The acyclic SCC-condensation a porting agent walks: the port order, the
/// cycles, and the first batch of leaves. The dependency graph may contain
/// cycles; collapsing each strongly-connected component to one node yields this
/// acyclic view, which is what makes the ordering well-defined.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Condensation {
    /// Every local symbol in **dependencies-first** order: a symbol appears only
    /// after all of its callees, so popping from the front is always safe to
    /// port. Members of an SCC are emitted as a contiguous block.
    pub symbol_topo_order: Vec<String>,
    /// Every file in dependencies-first order, same semantics.
    pub file_topo_order: Vec<String>,
    /// The non-trivial symbol SCCs (call cycles of size > 1).
    pub symbol_sccs: Vec<SccGroup>,
    /// The non-trivial file SCCs.
    pub file_sccs: Vec<SccGroup>,
    /// Symbols with no internal dependency — the first batch to port.
    pub symbol_leaves: Vec<String>,
    /// Files with no file dependency — the first batch to port.
    pub file_leaves: Vec<String>,
}

/// The call-only fidelity of this graph, stated so a consumer sees the caveats
/// next to the data.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Fidelity {
    /// Always true: edges are derived from the call/use graph only.
    pub call_only: bool,
    /// Human-readable caveats about what the graph does and does not capture.
    pub notes: Vec<String>,
    /// How many symbol edges resolve to an unknown/unresolved target.
    pub unknown_edge_count: usize,
    /// How many external (no-local-definition) target nodes there are.
    pub external_node_count: usize,
}

/// Aggregate counts for the whole graph.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stats {
    /// Local (internal) symbols.
    pub symbol_count: usize,
    /// External target nodes.
    pub external_count: usize,
    /// Distinct files.
    pub file_count: usize,
    /// Symbol edges.
    pub edge_count: usize,
    /// File-rollup edges.
    pub file_edge_count: usize,
    /// Non-trivial symbol SCCs.
    pub scc_count: usize,
}

/// The complete graph document, ready to serialize as JSON.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GraphOutput {
    /// The schema version ([`HINZU_GRAPH_VERSION`]).
    pub hinzu_graph_version: u32,
    /// The analyzed target (a label — usually the project path).
    pub root: String,
    /// The dominant source language, if one could be determined.
    pub language: Option<String>,
    /// The call-only fidelity caveats and counts.
    pub fidelity: Fidelity,
    /// Aggregate counts.
    pub stats: Stats,
    /// The symbol nodes, sorted by id.
    pub symbols: Vec<SymbolNode>,
    /// The symbol edges, in fact order.
    pub edges: Vec<SymbolEdge>,
    /// The file-rollup nodes, sorted by path.
    pub files: Vec<FileNode>,
    /// The file-rollup edges, sorted by (from, to).
    pub file_edges: Vec<FileEdge>,
    /// The acyclic SCC-condensation and the port-order utilities derived from it.
    pub condensation: Condensation,
}

/// The leading `::`-delimited segment of a symbol id — the "package" an external
/// callee belongs to (`subprocess::run` → `subprocess`, `node:fs::readFileSync`
/// → `node:fs`). Ids without `::` are their own package.
fn package_of(id: &str) -> String {
    match id.split_once("::") {
        Some((pkg, _)) => pkg.to_string(),
        None => id.to_string(),
    }
}

/// The count of distinct nodes reachable downward from `start` over `adj`,
/// excluding `start` itself. Cycles terminate: a node is expanded at most once.
fn transitive_count(start: &str, adj: &BTreeMap<String, BTreeSet<String>>) -> usize {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut stack: Vec<String> = vec![start.to_string()];
    while let Some(node) = stack.pop() {
        if let Some(deps) = adj.get(&node) {
            for dep in deps {
                if seen.insert(dep.clone()) {
                    stack.push(dep.clone());
                }
            }
        }
    }
    seen.remove(start);
    seen.len()
}

/// Tarjan's strongly-connected components over a dependency adjacency (node →
/// the nodes it depends on). Iterative, so a deep graph cannot overflow the
/// stack. The returned components are in **dependencies-first** order — Tarjan
/// finishes a sink (a leaf with no dependencies) before the nodes that depend on
/// it — which is exactly the port order. Members within a component are sorted.
fn strongly_connected_components(adj: &BTreeMap<String, BTreeSet<String>>) -> Vec<Vec<String>> {
    let nodes: Vec<&String> = adj.keys().collect();
    let n = nodes.len();
    let index_of: BTreeMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, name)| (name.as_str(), i))
        .collect();
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (name, deps) in adj {
        let u = index_of[name.as_str()];
        for dep in deps {
            if let Some(&v) = index_of.get(dep.as_str()) {
                succ[u].push(v);
            }
        }
    }

    let mut index: Vec<Option<usize>> = vec![None; n];
    let mut lowlink: Vec<usize> = vec![0; n];
    let mut on_stack: Vec<bool> = vec![false; n];
    let mut tarjan_stack: Vec<usize> = Vec::new();
    let mut counter = 0usize;
    let mut components: Vec<Vec<String>> = Vec::new();

    for start in 0..n {
        if index[start].is_some() {
            continue;
        }
        // Each frame is (node, next-child-position).
        let mut work: Vec<(usize, usize)> = vec![(start, 0)];
        while let Some(&(v, pos)) = work.last() {
            if pos == 0 {
                index[v] = Some(counter);
                lowlink[v] = counter;
                counter += 1;
                tarjan_stack.push(v);
                on_stack[v] = true;
            }
            if pos < succ[v].len() {
                let w = succ[v][pos];
                work.last_mut().unwrap().1 += 1;
                if index[w].is_none() {
                    work.push((w, 0));
                } else if on_stack[w] {
                    lowlink[v] = lowlink[v].min(index[w].unwrap());
                }
            } else {
                if lowlink[v] == index[v].unwrap() {
                    let mut comp: Vec<String> = Vec::new();
                    loop {
                        let w = tarjan_stack.pop().unwrap();
                        on_stack[w] = false;
                        comp.push(nodes[w].clone());
                        if w == v {
                            break;
                        }
                    }
                    comp.sort();
                    components.push(comp);
                }
                work.pop();
                if let Some(&(parent, _)) = work.last() {
                    lowlink[parent] = lowlink[parent].min(lowlink[v]);
                }
            }
        }
    }
    components
}

/// The SCC condensation packaged for output: the components in port order, the
/// per-node group-id map (only for non-trivial components), and the reported
/// [`SccGroup`]s. A `scc:N` id is minted per non-trivial component in port order.
struct SccCondensation {
    /// Components (each already sorted) in dependencies-first order.
    order: Vec<Vec<String>>,
    /// node id → its SCC group id, for members of non-trivial components only.
    group_of: BTreeMap<String, String>,
    /// The non-trivial components, as reported groups.
    groups: Vec<SccGroup>,
}

/// Condense a dependency adjacency into ordered components + group ids.
fn condense(adj: &BTreeMap<String, BTreeSet<String>>) -> SccCondensation {
    let order = strongly_connected_components(adj);
    let mut group_of: BTreeMap<String, String> = BTreeMap::new();
    let mut groups: Vec<SccGroup> = Vec::new();
    for comp in &order {
        if comp.len() > 1 {
            let id = format!("scc:{}", groups.len());
            for member in comp {
                group_of.insert(member.clone(), id.clone());
            }
            groups.push(SccGroup {
                id,
                members: comp.clone(),
            });
        }
    }
    SccCondensation {
        order,
        group_of,
        groups,
    }
}

/// Per-node metadata for a local definition, carried into [`assemble_graph`].
/// External nodes have no metadata (`None`).
struct NodeMeta {
    display: String,
    file: String,
    language: String,
    line_start: u32,
    line_end: u32,
}

/// Build the porting dependency graph from a fact set.
///
/// `root` is a free-form label for the analyzed target (usually the project
/// path). `language` is the dominant language spelling for the top-level field;
/// when `None`, it is inferred from the definitions. The facts' effect roots (if
/// any were seeded) drive the per-symbol `effect_roots` — this function reads
/// whatever is present and never requires a policy.
///
/// This resolves the nodes, provenance-tags the edges, and rolls up each
/// symbol's reachable effects from the facts, then hands off to the shared
/// [`assemble_graph`] for all the derived structure (adjacency, fan-in/out,
/// transitive counts, the file rollup, and the SCC condensation) — the same
/// assembly a rooted closure ([`filter_to_closure`]) reuses.
pub fn build_graph(facts: &FactSet, root: &str, language: Option<&str>) -> GraphOutput {
    // Every node: local definitions (with metadata) plus every edge endpoint
    // (external targets have no definition but must still be resolvable).
    let mut nodes: BTreeMap<String, Option<NodeMeta>> = BTreeMap::new();
    for (id, d) in &facts.defs {
        nodes.insert(
            id.clone(),
            Some(NodeMeta {
                display: d.display.clone(),
                file: d.file.clone(),
                language: d.language.as_str().to_string(),
                line_start: d.line_start,
                line_end: d.line_end,
            }),
        );
    }
    for edge in &facts.edges {
        nodes.entry(edge.caller.clone()).or_insert(None);
        nodes.entry(edge.callee.clone()).or_insert(None);
    }

    // Per-symbol reachable effects, via the propagation engine over whatever
    // roots the facts carry. Best-effort: empty when nothing is seeded.
    let summaries = crate::effects::propagate(facts);
    let effect_roots: BTreeMap<String, Vec<String>> = facts
        .defs
        .keys()
        .map(|id| {
            let effs = summaries
                .get(id)
                .map(|s| s.effects.iter().map(|e| e.as_str().to_string()).collect())
                .unwrap_or_default();
            (id.clone(), effs)
        })
        .collect();

    // Symbol edges, provenance-tagged against the fact set.
    let unknown_root_set: BTreeSet<&str> = facts
        .roots
        .iter()
        .filter(|r| r.effect == crate::facts::Effect::Unknown)
        .map(|r| r.symbol.as_str())
        .collect();
    let mut edges: Vec<SymbolEdge> = Vec::with_capacity(facts.edges.len());
    for edge in &facts.edges {
        let provenance = if facts.defs.contains_key(&edge.callee) {
            "resolved"
        } else if edge.resolution == EdgeResolution::Unresolved
            || unknown_root_set.contains(edge.callee.as_str())
        {
            "unknown"
        } else {
            "external"
        };
        edges.push(SymbolEdge {
            from: edge.caller.clone(),
            to: edge.callee.clone(),
            kind: edge.kind.as_str().to_string(),
            resolution: edge.resolution.as_str().to_string(),
            provenance: provenance.to_string(),
            evidence_file: edge.evidence_file.clone(),
            evidence_line: edge.evidence_line,
        });
    }

    assemble_graph(root, language, &nodes, edges, &effect_roots)
}

/// The transitive **dependency** closure of `roots` over the symbol graph:
/// `roots` plus every symbol reachable by following caller→callee (out) edges.
/// This is "everything the roots need to run" — external leaves are included, as
/// the assumed-available library boundary the closure bottoms out on. A root
/// with no outgoing edges yields just itself.
pub fn dependency_closure(graph: &GraphOutput, roots: &[SymbolId]) -> BTreeSet<SymbolId> {
    // Out-adjacency straight off the symbol edges: `from` depends on `to`.
    let mut out: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for edge in &graph.edges {
        out.entry(edge.from.as_str())
            .or_default()
            .push(edge.to.as_str());
    }
    let mut seen: BTreeSet<SymbolId> = BTreeSet::new();
    let mut stack: Vec<String> = Vec::new();
    for r in roots {
        if seen.insert(r.clone()) {
            stack.push(r.clone());
        }
    }
    while let Some(node) = stack.pop() {
        if let Some(deps) = out.get(node.as_str()) {
            for &dep in deps {
                if seen.insert(dep.to_string()) {
                    stack.push(dep.to_string());
                }
            }
        }
    }
    seen
}

/// Restrict `graph` to the dependency closure of `roots`: keep only closure
/// symbols and the edges among them, then recompute every derived field
/// (fan-in/out, transitive counts, the file rollup, and the SCC condensation)
/// for the sub-graph via the shared [`assemble_graph`]. The result is a
/// standalone [`GraphOutput`] — plan-able exactly like a full one, but scoped to
/// "what these roots need". Effect rollups carry over unchanged: because the
/// closure includes every transitive dependency, each symbol's reachable
/// effects are identical to the full graph's.
pub fn filter_to_closure(graph: &GraphOutput, roots: &[SymbolId]) -> GraphOutput {
    let closure = dependency_closure(graph, roots);

    // Node metadata + effect rollups for the closure, straight off the existing
    // symbol nodes (every closure member is a node — build_graph emits one per
    // edge endpoint, external targets included).
    let mut nodes: BTreeMap<String, Option<NodeMeta>> = BTreeMap::new();
    let mut effect_roots: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for sym in &graph.symbols {
        if !closure.contains(&sym.id) {
            continue;
        }
        let meta = if sym.external {
            None
        } else {
            effect_roots.insert(sym.id.clone(), sym.effect_roots.clone());
            Some(NodeMeta {
                display: sym.display.clone(),
                file: sym.file.clone().unwrap_or_default(),
                language: sym.language.clone().unwrap_or_default(),
                line_start: sym.line_start.unwrap_or(0),
                line_end: sym.line_end.unwrap_or(0),
            })
        };
        nodes.insert(sym.id.clone(), meta);
    }

    // Edges among closure members only.
    let edges: Vec<SymbolEdge> = graph
        .edges
        .iter()
        .filter(|e| nodes.contains_key(&e.from) && nodes.contains_key(&e.to))
        .cloned()
        .collect();

    assemble_graph(
        &graph.root,
        graph.language.as_deref(),
        &nodes,
        edges,
        &effect_roots,
    )
}

/// A single `--from` pattern resolving to more than this many symbols is called
/// out in [`RootResolution::notes`], so the operator sees a broad match.
const AMBIGUOUS_ROOT_THRESHOLD: usize = 8;

/// The outcome of resolving user `--from` patterns into concrete root symbols.
#[derive(Clone, Debug)]
pub struct RootResolution {
    /// The resolved root symbol ids, deduplicated and in graph (id) order.
    pub roots: Vec<SymbolId>,
    /// Human-readable notes about what each pattern resolved to — file
    /// expansions and broad/ambiguous matches — for surfacing on stderr.
    pub notes: Vec<String>,
}

/// The `::`-delimited leaf of a symbol id (`app::run` → `run`), matching the
/// extractor's `display` convention.
fn leaf_name(id: &str) -> &str {
    id.rsplit("::").next().unwrap_or(id)
}

/// A few symbols "near" a pattern, for a not-found error message: symbols whose
/// id or display contains the pattern case-insensitively, else the first handful
/// of internal symbols as examples.
fn near_misses(graph: &GraphOutput, pattern: &str) -> Vec<String> {
    let needle = pattern.to_lowercase();
    let mut hits: Vec<String> = graph
        .symbols
        .iter()
        .filter(|s| {
            s.id.to_lowercase().contains(&needle) || s.display.to_lowercase().contains(&needle)
        })
        .map(|s| s.id.clone())
        .take(5)
        .collect();
    if hits.is_empty() {
        hits = graph
            .symbols
            .iter()
            .filter(|s| !s.external)
            .map(|s| s.id.clone())
            .take(5)
            .collect();
    }
    hits
}

/// Resolve one `--from` pattern to a set of symbol ids. Tiers are tried in order
/// and the first non-empty tier wins:
///   1. exact symbol id;
///   2. symbol whose id **ends with** the pattern, or whose display/leaf name
///      equals it;
///   3. substring match on the symbol id;
///   4. file match — path equals or contains the pattern → every symbol defined
///      in that file.
///
/// Returns an empty set when nothing matches (the caller turns that into an
/// error naming near-misses).
fn resolve_one(graph: &GraphOutput, pattern: &str) -> BTreeSet<SymbolId> {
    // Tier 1: exact id.
    let exact: BTreeSet<SymbolId> = graph
        .symbols
        .iter()
        .filter(|s| s.id == pattern)
        .map(|s| s.id.clone())
        .collect();
    if !exact.is_empty() {
        return exact;
    }
    // Tier 2: ends-with id, or display/leaf name equals.
    let by_name: BTreeSet<SymbolId> = graph
        .symbols
        .iter()
        .filter(|s| s.id.ends_with(pattern) || s.display == pattern || leaf_name(&s.id) == pattern)
        .map(|s| s.id.clone())
        .collect();
    if !by_name.is_empty() {
        return by_name;
    }
    // Tier 3: substring on the id.
    let by_substr: BTreeSet<SymbolId> = graph
        .symbols
        .iter()
        .filter(|s| s.id.contains(pattern))
        .map(|s| s.id.clone())
        .collect();
    if !by_substr.is_empty() {
        return by_substr;
    }
    // Tier 4: file match → expand to all symbols defined in the matching file(s).
    graph
        .symbols
        .iter()
        .filter(|s| {
            s.file
                .as_deref()
                .map(|f| f == pattern || f.contains(pattern))
                .unwrap_or(false)
        })
        .map(|s| s.id.clone())
        .collect()
}

/// Resolve user `--from` patterns to a concrete, deduplicated set of root symbol
/// ids (see [`resolve_one`] for the per-pattern match rules). Every pattern must
/// match at least one symbol; a pattern that matches nothing is an `Err` naming
/// a few near-misses. A pattern that resolves to more than one symbol (a file
/// expansion or a broad match) is kept as a union and reported in
/// [`RootResolution::notes`].
pub fn resolve_roots(graph: &GraphOutput, patterns: &[String]) -> Result<RootResolution, String> {
    let mut roots: BTreeSet<SymbolId> = BTreeSet::new();
    let mut notes: Vec<String> = Vec::new();
    for pattern in patterns {
        let matched = resolve_one(graph, pattern);
        if matched.is_empty() {
            let near = near_misses(graph, pattern);
            return Err(format!(
                "--from '{}' matched no symbol or file; did you mean one of: {}",
                pattern,
                near.join(", ")
            ));
        }
        if matched.len() > 1 {
            let shown: Vec<String> = matched
                .iter()
                .take(AMBIGUOUS_ROOT_THRESHOLD)
                .cloned()
                .collect();
            let suffix = if matched.len() > AMBIGUOUS_ROOT_THRESHOLD {
                format!(", … (+{} more)", matched.len() - AMBIGUOUS_ROOT_THRESHOLD)
            } else {
                String::new()
            };
            notes.push(format!(
                "--from '{}' resolved to {} symbols: {}{}",
                pattern,
                matched.len(),
                shown.join(", "),
                suffix
            ));
        }
        roots.extend(matched);
    }
    Ok(RootResolution {
        roots: roots.into_iter().collect(),
        notes,
    })
}

/// Assemble a [`GraphOutput`] from a resolved node set, provenance-tagged edge
/// list, and per-node effect rollups. This is the post-edge assembly shared by
/// the full build ([`build_graph`]) and the rooted closure filter
/// ([`filter_to_closure`]): it derives the full/internal adjacency, fan-in/out,
/// transitive counts, the SCC condensation, and the file rollup **purely** from
/// `nodes` + `edges`, so a sub-graph reuses exactly the same math as the whole
/// graph.
///
/// `nodes` maps every node id (internal *and* external) to its metadata, or
/// `None` for an external target. Every endpoint of every edge in `edges` must
/// be a key in `nodes`. `effect_roots` gives each internal node its reachable
/// effect categories (external nodes always report none).
fn assemble_graph(
    root: &str,
    language: Option<&str>,
    nodes: &BTreeMap<String, Option<NodeMeta>>,
    edges: Vec<SymbolEdge>,
    effect_roots: &BTreeMap<String, Vec<String>>,
) -> GraphOutput {
    let is_internal = |id: &str| nodes.get(id).map(|m| m.is_some()).unwrap_or(false);

    // Full dependency adjacency (caller → distinct callees, external included,
    // self-loops dropped).
    let mut full_deps: BTreeMap<String, BTreeSet<String>> =
        nodes.keys().map(|n| (n.clone(), BTreeSet::new())).collect();
    // Internal dependency adjacency (both endpoints are local definitions): the
    // graph the leaves/SCCs/topo order are computed over.
    let mut int_deps: BTreeMap<String, BTreeSet<String>> = nodes
        .iter()
        .filter(|(_, m)| m.is_some())
        .map(|(n, _)| (n.clone(), BTreeSet::new()))
        .collect();
    for edge in &edges {
        if edge.from == edge.to {
            continue;
        }
        full_deps
            .get_mut(&edge.from)
            .expect("from is a known node")
            .insert(edge.to.clone());
        if is_internal(&edge.from) && is_internal(&edge.to) {
            int_deps
                .get_mut(&edge.from)
                .expect("from is a local definition")
                .insert(edge.to.clone());
        }
    }

    // Reverse of the full graph, for fan-in.
    let mut callers_of: BTreeMap<String, BTreeSet<String>> =
        nodes.keys().map(|n| (n.clone(), BTreeSet::new())).collect();
    for (caller, callees) in &full_deps {
        for callee in callees {
            callers_of
                .get_mut(callee)
                .expect("callee is a known node")
                .insert(caller.clone());
        }
    }

    let effect_roots_of =
        |id: &str| -> Vec<String> { effect_roots.get(id).cloned().unwrap_or_default() };

    // External package prefixes a node calls.
    let external_packages_of = |id: &str| -> Vec<String> {
        let mut pkgs: BTreeSet<String> = BTreeSet::new();
        if let Some(deps) = full_deps.get(id) {
            for callee in deps {
                if !is_internal(callee) {
                    pkgs.insert(package_of(callee));
                }
            }
        }
        pkgs.into_iter().collect()
    };

    // Symbol SCCs / port order.
    let sym_condensation = condense(&int_deps);

    // Symbol nodes (sorted by id — `nodes` is a BTreeMap).
    let mut symbols: Vec<SymbolNode> = Vec::with_capacity(nodes.len());
    for (id, meta) in nodes {
        let external = meta.is_none();
        let internal_dep_set = int_deps.get(id);
        let is_leaf = internal_dep_set.map(|s| s.is_empty()).unwrap_or(true);
        let (file, language_field, line_start, line_end, loc, display) = match meta {
            Some(m) => (
                Some(m.file.clone()),
                Some(m.language.clone()),
                Some(m.line_start),
                Some(m.line_end),
                Some(m.line_end.saturating_sub(m.line_start) + 1),
                m.display.clone(),
            ),
            None => (None, None, None, None, None, id.clone()),
        };
        symbols.push(SymbolNode {
            id: id.clone(),
            display,
            file,
            language: language_field,
            line_start,
            line_end,
            loc,
            external,
            fan_in: callers_of.get(id).map(|s| s.len()).unwrap_or(0),
            fan_out: full_deps.get(id).map(|s| s.len()).unwrap_or(0),
            transitive_dep_count: transitive_count(id, &full_deps),
            is_leaf,
            effect_roots: if external {
                Vec::new()
            } else {
                effect_roots_of(id)
            },
            external_packages: external_packages_of(id),
            scc: sym_condensation.group_of.get(id).cloned(),
        });
    }

    // Unknown-edge count, straight off the tagged provenance.
    let unknown_edge_count = edges.iter().filter(|e| e.provenance == "unknown").count();

    // File rollup. Only local definitions have files.
    let file_of: BTreeMap<&str, &str> = nodes
        .iter()
        .filter_map(|(id, m)| m.as_ref().map(|meta| (id.as_str(), meta.file.as_str())))
        .collect();
    let files_set: BTreeSet<String> = file_of.values().map(|f| f.to_string()).collect();

    let mut file_deps: BTreeMap<String, BTreeSet<String>> = files_set
        .iter()
        .map(|f| (f.clone(), BTreeSet::new()))
        .collect();
    let mut file_edge_agg: BTreeMap<(String, String), (usize, bool)> = BTreeMap::new();
    for edge in &edges {
        let (Some(from_file), Some(to_file)) = (
            file_of.get(edge.from.as_str()),
            file_of.get(edge.to.as_str()),
        ) else {
            continue;
        };
        if from_file == to_file {
            continue;
        }
        file_deps
            .get_mut(*from_file)
            .expect("caller file is known")
            .insert(to_file.to_string());
        let entry = file_edge_agg
            .entry((from_file.to_string(), to_file.to_string()))
            .or_insert((0, false));
        entry.0 += 1;
        entry.1 = entry.1 || edge.resolution == "unresolved";
    }

    let mut file_callers: BTreeMap<String, BTreeSet<String>> = files_set
        .iter()
        .map(|f| (f.clone(), BTreeSet::new()))
        .collect();
    for (from_file, deps) in &file_deps {
        for to_file in deps {
            file_callers
                .get_mut(to_file)
                .expect("to_file is known")
                .insert(from_file.clone());
        }
    }

    // Per-file symbol lists, for count/loc/effect/package rollups.
    let mut symbols_in_file: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (id, meta) in nodes {
        if let Some(m) = meta {
            symbols_in_file
                .entry(m.file.as_str())
                .or_default()
                .push(id.as_str());
        }
    }

    let file_condensation = condense(&file_deps);

    let mut files: Vec<FileNode> = Vec::with_capacity(files_set.len());
    for path in &files_set {
        let members: &[&str] = symbols_in_file
            .get(path.as_str())
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let loc: u32 = members
            .iter()
            .filter_map(|id| nodes.get(*id).and_then(|m| m.as_ref()))
            .map(|m| m.line_end.saturating_sub(m.line_start) + 1)
            .sum();
        let mut effect_roots_set: BTreeSet<String> = BTreeSet::new();
        let mut external_packages: BTreeSet<String> = BTreeSet::new();
        for id in members {
            effect_roots_set.extend(effect_roots_of(id));
            external_packages.extend(external_packages_of(id));
        }
        let is_leaf = file_deps.get(path).map(|s| s.is_empty()).unwrap_or(true);
        files.push(FileNode {
            path: path.clone(),
            symbol_count: members.len(),
            loc,
            fan_in: file_callers.get(path).map(|s| s.len()).unwrap_or(0),
            fan_out: file_deps.get(path).map(|s| s.len()).unwrap_or(0),
            transitive_dep_count: transitive_count(path, &file_deps),
            is_leaf,
            effect_roots: effect_roots_set.into_iter().collect(),
            external_packages: external_packages.into_iter().collect(),
            scc: file_condensation.group_of.get(path).cloned(),
        });
    }

    let mut file_edges: Vec<FileEdge> = file_edge_agg
        .into_iter()
        .map(|((from, to), (count, has_unknown))| FileEdge {
            from,
            to,
            call_edge_count: count,
            has_unknown,
        })
        .collect();
    file_edges
        .sort_by(|a, b| (a.from.as_str(), a.to.as_str()).cmp(&(b.from.as_str(), b.to.as_str())));

    // Leaves and topo orders.
    let symbol_leaves: Vec<String> = int_deps
        .iter()
        .filter(|(_, deps)| deps.is_empty())
        .map(|(id, _)| id.clone())
        .collect();
    let file_leaves: Vec<String> = file_deps
        .iter()
        .filter(|(_, deps)| deps.is_empty())
        .map(|(path, _)| path.clone())
        .collect();
    let symbol_topo_order: Vec<String> = sym_condensation.order.iter().flatten().cloned().collect();
    let file_topo_order: Vec<String> = file_condensation.order.iter().flatten().cloned().collect();

    let symbol_count = nodes.values().filter(|m| m.is_some()).count();
    let external_count = nodes.len() - symbol_count;

    let language = language
        .map(|l| l.to_string())
        .or_else(|| nodes.values().flatten().next().map(|m| m.language.clone()));

    let fidelity = Fidelity {
        call_only: true,
        notes: vec![
            "Edges come from the call/use graph (call-hierarchy style): `from` \
             depends on `to` when it calls or references it."
                .to_string(),
            "Call-only fidelity — higher-order calls, dynamic dispatch through \
             trait objects or function pointers, and unresolved callbacks are \
             approximated or missed. An edge the adapter could not resolve is \
             marked provenance=\"unknown\"."
                .to_string(),
            "There is no textDocument/implementation or explicit imports table; \
             file edges are inferred by projecting symbol call edges onto their \
             files, so a file dependency that flows only through types or \
             imports (never a call) is not represented."
                .to_string(),
            "External callees (no local definition) are emitted as leaf nodes \
             with provenance external/unknown; treat them as already-available \
             library calls, not port targets."
                .to_string(),
        ],
        unknown_edge_count,
        external_node_count: external_count,
    };

    let stats = Stats {
        symbol_count,
        external_count,
        file_count: files.len(),
        edge_count: edges.len(),
        file_edge_count: file_edges.len(),
        scc_count: sym_condensation.groups.len(),
    };

    GraphOutput {
        hinzu_graph_version: HINZU_GRAPH_VERSION,
        root: root.to_string(),
        language,
        fidelity,
        stats,
        symbols,
        edges,
        files,
        file_edges,
        condensation: Condensation {
            symbol_topo_order,
            file_topo_order,
            symbol_sccs: sym_condensation.groups,
            file_sccs: file_condensation.groups,
            symbol_leaves,
            file_leaves,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{make_def, Definition, Edge, Effect, EffectRoot};

    fn def(id: &str, file: &str, line_start: u32, line_end: u32) -> Definition {
        make_def(id, file, line_start, line_end)
    }

    fn node<'a>(out: &'a GraphOutput, id: &str) -> &'a SymbolNode {
        out.symbols
            .iter()
            .find(|s| s.id == id)
            .expect("node present")
    }

    /// Three defs `a`/`b`/`c` in their own files, wired `a -> b`. The two
    /// ordering tests share this base and each adds the edge(s) that make it a
    /// chain or a cycle.
    fn abc_facts() -> FactSet {
        let mut facts = FactSet::default();
        facts.add_def(def("a", "a.rs", 1, 3));
        facts.add_def(def("b", "b.rs", 1, 3));
        facts.add_def(def("c", "c.rs", 1, 3));
        facts.add_edge(Edge::call("a", "b", "a.rs", 2));
        facts
    }

    #[test]
    fn simple_chain_orders_dependencies_first() {
        // a -> b -> c
        let mut facts = abc_facts();
        facts.add_edge(Edge::call("b", "c", "b.rs", 2));

        let out = build_graph(&facts, "chain", Some("rust"));

        // Dependencies first: c before b before a.
        assert_eq!(
            out.condensation.symbol_topo_order,
            vec!["c".to_string(), "b".to_string(), "a".to_string()]
        );
        // Only c is a leaf (no internal deps).
        assert_eq!(out.condensation.symbol_leaves, vec!["c".to_string()]);
        // a transitively depends on b and c.
        assert_eq!(node(&out, "a").transitive_dep_count, 2);
        assert_eq!(node(&out, "c").transitive_dep_count, 0);
        assert!(node(&out, "c").is_leaf);
        assert!(!node(&out, "a").is_leaf);
        assert_eq!(out.stats.symbol_count, 3);
        assert_eq!(out.stats.external_count, 0);
        assert_eq!(out.stats.scc_count, 0);
    }

    #[test]
    fn cycle_is_condensed_and_ordered_before_its_dependent() {
        // a <-> b, and c -> a. The SCC {a,b} must be ported before c.
        let mut facts = abc_facts();
        facts.add_edge(Edge::call("b", "a", "b.rs", 2));
        facts.add_edge(Edge::call("c", "a", "c.rs", 2));

        let out = build_graph(&facts, "cycle", Some("rust"));

        // One non-trivial SCC {a,b} is reported and shared by both members.
        assert_eq!(out.condensation.symbol_sccs.len(), 1);
        assert_eq!(out.stats.scc_count, 1);
        let scc = &out.condensation.symbol_sccs[0];
        assert_eq!(scc.members, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(node(&out, "a").scc.as_deref(), Some("scc:0"));
        assert_eq!(node(&out, "b").scc.as_deref(), Some("scc:0"));
        assert_eq!(node(&out, "c").scc, None);

        // The SCC members are contiguous and precede c in the port order.
        let pos = |id: &str| {
            out.condensation
                .symbol_topo_order
                .iter()
                .position(|x| x == id)
                .unwrap()
        };
        assert!(pos("a") < pos("c"));
        assert!(pos("b") < pos("c"));
        assert_eq!(pos("a").abs_diff(pos("b")), 1); // adjacent
    }

    #[test]
    fn external_callee_is_a_leaf_node_not_an_internal_symbol() {
        // local `run` calls an external `pkg::foo` with a seeded effect, and an
        // unresolved indirect target.
        let mut facts = FactSet::default();
        facts.add_def(def("app::run", "run.rs", 1, 5));
        facts.add_edge(Edge::call("app::run", "pkg::foo", "run.rs", 2));
        facts.add_root(EffectRoot {
            symbol: "pkg::foo".to_string(),
            effect: Effect::Net,
        });

        let out = build_graph(&facts, "ext", Some("rust"));

        // The external target is a node, marked external, always a leaf.
        let foo = node(&out, "pkg::foo");
        assert!(foo.external);
        assert!(foo.is_leaf);
        assert!(foo.file.is_none());
        assert!(foo.effect_roots.is_empty());
        // It is not counted among internal symbols, and not in the port order.
        assert_eq!(out.stats.symbol_count, 1);
        assert_eq!(out.stats.external_count, 1);
        assert!(!out
            .condensation
            .symbol_topo_order
            .contains(&"pkg::foo".to_string()));
        // `run` still counts it as an external package it depends on, and the
        // seeded effect propagates to `run`'s reachable effects.
        assert_eq!(
            node(&out, "app::run").external_packages,
            vec!["pkg".to_string()]
        );
        assert_eq!(node(&out, "app::run").effect_roots, vec!["net".to_string()]);
        assert!(node(&out, "app::run").is_leaf); // no *internal* deps

        // The edge to the seeded external is provenance "external".
        let edge = out.edges.iter().find(|e| e.to == "pkg::foo").unwrap();
        assert_eq!(edge.provenance, "external");
    }

    #[test]
    fn unresolved_edge_is_unknown_provenance() {
        let mut facts = FactSet::default();
        facts.add_def(def("app::dispatch", "d.rs", 1, 3));
        facts.add_edge(Edge {
            caller: "app::dispatch".to_string(),
            callee: "<indirect>".to_string(),
            kind: crate::facts::EdgeKind::Call,
            resolution: EdgeResolution::Unresolved,
            evidence_file: "d.rs".to_string(),
            evidence_line: 2,
        });
        let out = build_graph(&facts, "u", Some("rust"));
        let edge = out.edges.iter().find(|e| e.to == "<indirect>").unwrap();
        assert_eq!(edge.provenance, "unknown");
        assert_eq!(out.fidelity.unknown_edge_count, 1);
    }

    #[test]
    fn file_rollup_projects_cross_file_calls() {
        // two files, a call from a.rs into b.rs.
        let mut facts = FactSet::default();
        facts.add_def(def("a::top", "a.rs", 1, 4));
        facts.add_def(def("b::leaf", "b.rs", 1, 4));
        facts.add_edge(Edge::call("a::top", "b::leaf", "a.rs", 2));

        let out = build_graph(&facts, "files", Some("rust"));

        assert_eq!(out.stats.file_count, 2);
        assert_eq!(out.file_edges.len(), 1);
        let fe = &out.file_edges[0];
        assert_eq!(fe.from, "a.rs");
        assert_eq!(fe.to, "b.rs");
        assert_eq!(fe.call_edge_count, 1);
        assert!(!fe.has_unknown);
        // b.rs is the file leaf, ordered first.
        assert_eq!(out.condensation.file_leaves, vec!["b.rs".to_string()]);
        assert_eq!(
            out.condensation.file_topo_order,
            vec!["b.rs".to_string(), "a.rs".to_string()]
        );
    }

    /// The dependency closure of `roots`, as a sorted Vec for easy assertions.
    fn closure_of(out: &GraphOutput, roots: &[&str]) -> Vec<String> {
        let roots: Vec<String> = roots.iter().map(|s| s.to_string()).collect();
        dependency_closure(out, &roots).into_iter().collect()
    }

    #[test]
    fn closure_follows_dependency_edges_down_a_chain() {
        // a -> b -> c (a depends on b depends on c).
        let mut facts = abc_facts();
        facts.add_edge(Edge::call("b", "c", "b.rs", 2));
        let out = build_graph(&facts, "chain", Some("rust"));

        // b needs c; a is *upstream* of b and is not pulled in.
        assert_eq!(
            closure_of(&out, &["b"]),
            vec!["b".to_string(), "c".to_string()]
        );
        // A root with no outgoing edges is just itself.
        assert_eq!(closure_of(&out, &["c"]), vec!["c".to_string()]);

        // The filtered sub-graph is scoped to {b, c}, c the leaf, and stays
        // plan-able — build_plan yields a plan over exactly those two files.
        let scoped = filter_to_closure(&out, &["b".to_string()]);
        assert_eq!(scoped.stats.symbol_count, 2);
        assert!(scoped.symbols.iter().all(|s| s.id == "b" || s.id == "c"));
        assert_eq!(
            scoped.condensation.symbol_topo_order,
            vec!["c".to_string(), "b".to_string()]
        );
        assert_eq!(scoped.condensation.symbol_leaves, vec!["c".to_string()]);

        let plan = crate::plan::build_plan(
            &scoped,
            crate::plan::PlanOpts {
                max_group_loc: 200,
                coalesce_small: false,
            },
        );
        assert_eq!(plan.stats.file_count, 2);
        assert!(plan
            .groups
            .iter()
            .all(|g| g.files.iter().all(|f| f == "b.rs" || f == "c.rs")));
        let group_with = |file: &str| {
            plan.groups
                .iter()
                .find(|g| g.files.iter().any(|f| f == file))
                .expect("group holds the file")
        };
        assert_eq!(group_with("c.rs").wave, 0); // c is the leaf / wave 0
        assert_eq!(group_with("b.rs").wave, 1);
    }

    #[test]
    fn closure_over_a_diamond_keeps_both_sides() {
        // a -> b, a -> c, b -> d, c -> d.
        let mut facts = abc_facts(); // already has a -> b
        facts.add_def(def("d", "d.rs", 1, 3));
        facts.add_edge(Edge::call("a", "c", "a.rs", 3));
        facts.add_edge(Edge::call("b", "d", "b.rs", 2));
        facts.add_edge(Edge::call("c", "d", "c.rs", 2));
        let out = build_graph(&facts, "diamond", Some("rust"));

        // One arm of the diamond: b needs only d.
        assert_eq!(
            closure_of(&out, &["b"]),
            vec!["b".to_string(), "d".to_string()]
        );
        // The apex pulls in the whole diamond.
        assert_eq!(
            closure_of(&out, &["a"]),
            vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string()
            ]
        );
    }

    #[test]
    fn closure_includes_external_callee_as_a_leaf() {
        // local `run` calls an external `pkg::foo` — an external node has no
        // definition and is always a leaf, but it belongs in the closure as the
        // assumed-available dependency the root needs.
        let mut facts = FactSet::default();
        facts.add_def(def("app::run", "run.rs", 1, 5));
        facts.add_edge(Edge::call("app::run", "pkg::foo", "run.rs", 2));
        let out = build_graph(&facts, "ext", Some("rust"));

        assert_eq!(
            closure_of(&out, &["app::run"]),
            vec!["app::run".to_string(), "pkg::foo".to_string()]
        );
        let scoped = filter_to_closure(&out, &["app::run".to_string()]);
        let foo = node(&scoped, "pkg::foo");
        assert!(foo.external);
        assert!(foo.is_leaf);
        assert_eq!(scoped.stats.symbol_count, 1);
        assert_eq!(scoped.stats.external_count, 1);
        // `run` still records the external package it depends on.
        assert_eq!(
            node(&scoped, "app::run").external_packages,
            vec!["pkg".to_string()]
        );
    }

    #[test]
    fn resolve_roots_by_id_name_file_and_error() {
        let mut facts = FactSet::default();
        facts.add_def(def("app::run", "run.rs", 1, 5));
        facts.add_def(def("app::helper", "run.rs", 6, 9));
        facts.add_def(def("other::thing", "other.rs", 1, 3));
        let out = build_graph(&facts, "resolve", Some("rust"));

        // Exact symbol id.
        assert_eq!(
            resolve_roots(&out, &["app::run".to_string()])
                .unwrap()
                .roots,
            vec!["app::run".to_string()]
        );
        // Leaf / display name.
        assert_eq!(
            resolve_roots(&out, &["run".to_string()]).unwrap().roots,
            vec!["app::run".to_string()]
        );
        // File expansion: run.rs defines two symbols, both become roots, and the
        // multi-symbol match is reported.
        let r = resolve_roots(&out, &["run.rs".to_string()]).unwrap();
        assert_eq!(
            r.roots,
            vec!["app::helper".to_string(), "app::run".to_string()]
        );
        assert!(!r.notes.is_empty());
        // Not found: an error naming near-misses, never a silent empty plan.
        let err = resolve_roots(&out, &["nope".to_string()]).unwrap_err();
        assert!(err.contains("matched no symbol or file"), "got: {err}");
    }
}
