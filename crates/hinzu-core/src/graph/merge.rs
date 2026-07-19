//! Cross-package graph union + per-package slicing. [`merge_graphs`] unions
//! several [`GraphOutput`]s into one graph (so a `--from` closure can cross
//! package boundaries), and [`reroot_subgraph`] partitions such a union back down
//! to one package's slice, re-rooted so a single-package matcher consumes it
//! unchanged. Both reuse the parent module's `assemble_graph` to recompute every
//! derived field, so a merged or sliced graph is a first-class [`GraphOutput`].

use std::collections::{BTreeMap, BTreeSet};

use super::{assemble_graph, node_meta_of, GraphOutput, NodeMeta, SymbolEdge};

/// Merge several [`GraphOutput`]s into a single union graph, then recompute every
/// derived field (adjacency, fan-in/out, transitive counts, the file rollup, and
/// the SCC condensation) over the union via the shared `assemble_graph` — so the
/// result is a standalone graph, closure-able and plan-able exactly like a freshly
/// built one.
///
/// This is how the CLI builds a **cross-package** source graph: extract each
/// package (or, in practice, one monorepo-rooted extraction whose files already
/// span several packages), then union them so a `--from` closure can cross package
/// boundaries — following the merged branch's call, reference, *and* signature-type
/// edges into whatever the entry point transitively needs, wherever it lives.
///
/// **Symbols** are unioned by id: the first graph a given id appears in wins, with
/// one refinement — an id first seen as an *external* leaf (no defining file) is
/// upgraded in place if a later graph carries it as an *internal* definition, so a
/// symbol that is a library boundary in one extraction but a local definition in
/// another lands as the local definition. **Edges** are unioned and de-duplicated
/// on their full identity (`from`/`to`/`kind`/`evidence`), so two extractions that
/// both observed the same edge contribute it once. `root` and `language` are taken
/// from the first input (they are labels); the fidelity/stat blocks are recomputed
/// by the assembly. The result is deterministic.
pub fn merge_graphs(graphs: Vec<GraphOutput>) -> GraphOutput {
    // Union the nodes by id (first-writer-wins, external upgraded to internal) and
    // carry each internal node's effect rollup across.
    let mut nodes: BTreeMap<String, Option<NodeMeta>> = BTreeMap::new();
    let mut effect_roots: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut root = String::new();
    let mut language: Option<String> = None;
    for (gi, graph) in graphs.iter().enumerate() {
        if gi == 0 {
            root = graph.root.clone();
            language = graph.language.clone();
        }
        for sym in &graph.symbols {
            let meta = node_meta_of(sym);
            match nodes.get(&sym.id) {
                // A later internal definition upgrades an earlier external leaf.
                Some(existing) if existing.is_none() && meta.is_some() => {
                    nodes.insert(sym.id.clone(), meta);
                    effect_roots.insert(sym.id.clone(), sym.effect_roots.clone());
                }
                Some(_) => {}
                None => {
                    if meta.is_some() {
                        effect_roots.insert(sym.id.clone(), sym.effect_roots.clone());
                    }
                    nodes.insert(sym.id.clone(), meta);
                }
            }
        }
    }

    // Union the edges, de-duplicated on their full identity so a shared edge two
    // extractions both saw is contributed once. Both endpoints must be union nodes
    // (build_graph always emits a node per edge endpoint, so this holds within any
    // one input; the guard keeps the invariant assemble_graph relies on).
    let mut seen_edges: BTreeSet<(String, String, String, String, u32)> = BTreeSet::new();
    let mut edges: Vec<SymbolEdge> = Vec::new();
    for graph in &graphs {
        for e in &graph.edges {
            if !nodes.contains_key(&e.from) || !nodes.contains_key(&e.to) {
                continue;
            }
            let key = (
                e.from.clone(),
                e.to.clone(),
                e.kind.clone(),
                e.evidence_file.clone(),
                e.evidence_line,
            );
            if seen_edges.insert(key) {
                edges.push(e.clone());
            }
        }
    }

    assemble_graph(&root, language.as_deref(), &nodes, edges, &effect_roots)
}

/// Partition a cross-package union graph down to **one package's slice** and
/// re-root it so it reads like a stand-alone single-package extraction.
///
/// Keeps only the internal (locally-defined) symbols whose file lives under
/// `path_prefix` (e.g. `"ai/"`), strips that prefix from every file path, symbol
/// id, and surviving edge endpoint, keeps the edges among the kept symbols, and
/// re-assembles. The result carries `src/…` file paths and `src/…#leaf` ids —
/// exactly the shape [`crate::portdiff::port_diff`] expects for a package whose
/// `source_src_prefix` is `src` — so a slice of a cross-package `--from` closure
/// can be matched against that package's target graph with the package's own
/// `PortDiffConfig`, unchanged. Cross-package edges (an endpoint outside the
/// prefix) are dropped, since the slice describes one package's internal structure.
pub fn reroot_subgraph(graph: &GraphOutput, path_prefix: &str) -> GraphOutput {
    let strip = |s: &str| -> Option<String> { s.strip_prefix(path_prefix).map(|r| r.to_string()) };

    // Kept internal symbols, re-rooted; their ids form the retained node set.
    let mut nodes: BTreeMap<String, Option<NodeMeta>> = BTreeMap::new();
    let mut effect_roots: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut id_rewrite: BTreeMap<&str, String> = BTreeMap::new();
    for sym in &graph.symbols {
        if sym.external {
            continue;
        }
        let Some(file) = sym.file.as_deref() else {
            continue;
        };
        let Some(new_file) = strip(file) else {
            continue;
        };
        // The id shares the file's prefix (`<file-sans-ext>#<leaf>`); strip it too,
        // falling back to the raw id if the shape is unexpected.
        let new_id = strip(&sym.id).unwrap_or_else(|| sym.id.clone());
        id_rewrite.insert(sym.id.as_str(), new_id.clone());
        effect_roots.insert(new_id.clone(), sym.effect_roots.clone());
        nodes.insert(
            new_id,
            Some(NodeMeta {
                display: sym.display.clone(),
                file: new_file,
                language: sym.language.clone().unwrap_or_default(),
                line_start: sym.line_start.unwrap_or(0),
                line_end: sym.line_end.unwrap_or(0),
            }),
        );
    }

    // Edges among kept symbols, endpoints rewritten to the stripped ids.
    let mut edges: Vec<SymbolEdge> = Vec::new();
    for e in &graph.edges {
        let (Some(from), Some(to)) = (
            id_rewrite.get(e.from.as_str()),
            id_rewrite.get(e.to.as_str()),
        ) else {
            continue;
        };
        edges.push(SymbolEdge {
            from: from.clone(),
            to: to.clone(),
            kind: e.kind.clone(),
            resolution: e.resolution.clone(),
            provenance: e.provenance.clone(),
            evidence_file: strip(&e.evidence_file).unwrap_or_else(|| e.evidence_file.clone()),
            evidence_line: e.evidence_line,
        });
    }

    assemble_graph(
        &graph.root,
        graph.language.as_deref(),
        &nodes,
        edges,
        &effect_roots,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{make_def, Edge, FactSet};
    use crate::graph::{build_graph, dependency_closure};

    /// Two package graphs whose only bridge is an edge from package A into a symbol
    /// package B defines: in A's own extraction that callee is an external leaf;
    /// merging in B's graph upgrades it to the local definition, so a `--from`
    /// closure now crosses the package boundary. This is the cross-package union the
    /// CLI relies on.
    fn two_package_union() -> GraphOutput {
        // Package A: `pkg_a/src/main#main` calls `pkg_b/src/lib#helper` (external
        // to A — B defines it).
        let mut fa = FactSet::default();
        fa.add_def(make_def("pkg_a/src/main#main", "pkg_a/src/main.ts", 1, 5));
        fa.add_edge(Edge::call(
            "pkg_a/src/main#main",
            "pkg_b/src/lib#helper",
            "pkg_a/src/main.ts",
            2,
        ));
        let ga = build_graph(&fa, "a", Some("typescript"));

        // Package B: defines `helper`, which calls a deeper local `util#deep`.
        let mut fb = FactSet::default();
        fb.add_def(make_def("pkg_b/src/lib#helper", "pkg_b/src/lib.ts", 1, 4));
        fb.add_def(make_def("pkg_b/src/util#deep", "pkg_b/src/util.ts", 1, 3));
        fb.add_edge(Edge::call(
            "pkg_b/src/lib#helper",
            "pkg_b/src/util#deep",
            "pkg_b/src/lib.ts",
            2,
        ));
        let gb = build_graph(&fb, "b", Some("typescript"));

        merge_graphs(vec![ga, gb])
    }

    #[test]
    fn merge_upgrades_external_to_internal_and_closure_spans_packages() {
        let merged = two_package_union();

        // `helper` was external in A's graph; the merge upgrades it to B's local
        // definition (a real file, non-external).
        let helper = merged
            .symbols
            .iter()
            .find(|s| s.id == "pkg_b/src/lib#helper")
            .expect("helper present");
        assert!(!helper.external);
        assert_eq!(helper.file.as_deref(), Some("pkg_b/src/lib.ts"));

        // The closure of A's entry now crosses into B — main → helper → deep.
        let closure: Vec<String> =
            dependency_closure(&merged, &["pkg_a/src/main#main".to_string()])
                .into_iter()
                .collect();
        assert_eq!(
            closure,
            vec![
                "pkg_a/src/main#main".to_string(),
                "pkg_b/src/lib#helper".to_string(),
                "pkg_b/src/util#deep".to_string(),
            ]
        );
        // The union's files span both packages.
        let files: BTreeSet<&str> = merged.files.iter().map(|f| f.path.as_str()).collect();
        assert!(files.iter().any(|f| f.starts_with("pkg_a/")));
        assert!(files.iter().any(|f| f.starts_with("pkg_b/")));
    }

    #[test]
    fn reroot_subgraph_slices_one_package_into_src_relative_shape() {
        let merged = two_package_union();
        // Route to package B: keep only its files, re-rooted to `src/…`.
        let sub = reroot_subgraph(&merged, "pkg_b/");

        // Package B's two symbols, ids/files stripped of the `pkg_b/` prefix.
        let ids: BTreeSet<&str> = sub.symbols.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            ["src/lib#helper", "src/util#deep"].into_iter().collect()
        );
        assert!(sub.symbols.iter().all(|s| !s.external));
        let helper = sub
            .symbols
            .iter()
            .find(|s| s.id == "src/lib#helper")
            .expect("helper present");
        assert_eq!(helper.file.as_deref(), Some("src/lib.ts"));
        // No package-A symbol leaked into B's slice.
        assert!(!sub.symbols.iter().any(|s| s.id.starts_with("pkg_a")));
        // The internal edge survived, re-rooted, so the slice is plan-able.
        assert!(sub
            .edges
            .iter()
            .any(|e| e.from == "src/lib#helper" && e.to == "src/util#deep"));
        assert_eq!(
            sub.condensation.symbol_topo_order,
            vec!["src/util#deep".to_string(), "src/lib#helper".to_string()]
        );
    }
}
