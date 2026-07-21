use super::*;
use crate::facts::{make_def, Edge, FactSet};
use crate::graph::build_graph;
use crate::plan::{build_plan, PlanOpts};

/// The prototype's TS→Rust naming rules, minus the conformance oracle (so the
/// synthetic tests never touch the filesystem).
fn rules() -> NamingRules {
    PortDiffConfig::default_ts_rust().naming
}

/// A config with no conformance oracle — DONE never fires, so band tests key
/// on the structural bands only.
fn cfg_no_conformance() -> PortDiffConfig {
    PortDiffConfig {
        conformance: None,
        ..PortDiffConfig::default_ts_rust()
    }
}

/// Build a `GraphOutput` from `(id, file)` defs and `(from, to)` call edges.
fn graph_of(defs: &[(&str, &str)], edges: &[(&str, &str)]) -> GraphOutput {
    let mut facts = FactSet::default();
    for (id, file) in defs {
        facts.add_def(make_def(id, file, 1, 5));
    }
    for (from, to) in edges {
        facts.add_edge(Edge::call(from, to, "x", 1));
    }
    build_graph(&facts, "test", None)
}

// ---- 1. Normalization -------------------------------------------------

#[test]
fn normalization_camel_pascal_screaming() {
    let r = rules();
    assert_eq!(camel_to_snake("convertMessages"), "convert_messages");
    assert_eq!(camel_to_snake("toClaudeCodeName"), "to_claude_code_name");
    assert_eq!(camel_to_snake("isOAuthToken"), "is_o_auth_token");
    // PascalCase types and SCREAMING consts are kept verbatim.
    assert_eq!(norm_leaf("AnthropicModel", &r), "AnthropicModel");
    assert_eq!(norm_leaf("MAX_TOKENS", &r), "MAX_TOKENS");
    assert_eq!(norm_leaf("convertMessages", &r), "convert_messages");
}

#[test]
fn normalization_trait_impl_and_synthetic_leaves() {
    // Trait-impl method is the tail after the last `>::`.
    assert_eq!(
        target_leaf_raw("<atilla_ai::api::anthropic::AnthropicModel as std::clone::Clone>::clone")
            .as_deref(),
        Some("clone")
    );
    assert_eq!(
        target_leaf_raw("atilla_ai::api::google_shared::short_hash").as_deref(),
        Some("short_hash")
    );
    // Synthetic tails are rejected.
    assert!(target_leaf_raw(
            "<atilla_ai::auth::context::DefaultAuthContext<E> as atilla_ai::auth::types::AuthContext>::env::{closure#0}"
        )
        .is_none());
    assert!(target_leaf_raw("atilla_ai::api::a::FIELDS").is_none());
    assert!(target_leaf_raw("atilla_ai::api::a::{constant#1}").is_none());
}

#[test]
fn normalization_module_anchoring() {
    let r = rules();
    // mod.rs anchors to the directory; a plain file to itself; a sibling .rs
    // beside a same-named directory to itself.
    assert_eq!(
        target_file_to_module("crates/atilla-ai/src/api/anthropic/mod.rs", &r),
        "api/anthropic"
    );
    assert_eq!(
        target_file_to_module("crates/atilla-ai/src/api/anthropic/boundary.rs", &r),
        "api/anthropic/boundary"
    );
    assert_eq!(
        target_file_to_module("crates/atilla-ai/src/api/anthropic.rs", &r),
        "api/anthropic"
    );
    // Source files: kebab→snake, `.lazy` folded, extension stripped.
    assert_eq!(
        source_file_to_module("src/api/anthropic-messages.lazy.ts", &r),
        "api/anthropic_messages"
    );
    assert_eq!(
        source_file_to_module("src/utils/event-stream.ts", &r),
        "utils/event_stream"
    );
}

// ---- 2. Decomposition-aware clustering --------------------------------

#[test]
fn clustering_recovers_a_relocated_file() {
    // Source file `src/api/simple-options.ts` holds three distinctive leaves.
    // The target relocated them into the subtree `api/anthropic/simple_options`
    // (there is NO target module `api/simple_options`), plus an unrelated
    // module so the cluster has to concentrate.
    let source = graph_of(
        &[
            (
                "src/api/simple-options.ts#convertOptions",
                "src/api/simple-options.ts",
            ),
            (
                "src/api/simple-options.ts#buildParams",
                "src/api/simple-options.ts",
            ),
            (
                "src/api/simple-options.ts#mapModel",
                "src/api/simple-options.ts",
            ),
        ],
        &[],
    );
    let target = graph_of(
        &[
            (
                "atilla_ai::api::anthropic::simple_options::convert_options",
                "crates/atilla-ai/src/api/anthropic/simple_options.rs",
            ),
            (
                "atilla_ai::api::anthropic::simple_options::build_params",
                "crates/atilla-ai/src/api/anthropic/simple_options.rs",
            ),
            (
                "atilla_ai::api::anthropic::simple_options::map_model",
                "crates/atilla-ai/src/api/anthropic/simple_options.rs",
            ),
            (
                "atilla_ai::util::unrelated::helper",
                "crates/atilla-ai/src/util/unrelated.rs",
            ),
        ],
        &[],
    );
    let plan = build_plan(&source, PlanOpts::default());
    let report = port_diff(&source, &plan, &target, &cfg_no_conformance(), None);

    let fe = report
        .files
        .iter()
        .find(|f| f.path == "src/api/simple-options.ts")
        .unwrap();
    assert_eq!(fe.map_method.as_deref(), Some("graph-cluster"));
    assert_eq!(
        fe.mapped_target.as_deref(),
        Some("api/anthropic/simple_options")
    );
    // All three symbols matched into the recovered subtree.
    assert_eq!(fe.matched_symbols, 3);
    assert_eq!(fe.total_symbols, 3);
    assert_eq!(fe.tier_breakdown.subtree, 3);
    assert_eq!(fe.band, Band::Ported);
    // And it shows up as a recovered file the naive path pass would miss.
    assert!(report
        .naive_vs_graph
        .recovered_files
        .contains(&"src/api/simple-options.ts".to_string()));
}

#[test]
fn cross_crate_symbol_is_matched_when_graphs_merged() {
    // A source package ported across two crates: `keep.ts#stays` landed in
    // crate `a`, but `moved.ts#relocated` was ported into a DIFFERENT crate
    // `b`. When both crates' symbols share one (merged) target graph, the
    // file whose symbol only exists in crate `b` still matches — it is not
    // banded NOT-STARTED. This is the cross-crate visibility fix.
    let source = graph_of(
        &[
            ("src/keep.ts#stays", "src/keep.ts"),
            ("src/moved.ts#relocated", "src/moved.ts"),
        ],
        &[],
    );
    // `stays` is in crate a; `relocated` is only in crate b.
    let target = graph_of(
        &[
            ("a::keep::stays", "crates/a/src/keep.rs"),
            ("b::moved::relocated", "crates/b/src/moved.rs"),
        ],
        &[],
    );
    let plan = build_plan(&source, PlanOpts::default());
    let report = port_diff(&source, &plan, &target, &cfg_no_conformance(), None);

    let moved = report
        .files
        .iter()
        .find(|f| f.path == "src/moved.ts")
        .unwrap();
    // The symbol in the other crate is visible: matched, not NOT-STARTED.
    assert_eq!(moved.matched_symbols, 1);
    assert_ne!(moved.band, Band::NotStarted);
}

// ---- 3. Graph-confirm -------------------------------------------------

#[test]
fn graph_confirm_rewards_edge_overlap_not_name_coincidence() {
    // File A: `alpha` calls `beta`; the target `a_mod` has `alpha`->`beta` too,
    // so the match is edge-confirmed (overlap 1.0).
    // File C: `gamma` calls `delta`; both names exist in target `c_mod`, but
    // `gamma` does NOT call `delta` there — a name coincidence, overlap 0.
    let source = graph_of(
        &[
            ("src/a.ts#alpha", "src/a.ts"),
            ("src/a.ts#beta", "src/a.ts"),
            ("src/c.ts#gamma", "src/c.ts"),
            ("src/c.ts#delta", "src/c.ts"),
        ],
        &[
            ("src/a.ts#alpha", "src/a.ts#beta"),
            ("src/c.ts#gamma", "src/c.ts#delta"),
        ],
    );
    let target = graph_of(
        &[
            ("atilla_ai::a::alpha", "crates/atilla-ai/src/a.rs"),
            ("atilla_ai::a::beta", "crates/atilla-ai/src/a.rs"),
            ("atilla_ai::c::gamma", "crates/atilla-ai/src/c.rs"),
            ("atilla_ai::c::delta", "crates/atilla-ai/src/c.rs"),
            // A distractor so `gamma` has an out-edge that isn't `delta`.
            ("atilla_ai::c::other", "crates/atilla-ai/src/c.rs"),
        ],
        &[
            ("atilla_ai::a::alpha", "atilla_ai::a::beta"),
            ("atilla_ai::c::gamma", "atilla_ai::c::other"),
        ],
    );
    let plan = build_plan(&source, PlanOpts::default());
    let report = port_diff(&source, &plan, &target, &cfg_no_conformance(), None);

    // Two evaluable matched symbols (alpha, gamma), exactly one confirmed.
    assert_eq!(report.overall.graph.evaluable, 2);
    assert_eq!(report.overall.graph.confirmed, 1);

    let fa = report.files.iter().find(|f| f.path == "src/a.ts").unwrap();
    let fc = report.files.iter().find(|f| f.path == "src/c.ts").unwrap();
    assert_eq!(fa.graph_confirmed_coverage, Some(1.0));
    assert_eq!(fc.graph_confirmed_coverage, Some(0.0));
}

// ---- 4. Band classification ------------------------------------------

#[test]
fn band_thresholds() {
    // Native short-circuits to DONE regardless of coverage.
    assert_eq!(classify_band(true, 0, None, false, 0.6), Band::Done);
    // >= threshold, not native -> PORTED.
    assert_eq!(classify_band(false, 5, Some(0.6), true, 0.6), Band::Ported);
    assert_eq!(classify_band(false, 5, Some(0.8), true, 0.6), Band::Ported);
    // Below threshold but with a match/map -> STARTED.
    assert_eq!(classify_band(false, 5, Some(0.4), true, 0.6), Band::Started);
    // A mapped target with zero matched symbols still counts as STARTED.
    assert_eq!(classify_band(false, 3, Some(0.0), true, 0.6), Band::Started);
    // Nothing matched and nothing mapped -> NOT-STARTED.
    assert_eq!(
        classify_band(false, 3, Some(0.0), false, 0.6),
        Band::NotStarted
    );
    assert_eq!(classify_band(false, 0, None, false, 0.6), Band::NotStarted);
}

// ---- 5. Ready frontier ------------------------------------------------

#[test]
fn ready_frontier_needs_all_deps_ported() {
    // `hi.ts` depends on `lo.ts`; `lo.ts` is fully ported, `hi.ts` is not.
    // `orphan.ts` depends on `unported.ts` which is NOT-STARTED, so it is held
    // off the frontier.
    let source = graph_of(
        &[
            ("src/hi.ts#useLow", "src/hi.ts"),
            ("src/lo.ts#lowHelper", "src/lo.ts"),
            ("src/orphan.ts#useUnported", "src/orphan.ts"),
            ("src/unported.ts#nope", "src/unported.ts"),
        ],
        &[
            ("src/hi.ts#useLow", "src/lo.ts#lowHelper"),
            ("src/orphan.ts#useUnported", "src/unported.ts#nope"),
        ],
    );
    // Target ports `lo` (so lo.ts is PORTED) but NOT hi/orphan/unported.
    let target = graph_of(
        &[("atilla_ai::lo::low_helper", "crates/atilla-ai/src/lo.rs")],
        &[],
    );
    let plan = build_plan(&source, PlanOpts::default());
    let report = port_diff(&source, &plan, &target, &cfg_no_conformance(), None);

    let lo = report.files.iter().find(|f| f.path == "src/lo.ts").unwrap();
    assert_eq!(lo.band, Band::Ported);

    let frontier: Vec<&str> = report
        .ready_frontier
        .iter()
        .map(|f| f.path.as_str())
        .collect();
    // hi.ts is unported but its only dep (lo.ts) is ported -> on the frontier.
    assert!(frontier.contains(&"src/hi.ts"));
    // orphan.ts depends on an unported file -> NOT on the frontier.
    assert!(!frontier.contains(&"src/orphan.ts"));
}
