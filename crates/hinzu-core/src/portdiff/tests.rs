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

/// A two-target-crate config: `crates/primary/src` is the PRIMARY crate (index 0),
/// `crates/secondary/src` the secondary. Reuses the prototype naming rules, only
/// swapping the crate prefixes so the primary/secondary distinction is real.
fn cfg_two_crates() -> PortDiffConfig {
    let mut cfg = cfg_no_conformance();
    cfg.naming.target_src_prefix = vec![
        "crates/primary/src".to_string(),
        "crates/secondary/src".to_string(),
    ];
    cfg.naming.strip_crate_prefix = vec!["primary".to_string(), "secondary".to_string()];
    cfg
}

#[test]
fn relocated_band_flags_a_port_in_a_secondary_crate() {
    // Two source files, each with one matchable symbol. `kept.ts#stays` ports
    // in place into the PRIMARY crate; `moved.ts#gone` ports into the SECONDARY
    // crate. Both would otherwise band PORTED (coverage 1.0), but the moved file
    // is relabeled RELOCATED because its match landed outside the primary crate.
    let source = graph_of(
        &[
            ("src/kept.ts#stays", "src/kept.ts"),
            ("src/moved.ts#gone", "src/moved.ts"),
        ],
        &[],
    );
    let target = graph_of(
        &[
            ("primary::kept::stays", "crates/primary/src/kept.rs"),
            ("secondary::moved::gone", "crates/secondary/src/moved.rs"),
        ],
        &[],
    );
    let plan = build_plan(&source, PlanOpts::default());
    let report = port_diff(&source, &plan, &target, &cfg_two_crates(), None);

    let find = |path: &str| {
        report
            .files
            .iter()
            .find(|f| f.path == path)
            .unwrap_or_else(|| panic!("missing {path}"))
    };
    // Control: matched in the primary crate → normal PORTED band.
    let kept = find("src/kept.ts");
    assert_eq!(kept.matched_symbols, 1);
    assert_eq!(kept.band, Band::Ported);
    // Subject: matched predominantly in a secondary crate → RELOCATED.
    let moved = find("src/moved.ts");
    assert_eq!(moved.matched_symbols, 1);
    assert_eq!(moved.band, Band::Relocated);
    // The band count reflects exactly one relocated file.
    assert_eq!(report.overall.bands.relocated, 1);
    assert_eq!(report.overall.bands.ported, 1);
}

#[test]
fn single_crate_package_never_relocates() {
    // Backward-compat: with one configured target crate, every match is in the
    // primary crate, so RELOCATED can never fire even for a decomposed port.
    let source = graph_of(&[("src/only.ts#thing", "src/only.ts")], &[]);
    let target = graph_of(
        &[("atilla_ai::only::thing", "crates/atilla-ai/src/only.rs")],
        &[],
    );
    let plan = build_plan(&source, PlanOpts::default());
    let report = port_diff(&source, &plan, &target, &cfg_no_conformance(), None);

    let only = report
        .files
        .iter()
        .find(|f| f.path == "src/only.ts")
        .unwrap();
    assert_eq!(only.band, Band::Ported);
    assert_eq!(report.overall.bands.relocated, 0);
}

// ---- 6. Split-not-merge detector --------------------------------------

#[test]
fn slice_merge_two_substantial_contributors_neither_dominant_is_flagged() {
    // The signature v1 miss: a target file `content.rs` folds a *slice* of two
    // source files, and NEITHER slice is that file's plurality home. Each source
    // file decomposes across the `api/anthropic` subtree — a 3-symbol home, a
    // 2-symbol slice into the shared `content.rs`, and a 2-symbol extra — so no
    // single target module holds ≥ 60% of its leaves and it clusters to the
    // `api/anthropic` parent, making every match STRONG (subtree tier). Both files
    // then land 2 STRONG symbols in `content.rs` (a substantial contribution each),
    // yet each file's DOMINANT target is its own 3-symbol home — so v1's
    // plurality-of-dominant scheme never flags content.rs. The substantial-
    // contributor detector does.
    let source = graph_of(
        &[
            ("src/msg-a.ts#a_home1", "src/msg-a.ts"),
            ("src/msg-a.ts#a_home2", "src/msg-a.ts"),
            ("src/msg-a.ts#a_home3", "src/msg-a.ts"),
            ("src/msg-a.ts#a_content1", "src/msg-a.ts"),
            ("src/msg-a.ts#a_content2", "src/msg-a.ts"),
            ("src/msg-a.ts#a_extra1", "src/msg-a.ts"),
            ("src/msg-a.ts#a_extra2", "src/msg-a.ts"),
            ("src/msg-b.ts#b_home1", "src/msg-b.ts"),
            ("src/msg-b.ts#b_home2", "src/msg-b.ts"),
            ("src/msg-b.ts#b_home3", "src/msg-b.ts"),
            ("src/msg-b.ts#b_content1", "src/msg-b.ts"),
            ("src/msg-b.ts#b_content2", "src/msg-b.ts"),
            ("src/msg-b.ts#b_extra1", "src/msg-b.ts"),
            ("src/msg-b.ts#b_extra2", "src/msg-b.ts"),
        ],
        &[],
    );
    let target = graph_of(
        &[
            // msg-a's home (3), its content slice (2), its extra (2).
            (
                "atilla_ai::api::anthropic::a_home::a_home1",
                "crates/atilla-ai/src/api/anthropic/a_home.rs",
            ),
            (
                "atilla_ai::api::anthropic::a_home::a_home2",
                "crates/atilla-ai/src/api/anthropic/a_home.rs",
            ),
            (
                "atilla_ai::api::anthropic::a_home::a_home3",
                "crates/atilla-ai/src/api/anthropic/a_home.rs",
            ),
            (
                "atilla_ai::api::anthropic::content::a_content1",
                "crates/atilla-ai/src/api/anthropic/content.rs",
            ),
            (
                "atilla_ai::api::anthropic::content::a_content2",
                "crates/atilla-ai/src/api/anthropic/content.rs",
            ),
            (
                "atilla_ai::api::anthropic::a_extra::a_extra1",
                "crates/atilla-ai/src/api/anthropic/a_extra.rs",
            ),
            (
                "atilla_ai::api::anthropic::a_extra::a_extra2",
                "crates/atilla-ai/src/api/anthropic/a_extra.rs",
            ),
            // msg-b's home (3), its content slice (2), its extra (2).
            (
                "atilla_ai::api::anthropic::b_home::b_home1",
                "crates/atilla-ai/src/api/anthropic/b_home.rs",
            ),
            (
                "atilla_ai::api::anthropic::b_home::b_home2",
                "crates/atilla-ai/src/api/anthropic/b_home.rs",
            ),
            (
                "atilla_ai::api::anthropic::b_home::b_home3",
                "crates/atilla-ai/src/api/anthropic/b_home.rs",
            ),
            (
                "atilla_ai::api::anthropic::content::b_content1",
                "crates/atilla-ai/src/api/anthropic/content.rs",
            ),
            (
                "atilla_ai::api::anthropic::content::b_content2",
                "crates/atilla-ai/src/api/anthropic/content.rs",
            ),
            (
                "atilla_ai::api::anthropic::b_extra::b_extra1",
                "crates/atilla-ai/src/api/anthropic/b_extra.rs",
            ),
            (
                "atilla_ai::api::anthropic::b_extra::b_extra2",
                "crates/atilla-ai/src/api/anthropic/b_extra.rs",
            ),
        ],
        &[],
    );
    let plan = build_plan(&source, PlanOpts::default());
    let report = port_diff(&source, &plan, &target, &cfg_no_conformance(), None);

    // Neither source file's DOMINANT target is content.rs (v1 would miss it).
    let dom = |p: &str| {
        report
            .files
            .iter()
            .find(|f| f.path == p)
            .unwrap()
            .dominant_target_file
            .clone()
    };
    assert_eq!(
        dom("src/msg-a.ts").as_deref(),
        Some("crates/atilla-ai/src/api/anthropic/a_home.rs")
    );
    assert_eq!(
        dom("src/msg-b.ts").as_deref(),
        Some("crates/atilla-ai/src/api/anthropic/b_home.rs")
    );

    // content.rs is flagged as a file-merge of the two slices.
    let m = report
        .merges
        .file_merges
        .iter()
        .find(|e| e.target_file == "crates/atilla-ai/src/api/anthropic/content.rs")
        .expect("content.rs should be flagged as a slice-merge");
    let mut srcs: Vec<&str> = m
        .contributors
        .iter()
        .map(|c| c.source_file.as_str())
        .collect();
    srcs.sort();
    assert_eq!(srcs, vec!["src/msg-a.ts", "src/msg-b.ts"]);
    assert!(!m.cross_package);
    // The home files, each fed by a single source file, are NOT flagged.
    assert!(!report
        .merges
        .file_merges
        .iter()
        .any(|e| e.target_file.ends_with("a_home.rs") || e.target_file.ends_with("b_home.rs")));
}

#[test]
fn single_source_misplacement_into_another_packages_crate_is_flagged() {
    // A single source file of the `coding-agent` package ports substantially into
    // the `pidgin-ai` crate — no multi-source "merge" at all, so v1's ≥ 2-source
    // rule could never see it. The misplacement detector flags it because the
    // owning package of `pidgin-ai` (ai) differs from the source's package.
    let target = graph_of(
        &[
            // The ai package's own file, establishing ai as pidgin-ai's owner.
            ("pidgin_ai::types::AiThing", "crates/pidgin-ai/src/types.rs"),
            ("pidgin_ai::types::AiOther", "crates/pidgin-ai/src/types.rs"),
            ("pidgin_ai::types::AiMore", "crates/pidgin-ai/src/types.rs"),
            // The misplaced destination: a coding-agent file's symbols landed here.
            (
                "pidgin_ai::providers::composer::make",
                "crates/pidgin-ai/src/providers/composer.rs",
            ),
            (
                "pidgin_ai::providers::composer::wire",
                "crates/pidgin-ai/src/providers/composer.rs",
            ),
        ],
        &[],
    );
    // ai package: types.ts ports 1:1 into pidgin-ai/src/types.rs.
    let src_ai = graph_of(
        &[
            ("src/types.ts#AiThing", "src/types.ts"),
            ("src/types.ts#AiOther", "src/types.ts"),
            ("src/types.ts#AiMore", "src/types.ts"),
        ],
        &[],
    );
    // coding-agent package: composer.ts's symbols landed in the pidgin-ai crate.
    let src_ca = graph_of(
        &[
            ("src/composer.ts#make", "src/composer.ts"),
            ("src/composer.ts#wire", "src/composer.ts"),
        ],
        &[],
    );
    let cfg = cfg_no_conformance();
    let mk = |src: &GraphOutput| {
        port_diff(
            src,
            &build_plan(src, PlanOpts::default()),
            &target,
            &cfg,
            None,
        )
    };
    // Owning override from config: pidgin-ai is owned by the ai package.
    let mut owning = std::collections::BTreeMap::new();
    owning.insert("pidgin-ai".to_string(), "ai".to_string());
    let multi = MultiPackageReport::aggregate(
        "ts",
        "rust",
        vec![
            ("ai".to_string(), mk(&src_ai)),
            ("coding-agent".to_string(), mk(&src_ca)),
        ],
        &owning,
    );

    // composer.ts (coding-agent) is flagged as misplaced into the pidgin-ai crate.
    let mp = multi
        .merges
        .misplacements
        .iter()
        .find(|m| m.source_file == "src/composer.ts")
        .expect("composer.ts should be flagged as a misplacement");
    assert_eq!(mp.source_package, "coding-agent");
    assert_eq!(mp.target_crate, "pidgin-ai");
    assert_eq!(mp.owning_package, "ai");
    assert_eq!(mp.target_file, "crates/pidgin-ai/src/providers/composer.rs");
    // It is a single-source destination, so it is NOT a file-merge.
    assert!(!multi
        .merges
        .file_merges
        .iter()
        .any(|e| e.target_file.ends_with("providers/composer.rs")));
    // The ai package's own 1:1 port is not a misplacement.
    assert!(!multi
        .merges
        .misplacements
        .iter()
        .any(|m| m.source_file == "src/types.ts"));
}

#[test]
fn one_global_name_coincidence_is_not_flagged() {
    // `settings_list.rs` is `settings.ts`'s real 1:1 home (3 strong symbols).
    // `other.ts` shares a SINGLE leaf name (`render`) that resolves there by
    // global-name only — a 1-symbol coincidence. It is not a substantial
    // contributor, so `settings_list.rs` is NOT flagged as a merge.
    let source = graph_of(
        &[
            ("src/settings.ts#build", "src/settings.ts"),
            ("src/settings.ts#apply", "src/settings.ts"),
            ("src/settings.ts#reset", "src/settings.ts"),
            ("src/other.ts#render", "src/other.ts"),
            ("src/other.ts#unrelated", "src/other.ts"),
        ],
        &[],
    );
    let target = graph_of(
        &[
            (
                "atilla_ai::settings_list::build",
                "crates/atilla-ai/src/settings_list.rs",
            ),
            (
                "atilla_ai::settings_list::apply",
                "crates/atilla-ai/src/settings_list.rs",
            ),
            (
                "atilla_ai::settings_list::reset",
                "crates/atilla-ai/src/settings_list.rs",
            ),
            // `render` exists here too — the coincidental global-name leaf.
            (
                "atilla_ai::settings_list::render",
                "crates/atilla-ai/src/settings_list.rs",
            ),
            // other.ts's real home, so its dominant is elsewhere.
            (
                "atilla_ai::other::unrelated",
                "crates/atilla-ai/src/other.rs",
            ),
        ],
        &[],
    );
    let plan = build_plan(&source, PlanOpts::default());
    let report = port_diff(&source, &plan, &target, &cfg_no_conformance(), None);

    // other.ts contributes exactly one global-name symbol to settings_list.rs.
    let other = report
        .files
        .iter()
        .find(|f| f.path == "src/other.ts")
        .unwrap();
    let into_sl = other
        .target_file_contributions
        .iter()
        .find(|tc| tc.file == "crates/atilla-ai/src/settings_list.rs");
    if let Some(tc) = into_sl {
        assert_eq!(tc.strong_matched, 0);
        assert!(tc.total_matched <= 1);
    }
    // Not substantial → settings_list.rs is not flagged.
    assert!(report.merges.file_merges.is_empty());
    assert!(report.merges.misplacements.is_empty());
}

#[test]
fn clean_one_to_one_same_package_port_is_not_flagged() {
    // Two source files ported to two distinct target files, each a clean 1:1: no
    // target file draws substantial content from ≥ 2 source files, and every file
    // sits in its own package's crate, so nothing is flagged.
    let source = graph_of(
        &[
            ("src/a.ts#foo", "src/a.ts"),
            ("src/a.ts#foo2", "src/a.ts"),
            ("src/b.ts#bar", "src/b.ts"),
            ("src/b.ts#bar2", "src/b.ts"),
        ],
        &[],
    );
    let target = graph_of(
        &[
            ("atilla_ai::a::foo", "crates/atilla-ai/src/a.rs"),
            ("atilla_ai::a::foo2", "crates/atilla-ai/src/a.rs"),
            ("atilla_ai::b::bar", "crates/atilla-ai/src/b.rs"),
            ("atilla_ai::b::bar2", "crates/atilla-ai/src/b.rs"),
        ],
        &[],
    );
    let plan = build_plan(&source, PlanOpts::default());
    let report = port_diff(&source, &plan, &target, &cfg_no_conformance(), None);

    assert!(report.merges.file_merges.is_empty());
    assert!(report.merges.misplacements.is_empty());
}
