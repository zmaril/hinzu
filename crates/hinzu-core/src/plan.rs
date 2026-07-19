//! The porting plan: a file-granularity schedule of **waves** and **groups**
//! over the dependency graph that [`crate::graph`] already computed. Where the
//! graph answers "in what order can a symbol be ported so its dependencies come
//! first?", the plan answers the operational question a porting *orchestrator*
//! asks: **"which files can I hand to parallel threads/PRs right now, and what
//! does finishing them unlock?"**
//!
//! Porting happens file-by-file (a PR per group), so the plan works over the
//! graph's file rollup, never re-walking the raw facts — [`build_plan`] takes the
//! already-built [`GraphOutput`] and reuses its file nodes and file edges.
//!
//! - A **group** is a set of files ported together as one unit (one PR / one
//!   agent thread). Files in the same file-level dependency cycle *must* share a
//!   group (`reason = "cycle"`); optional small-file coalescing (`reason =
//!   "coalesced-small"`) merges tiny adjacent groups so an orchestrator isn't
//!   left spinning up a thread per one-liner; everything else is a `"singleton"`.
//! - A **wave** is a topological *layer* over the group-condensation DAG:
//!   `wave = 0` for groups that depend on nothing, else `1 + max(wave of deps)`.
//!   Two groups in the same wave never depend on each other, so a whole wave can
//!   be ported in parallel, and each wave is exactly "what the previous waves
//!   unlocked."
//!
//! ## Fidelity
//!
//! The plan inherits every caveat of the graph it is built from — the graph is
//! call-only, file edges are inferred from call edges (no imports/implementation
//! table), and unresolved targets are marked rather than dropped. On top of that,
//! grouping and wave assignment are only as good as those edges, and small-file
//! coalescing is a size heuristic, not a correctness requirement. These notes are
//! carried in [`PlanOutput::fidelity`] next to the data.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::graph::GraphOutput;

/// The schema version embedded in every emitted plan.
pub const HINZU_PLAN_VERSION: u32 = 1;

/// Knobs for [`build_plan`].
#[derive(Clone, Copy, Debug)]
pub struct PlanOpts {
    /// The loc ceiling a coalesced group is kept under. A group at or above this
    /// size is never grown by coalescing, and two groups are never merged if the
    /// union would reach it.
    pub max_group_loc: usize,
    /// Whether to greedily merge small adjacent groups (`"coalesced-small"`). When
    /// off, grouping is cycle-SCCs-plus-singletons only.
    pub coalesce_small: bool,
}

impl Default for PlanOpts {
    fn default() -> Self {
        PlanOpts {
            max_group_loc: 200,
            coalesce_small: true,
        }
    }
}

/// A group of files ported together as one unit (a PR / an agent thread).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanGroup {
    /// The group id (`"group:N"`, numbered in wave-then-path order).
    pub id: String,
    /// Why these files are one group: `"cycle"` (a mandatory file-cycle SCC),
    /// `"coalesced-small"` (a heuristic merge of small adjacent groups), or
    /// `"singleton"` (a lone file).
    pub reason: String,
    /// The member file paths, sorted.
    pub files: Vec<String>,
    /// Total lines of code across the member files.
    pub loc: u32,
    /// Total local symbol definitions across the member files.
    pub symbol_count: usize,
    /// The topological wave this group lands in (`0` = ported first).
    pub wave: u32,
    /// The group ids this group depends on — all in strictly earlier waves.
    pub depends_on: Vec<String>,
    /// The group ids that depend on this one — all in strictly later waves.
    pub unlocks: Vec<String>,
    /// Union of the member files' external package prefixes, sorted.
    pub external_packages: Vec<String>,
    /// Union of the member files' reachable effect categories, sorted.
    pub effect_roots: Vec<String>,
    /// Whether any file dependency edge incident to this group is unresolved
    /// (`has_unknown` on the underlying file edge) — a place the plan is guessing.
    pub has_unknown_edges: bool,
}

/// A topological layer of groups that can be ported fully in parallel.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Wave {
    /// The wave number (`0` = first).
    pub wave: u32,
    /// The ids of the groups in this wave, in numeric id order.
    pub group_ids: Vec<String>,
    /// Every member file of every group in this wave, flattened and sorted.
    pub files: Vec<String>,
    /// Total loc across the wave.
    pub loc: u32,
    /// How many groups are in this wave.
    pub group_count: usize,
}

/// The grouping knobs echoed back into the output.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Grouping {
    /// The loc ceiling coalescing respected.
    pub max_group_loc: usize,
    /// Whether small-file coalescing ran.
    pub coalesce_small: bool,
}

/// The call-only fidelity of the plan, carried next to the data.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Fidelity {
    /// Always true: the plan is built over the call-only dependency graph.
    pub call_only: bool,
    /// Human-readable caveats about what the plan does and does not capture.
    pub notes: Vec<String>,
}

/// Aggregate counts for the whole plan.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stats {
    /// Distinct files scheduled.
    pub file_count: usize,
    /// Total groups.
    pub group_count: usize,
    /// Total waves (`0..wave_count`).
    pub wave_count: usize,
    /// Groups formed because their files are in a dependency cycle.
    pub cycle_group_count: usize,
    /// Groups formed by small-file coalescing.
    pub coalesced_group_count: usize,
    /// Lone-file groups.
    pub singleton_group_count: usize,
    /// The most groups in any one wave — the peak parallelism a plan offers.
    pub largest_wave: usize,
    /// The longest dependency chain, in waves — the minimum number of sequential
    /// porting rounds (equals `wave_count`).
    pub critical_path_length: usize,
}

/// The complete plan document, ready to serialize as JSON.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanOutput {
    /// The schema version ([`HINZU_PLAN_VERSION`]).
    pub hinzu_plan_version: u32,
    /// The analyzed target (carried over from the graph).
    pub root: String,
    /// The dominant source language, if one was determined.
    pub language: Option<String>,
    /// The porting granularity — always `"file"` for this plan.
    pub granularity: String,
    /// The grouping knobs used.
    pub grouping: Grouping,
    /// The call-only fidelity caveats.
    pub fidelity: Fidelity,
    /// Aggregate counts.
    pub stats: Stats,
    /// The groups, in numeric id order.
    pub groups: Vec<PlanGroup>,
    /// The waves, `0..wave_count`.
    pub waves: Vec<Wave>,
}

/// A group under construction: its member files and whether it carries a cycle.
#[derive(Clone)]
struct WorkGroup {
    files: BTreeSet<String>,
    /// True once the group contains (or absorbs) a file-level dependency cycle;
    /// this dominates the reported reason.
    is_cycle: bool,
}

/// The smallest member path of a group — the stable key groups are ordered by.
fn min_path(files: &BTreeSet<String>) -> &str {
    files.iter().next().map(|s| s.as_str()).unwrap_or("")
}

/// Project the file dependency adjacency onto groups: `group_deps[g]` is the set
/// of groups `g` depends on (an edge `from -> to` where `from`/`to` land in
/// different groups). Only keys present in `groups` appear.
fn group_deps(
    groups: &BTreeMap<usize, WorkGroup>,
    group_of: &BTreeMap<String, usize>,
    file_deps: &[(String, String)],
) -> BTreeMap<usize, BTreeSet<usize>> {
    let mut deps: BTreeMap<usize, BTreeSet<usize>> =
        groups.keys().map(|&g| (g, BTreeSet::new())).collect();
    for (from, to) in file_deps {
        let (Some(&gf), Some(&gt)) = (group_of.get(from), group_of.get(to)) else {
            continue;
        };
        if gf != gt {
            deps.get_mut(&gf).expect("from group is known").insert(gt);
        }
    }
    deps
}

/// Longest-path (ASAP) wave layering over a group-condensation DAG: `wave(g) = 0`
/// when `g` depends on nothing, else `1 + max(wave(dep))`. Returns the wave per
/// group, or `None` if the graph has a cycle (some group is never resolved) —
/// which is exactly the safety check coalescing needs before it merges.
fn topo_waves(deps: &BTreeMap<usize, BTreeSet<usize>>) -> Option<BTreeMap<usize, u32>> {
    // Kahn over the "depends on" relation: a group is ready once every group it
    // depends on has a wave. `pending` counts unresolved dependencies.
    let mut pending: BTreeMap<usize, usize> = deps.iter().map(|(&g, d)| (g, d.len())).collect();
    // Reverse: dependents[d] = groups that depend on d, so resolving d can ready
    // them.
    let mut dependents: BTreeMap<usize, Vec<usize>> =
        deps.keys().map(|&g| (g, Vec::new())).collect();
    for (&g, d) in deps {
        for &dep in d {
            dependents
                .get_mut(&dep)
                .expect("dep is a known group")
                .push(g);
        }
    }

    let mut wave: BTreeMap<usize, u32> = BTreeMap::new();
    let mut ready: Vec<usize> = pending
        .iter()
        .filter(|(_, &n)| n == 0)
        .map(|(&g, _)| g)
        .collect();
    while let Some(g) = ready.pop() {
        let w = deps[&g].iter().map(|d| wave[d] + 1).max().unwrap_or(0);
        wave.insert(g, w);
        for &dependent in &dependents[&g] {
            let p = pending.get_mut(&dependent).expect("dependent is known");
            *p -= 1;
            if *p == 0 {
                ready.push(dependent);
            }
        }
    }

    if wave.len() == deps.len() {
        Some(wave)
    } else {
        None // a cycle — some group never reached zero pending deps
    }
}

/// Whether merging groups `a` and `b` keeps the group-condensation a DAG. Merges
/// `b` into `a` in a scratch map and checks the result is still layerable.
fn merge_keeps_dag(
    groups: &BTreeMap<usize, WorkGroup>,
    group_of: &BTreeMap<String, usize>,
    file_deps: &[(String, String)],
    a: usize,
    b: usize,
) -> bool {
    let mut scratch: BTreeMap<String, usize> = group_of.clone();
    for f in &groups[&b].files {
        scratch.insert(f.clone(), a);
    }
    let mut merged_groups = groups.clone();
    let b_group = merged_groups.remove(&b).expect("b exists");
    merged_groups
        .get_mut(&a)
        .expect("a exists")
        .files
        .extend(b_group.files);
    topo_waves(&group_deps(&merged_groups, &scratch, file_deps)).is_some()
}

/// Total loc of a group's files, from the per-file loc table.
fn group_loc(g: &WorkGroup, file_loc: &BTreeMap<String, u32>) -> u32 {
    g.files
        .iter()
        .map(|f| file_loc.get(f).copied().unwrap_or(0))
        .sum()
}

/// Build a file-granularity porting plan from an already-built [`GraphOutput`].
///
/// Groups are formed from the graph's file-level dependency cycles (mandatory)
/// plus optional small-file coalescing, then laid out into topological waves. The
/// facts are never re-walked — everything comes from `graph`'s file rollup.
pub fn build_plan(graph: &GraphOutput, opts: PlanOpts) -> PlanOutput {
    // Per-file attributes, straight off the graph's file rollup.
    let file_loc: BTreeMap<String, u32> = graph
        .files
        .iter()
        .map(|f| (f.path.clone(), f.loc))
        .collect();
    let file_symcount: BTreeMap<String, usize> = graph
        .files
        .iter()
        .map(|f| (f.path.clone(), f.symbol_count))
        .collect();
    let file_pkgs: BTreeMap<String, Vec<String>> = graph
        .files
        .iter()
        .map(|f| (f.path.clone(), f.external_packages.clone()))
        .collect();
    let file_effects: BTreeMap<String, Vec<String>> = graph
        .files
        .iter()
        .map(|f| (f.path.clone(), f.effect_roots.clone()))
        .collect();

    // File dependency edges, `from` depends on `to` (see graph::FileEdge).
    let file_deps: Vec<(String, String)> = graph
        .file_edges
        .iter()
        .map(|e| (e.from.clone(), e.to.clone()))
        .collect();
    // Which file pairs carry an unresolved contributing edge.
    let unknown_pairs: BTreeSet<(String, String)> = graph
        .file_edges
        .iter()
        .filter(|e| e.has_unknown)
        .map(|e| (e.from.clone(), e.to.clone()))
        .collect();

    // Initial groups: each file-level SCC (a cycle) is one mandatory group; every
    // other file is its own singleton. `next_id` mints stable working indices.
    let mut groups: BTreeMap<usize, WorkGroup> = BTreeMap::new();
    let mut group_of: BTreeMap<String, usize> = BTreeMap::new();
    let mut next_id = 0usize;
    for scc in &graph.condensation.file_sccs {
        let files: BTreeSet<String> = scc.members.iter().cloned().collect();
        for f in &files {
            group_of.insert(f.clone(), next_id);
        }
        groups.insert(
            next_id,
            WorkGroup {
                files,
                is_cycle: true,
            },
        );
        next_id += 1;
    }
    for f in graph.files.iter().map(|f| &f.path) {
        if group_of.contains_key(f) {
            continue;
        }
        group_of.insert(f.clone(), next_id);
        groups.insert(
            next_id,
            WorkGroup {
                files: BTreeSet::from([f.clone()]),
                is_cycle: false,
            },
        );
        next_id += 1;
    }

    if opts.coalesce_small {
        coalesce(
            &mut groups,
            &mut group_of,
            &file_deps,
            &file_loc,
            opts.max_group_loc,
        );
    }

    // Final wave layering (the group graph is guaranteed acyclic here).
    let deps = group_deps(&groups, &group_of, &file_deps);
    let waves_of = topo_waves(&deps).expect("group condensation is a DAG");

    // Assign public ids by (wave, smallest path) so the numbering is stable and
    // reads top-down. `old_to_id` maps working index -> public numeric id.
    let mut ordered: Vec<usize> = groups.keys().copied().collect();
    ordered.sort_by(|&x, &y| {
        (waves_of[&x], min_path(&groups[&x].files))
            .cmp(&(waves_of[&y], min_path(&groups[&y].files)))
    });
    let old_to_id: BTreeMap<usize, usize> = ordered
        .iter()
        .enumerate()
        .map(|(id, &old)| (old, id))
        .collect();
    let id_str = |id: usize| format!("group:{id}");

    // Dependents (unlocks) as the reverse of deps.
    let mut dependents: BTreeMap<usize, BTreeSet<usize>> =
        groups.keys().map(|&g| (g, BTreeSet::new())).collect();
    for (&g, ds) in &deps {
        for &d in ds {
            dependents.get_mut(&d).expect("dep is known").insert(g);
        }
    }

    // Emit the groups in public-id order.
    let mut out_groups: Vec<PlanGroup> = Vec::with_capacity(ordered.len());
    for &old in &ordered {
        let wg = &groups[&old];
        let files: Vec<String> = wg.files.iter().cloned().collect();
        let loc: u32 = files
            .iter()
            .map(|f| file_loc.get(f).copied().unwrap_or(0))
            .sum();
        let symbol_count: usize = files
            .iter()
            .map(|f| file_symcount.get(f).copied().unwrap_or(0))
            .sum();
        let mut external_packages: BTreeSet<String> = BTreeSet::new();
        let mut effect_roots: BTreeSet<String> = BTreeSet::new();
        for f in &files {
            external_packages.extend(file_pkgs.get(f).cloned().unwrap_or_default());
            effect_roots.extend(file_effects.get(f).cloned().unwrap_or_default());
        }
        // Any dependency edge incident to a member file that is unresolved.
        let file_set: &BTreeSet<String> = &wg.files;
        let has_unknown_edges = unknown_pairs
            .iter()
            .any(|(from, to)| file_set.contains(from) || file_set.contains(to));
        let mut depends_on: Vec<String> =
            deps[&old].iter().map(|&d| id_str(old_to_id[&d])).collect();
        depends_on.sort_by_key(|s| numeric_id(s));
        let mut unlocks: Vec<String> = dependents[&old]
            .iter()
            .map(|&d| id_str(old_to_id[&d]))
            .collect();
        unlocks.sort_by_key(|s| numeric_id(s));
        let reason = if wg.is_cycle {
            "cycle"
        } else if wg.files.len() > 1 {
            "coalesced-small"
        } else {
            "singleton"
        };
        out_groups.push(PlanGroup {
            id: id_str(old_to_id[&old]),
            reason: reason.to_string(),
            files,
            loc,
            symbol_count,
            wave: waves_of[&old],
            depends_on,
            unlocks,
            external_packages: external_packages.into_iter().collect(),
            effect_roots: effect_roots.into_iter().collect(),
            has_unknown_edges,
        });
    }

    // Roll the groups up into waves.
    let wave_count = out_groups.iter().map(|g| g.wave + 1).max().unwrap_or(0) as usize;
    let mut waves: Vec<Wave> = (0..wave_count as u32)
        .map(|w| Wave {
            wave: w,
            group_ids: Vec::new(),
            files: Vec::new(),
            loc: 0,
            group_count: 0,
        })
        .collect();
    for g in &out_groups {
        let w = &mut waves[g.wave as usize];
        w.group_ids.push(g.id.clone());
        w.files.extend(g.files.iter().cloned());
        w.loc += g.loc;
        w.group_count += 1;
    }
    for w in &mut waves {
        w.group_ids.sort_by_key(|s| numeric_id(s));
        w.files.sort();
    }

    let cycle_group_count = out_groups.iter().filter(|g| g.reason == "cycle").count();
    let coalesced_group_count = out_groups
        .iter()
        .filter(|g| g.reason == "coalesced-small")
        .count();
    let singleton_group_count = out_groups
        .iter()
        .filter(|g| g.reason == "singleton")
        .count();
    let largest_wave = waves.iter().map(|w| w.group_count).max().unwrap_or(0);

    let stats = Stats {
        file_count: graph.files.len(),
        group_count: out_groups.len(),
        wave_count,
        cycle_group_count,
        coalesced_group_count,
        singleton_group_count,
        largest_wave,
        critical_path_length: wave_count,
    };

    PlanOutput {
        hinzu_plan_version: HINZU_PLAN_VERSION,
        root: graph.root.clone(),
        language: graph.language.clone(),
        granularity: "file".to_string(),
        grouping: Grouping {
            max_group_loc: opts.max_group_loc,
            coalesce_small: opts.coalesce_small,
        },
        fidelity: Fidelity {
            call_only: graph.fidelity.call_only,
            notes: fidelity_notes(graph.fidelity.includes_type_edges),
        },
        stats,
        groups: out_groups,
        waves,
    }
}

/// The numeric suffix of a `"group:N"` id, for numeric ordering of id lists.
fn numeric_id(id: &str) -> usize {
    id.rsplit(':')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Greedily merge small groups (loc under `max_loc`) with an adjacent group when
/// the union stays under `max_loc` and the merge keeps the condensation a DAG.
/// Independent (non-adjacent) small groups are left alone — they will share a
/// wave anyway, and an orchestrator can batch them freely. Terminates: every
/// merge reduces the group count.
fn coalesce(
    groups: &mut BTreeMap<usize, WorkGroup>,
    group_of: &mut BTreeMap<String, usize>,
    file_deps: &[(String, String)],
    file_loc: &BTreeMap<String, u32>,
    max_loc: usize,
) {
    loop {
        let deps = group_deps(groups, group_of, file_deps);
        // Neighbors of each group: the groups it depends on plus the groups that
        // depend on it (adjacency is undirected for "can I merge with it?").
        let mut neighbors: BTreeMap<usize, BTreeSet<usize>> =
            groups.keys().map(|&g| (g, BTreeSet::new())).collect();
        for (&g, ds) in &deps {
            for &d in ds {
                neighbors.get_mut(&g).expect("g known").insert(d);
                neighbors.get_mut(&d).expect("d known").insert(g);
            }
        }

        // Candidate initiators: small groups, in stable (min-path) order.
        let mut candidates: Vec<usize> = groups
            .iter()
            .filter(|(_, g)| (group_loc(g, file_loc) as usize) < max_loc)
            .map(|(&id, _)| id)
            .collect();
        candidates.sort_by(|&x, &y| min_path(&groups[&x].files).cmp(min_path(&groups[&y].files)));

        let mut chosen: Option<(usize, usize)> = None;
        'outer: for &g in &candidates {
            let mut ns: Vec<usize> = neighbors[&g].iter().copied().collect();
            ns.sort_by(|&x, &y| min_path(&groups[&x].files).cmp(min_path(&groups[&y].files)));
            for &h in &ns {
                let merged_loc = group_loc(&groups[&g], file_loc) as usize
                    + group_loc(&groups[&h], file_loc) as usize;
                if merged_loc >= max_loc {
                    continue;
                }
                if merge_keeps_dag(groups, group_of, file_deps, g, h) {
                    chosen = Some((g, h));
                    break 'outer;
                }
            }
        }

        let Some((a, b)) = chosen else { break };
        // Merge b into a: retag b's files, fold in its cycle flag, drop b.
        let b_group = groups.remove(&b).expect("b exists");
        for f in &b_group.files {
            group_of.insert(f.clone(), a);
        }
        let ag = groups.get_mut(&a).expect("a exists");
        ag.files.extend(b_group.files);
        ag.is_cycle = ag.is_cycle || b_group.is_cycle;
    }
}

/// The plan's honest caveats — the graph's dependency-edge limits, plus the
/// grouping and coalescing heuristics layered on top. `includes_type_edges`
/// reflects whether the underlying graph captured signature-type dependencies.
fn fidelity_notes(includes_type_edges: bool) -> Vec<String> {
    let dependency_note = if includes_type_edges {
        "Built over the dependency graph: an edge means a caller calls or references \
         a callee, or (kind=\"type\") depends on a type in its signature. Signature-type \
         dependencies are captured for TypeScript and Rust, so file dependencies that flow \
         only through a type are represented; the LSP/tree-sitter (Python) rung is still \
         call-only — a follow-up."
            .to_string()
    } else {
        "Built over the call-only dependency graph: an edge means a caller calls or \
         references a callee, so file dependencies that flow only through types or \
         imports (never a call) are not represented."
            .to_string()
    };
    vec![
        dependency_note,
        "File edges are inferred by projecting symbol edges onto their files; \
         there is no imports/implementation table."
            .to_string(),
        "Higher-order calls, dynamic dispatch, and unresolved callbacks are \
         approximated or missed; an edge the adapter could not resolve sets \
         has_unknown_edges on the groups it touches."
            .to_string(),
        "Grouping and wave assignment inherit those limits: a missed edge can \
         wrongly split a wave or under-constrain the order."
            .to_string(),
        "Cycle groups are mandatory (a file dependency cycle must be ported \
         together), but small-file coalescing is a size heuristic tuned by \
         max_group_loc; independent small files are not force-merged and simply \
         share a wave."
            .to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{make_def, Definition, Edge, FactSet};
    use crate::graph::build_graph;

    fn fdef(id: &str, file: &str, line_end: u32) -> Definition {
        make_def(id, file, 1, line_end)
    }

    /// Three defs `a`/`b`/`c`, each in its own file. The layering tests share
    /// this base and each adds the edges that shape it into a chain, a diamond,
    /// or a cycle.
    fn abc_facts() -> FactSet {
        let mut facts = FactSet::default();
        facts.add_def(fdef("a", "a.rs", 3));
        facts.add_def(fdef("b", "b.rs", 3));
        facts.add_def(fdef("c", "c.rs", 3));
        facts
    }

    /// Build a plan from a fact set, going through the real graph builder so the
    /// plan is exercised over genuine file rollups.
    fn plan_of(facts: &FactSet, opts: PlanOpts) -> PlanOutput {
        let graph = build_graph(facts, "test", Some("rust"));
        build_plan(&graph, opts)
    }

    fn group_with_file<'a>(plan: &'a PlanOutput, file: &str) -> &'a PlanGroup {
        plan.groups
            .iter()
            .find(|g| g.files.iter().any(|f| f == file))
            .expect("a group holds the file")
    }

    #[test]
    fn chain_layers_leaf_first() {
        // a.rs -> b.rs -> c.rs (a depends on b depends on c).
        let mut facts = abc_facts();
        facts.add_edge(Edge::call("a", "b", "a.rs", 2));
        facts.add_edge(Edge::call("b", "c", "b.rs", 2));

        // Coalescing off: the pure leaf-first layering, one singleton per file.
        let plan = plan_of(
            &facts,
            PlanOpts {
                max_group_loc: 200,
                coalesce_small: false,
            },
        );

        assert_eq!(plan.stats.group_count, 3);
        assert_eq!(plan.stats.singleton_group_count, 3);
        assert_eq!(plan.stats.wave_count, 3);
        assert_eq!(plan.stats.critical_path_length, 3);
        assert_eq!(plan.stats.largest_wave, 1);
        // c is the leaf: wave 0.
        assert_eq!(group_with_file(&plan, "c.rs").wave, 0);
        assert_eq!(group_with_file(&plan, "b.rs").wave, 1);
        assert_eq!(group_with_file(&plan, "a.rs").wave, 2);
        // One group per wave, in leaf-first order.
        let wave_files: Vec<&str> = plan.waves.iter().map(|w| w.files[0].as_str()).collect();
        assert_eq!(wave_files, vec!["c.rs", "b.rs", "a.rs"]);
        // depends_on / unlocks thread the chain.
        let a = group_with_file(&plan, "a.rs");
        assert_eq!(
            a.depends_on,
            vec![group_with_file(&plan, "b.rs").id.clone()]
        );
        let c = group_with_file(&plan, "c.rs");
        assert_eq!(c.unlocks, vec![group_with_file(&plan, "b.rs").id.clone()]);
    }

    #[test]
    fn diamond_puts_independent_middle_in_one_wave() {
        // a depends on b and c; b and c depend on d.
        let mut facts = abc_facts();
        facts.add_def(fdef("d", "d.rs", 3));
        facts.add_edge(Edge::call("a", "b", "a.rs", 2));
        facts.add_edge(Edge::call("a", "c", "a.rs", 3));
        facts.add_edge(Edge::call("b", "d", "b.rs", 2));
        facts.add_edge(Edge::call("c", "d", "c.rs", 2));

        // Coalescing off, so the diamond shape is what we're testing.
        let plan = plan_of(
            &facts,
            PlanOpts {
                max_group_loc: 200,
                coalesce_small: false,
            },
        );

        assert_eq!(plan.stats.wave_count, 3);
        assert_eq!(group_with_file(&plan, "d.rs").wave, 0);
        assert_eq!(group_with_file(&plan, "b.rs").wave, 1);
        assert_eq!(group_with_file(&plan, "c.rs").wave, 1);
        assert_eq!(group_with_file(&plan, "a.rs").wave, 2);
        // b and c share the middle wave (they do not depend on each other).
        assert_eq!(plan.waves[1].group_count, 2);
        assert_eq!(plan.stats.largest_wave, 2);
    }

    #[test]
    fn file_cycle_is_one_mandatory_group() {
        // b.rs <-> c.rs (cycle), and a.rs depends on the cycle.
        let mut facts = abc_facts();
        facts.add_edge(Edge::call("a", "b", "a.rs", 2));
        facts.add_edge(Edge::call("b", "c", "b.rs", 2));
        facts.add_edge(Edge::call("c", "b", "c.rs", 2));

        // Coalescing off, so the only grouping is the mandatory cycle.
        let plan = plan_of(
            &facts,
            PlanOpts {
                max_group_loc: 200,
                coalesce_small: false,
            },
        );

        let cycle = group_with_file(&plan, "b.rs");
        assert_eq!(cycle.reason, "cycle");
        assert_eq!(cycle.files, vec!["b.rs".to_string(), "c.rs".to_string()]);
        assert_eq!(plan.stats.cycle_group_count, 1);
        // a depends on the cycle group, and lands in a later wave.
        let a = group_with_file(&plan, "a.rs");
        assert_eq!(a.depends_on, vec![cycle.id.clone()]);
        assert!(a.wave > cycle.wave);
        assert_eq!(cycle.wave, 0);
    }

    #[test]
    fn coalescing_merges_small_adjacent_files() {
        // x.rs -> y.rs, both tiny, both under threshold: one coalesced group.
        let mut facts = FactSet::default();
        facts.add_def(fdef("x", "x.rs", 3));
        facts.add_def(fdef("y", "y.rs", 3));
        facts.add_edge(Edge::call("x", "y", "x.rs", 2));

        let plan = plan_of(
            &facts,
            PlanOpts {
                max_group_loc: 200,
                coalesce_small: true,
            },
        );

        assert_eq!(plan.stats.group_count, 1);
        assert_eq!(plan.stats.coalesced_group_count, 1);
        assert_eq!(plan.stats.wave_count, 1);
        let g = &plan.groups[0];
        assert_eq!(g.reason, "coalesced-small");
        assert_eq!(g.files, vec!["x.rs".to_string(), "y.rs".to_string()]);
    }

    #[test]
    fn coalescing_skips_a_merge_that_would_cycle() {
        // a.rs -> big.rs, big.rs -> b.rs, a.rs -> b.rs. `big.rs` is large so it is
        // never a merge candidate and never absorbs a neighbor. Merging the two
        // small groups a and b would close a cycle a+b -> big -> a+b, so it must
        // be skipped, leaving three singleton groups.
        let mut facts = FactSet::default();
        facts.add_def(fdef("a", "a.rs", 3));
        facts.add_def(fdef("b", "b.rs", 3));
        facts.add_def(fdef("big", "big.rs", 300));
        facts.add_edge(Edge::call("a", "big", "a.rs", 2));
        facts.add_edge(Edge::call("big", "b", "big.rs", 2));
        facts.add_edge(Edge::call("a", "b", "a.rs", 3));

        let plan = plan_of(
            &facts,
            PlanOpts {
                max_group_loc: 200,
                coalesce_small: true,
            },
        );

        assert_eq!(plan.stats.group_count, 3);
        assert_eq!(plan.stats.coalesced_group_count, 0);
        assert_eq!(plan.stats.singleton_group_count, 3);
        // Sanity: the leaf-first order still holds (b -> big -> a).
        assert_eq!(group_with_file(&plan, "b.rs").wave, 0);
        assert_eq!(group_with_file(&plan, "big.rs").wave, 1);
        assert_eq!(group_with_file(&plan, "a.rs").wave, 2);
    }
}
