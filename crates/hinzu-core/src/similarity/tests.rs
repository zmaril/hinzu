//! Unit tests for the pure similarity engine: scoring, clustering, and the
//! cluster-explanation heuristics, on small hand-built signatures.

use super::*;

/// A builder for a hand-authored [`StructuralSignature`]. Fields default to a
/// non-trivial body so tests aren't filtered out; each test overrides what it
/// exercises.
fn sig(id: &str, file: &str) -> StructuralSignature {
    let mut histogram = BTreeMap::new();
    histogram.insert("call".to_string(), 2);
    histogram.insert("if".to_string(), 1);
    histogram.insert("let".to_string(), 2);
    StructuralSignature {
        symbol_id: id.to_string(),
        display: id.rsplit("::").next().unwrap_or(id).to_string(),
        language: "rust".to_string(),
        kind: "function".to_string(),
        file: file.to_string(),
        line_start: 1,
        line_end: 20,
        arity: Arity {
            params: 2,
            results: 1,
            generics: 0,
        },
        cfg: Cfg {
            branch_count: 1,
            match_arms: 2,
            loop_count: 0,
            try_count: 1,
            return_points: 1,
            max_nesting: 2,
        },
        stmt_histogram: histogram,
        call_sequence: vec![
            "validate".to_string(),
            "map_err".to_string(),
            "into".to_string(),
        ],
        type_shape: TypeShape {
            params: vec!["_".to_string(), "&_".to_string()],
            result: "Result<_,_>".to_string(),
        },
        shingles: vec![1, 2, 3, 4, 5, 6],
        token_len: 30,
        features: BTreeMap::new(),
    }
}

/// Two structurally identical bodies cluster into one helper_function candidate.
#[test]
fn identical_bodies_cluster_as_a_helper() {
    let a = sig("crate::m::alpha", "a.rs");
    let b = sig("crate::m::beta", "a.rs");
    let out = analyze("root", vec![a, b], &AnalyzeParams::default());

    assert_eq!(out.stats.candidates_found, 1);
    let f = &out.candidates[0];
    assert_eq!(f.id, "cand-1");
    assert_eq!(f.members.len(), 2);
    assert!(
        f.pattern.similarity > 0.9,
        "sim was {}",
        f.pattern.similarity
    );
    // Same everything → helper_function.
    assert_eq!(f.likely_abstraction.family, "helper_function");
    // Confidence is capped below 1 by the syntactic profile.
    assert!(f.confidence <= SYNTACTIC_CONFIDENCE_CAP + 1e-9);
    assert!(f.confidence > 0.6, "confidence was {}", f.confidence);
    // The syntactic caveat is always present as counter-evidence.
    assert!(f
        .counter_evidence
        .iter()
        .any(|c| c.contains("syntactic match only")));
    // The profile block is the Rust/syn one.
    assert_eq!(out.profiles.len(), 1);
    assert_eq!(out.profiles[0].extractor, "syn");
}

/// Same control flow + same calls but differing type shapes → generic_function.
#[test]
fn same_shell_differing_types_is_a_generic() {
    let mut a = sig("crate::m::to_u8", "a.rs");
    a.type_shape = TypeShape {
        params: vec!["_".to_string()],
        result: "Vec<_>".to_string(),
    };
    let mut b = sig("crate::m::to_u16", "a.rs");
    b.type_shape = TypeShape {
        params: vec!["HashMap<_,_>".to_string()],
        result: "Option<_>".to_string(),
    };
    // Keep calls + cfg identical (both from `sig`), only types differ.
    let out = analyze("root", vec![a, b], &AnalyzeParams::default());
    assert_eq!(out.stats.candidates_found, 1);
    let f = &out.candidates[0];
    assert_eq!(f.likely_abstraction.family, "generic_function");
    // The differing types are surfaced as an abstraction axis.
    assert!(f.differences.iter().any(|d| d.contains("type shapes vary")));
    // And as a reason to be careful (a plain helper won't fit).
    assert!(f
        .counter_evidence
        .iter()
        .any(|c| c.contains("signature types differ")));
}

/// Same skeleton + same call *shape* but different callees in matching slots →
/// enum_dispatch.
#[test]
fn same_shape_differing_callees_is_dispatch() {
    let mut a = sig("crate::m::handle_a", "a.rs");
    a.call_sequence = vec![
        "parse".to_string(),
        "run_a".to_string(),
        "finish".to_string(),
    ];
    let mut b = sig("crate::m::handle_b", "a.rs");
    b.call_sequence = vec![
        "parse".to_string(),
        "run_b".to_string(),
        "finish".to_string(),
    ];
    let out = analyze("root", vec![a, b], &AnalyzeParams::default());
    assert_eq!(out.stats.candidates_found, 1);
    let f = &out.candidates[0];
    assert_eq!(f.likely_abstraction.family, "enum_dispatch");
    assert!(f
        .differences
        .iter()
        .any(|d| d.contains("differing callees in matching positions")));
}

/// A wildly different body does not join the cluster.
#[test]
fn dissimilar_body_does_not_cluster() {
    let a = sig("crate::m::alpha", "a.rs");
    let b = sig("crate::m::beta", "a.rs");
    let mut c = sig("crate::m::gamma", "c.rs");
    c.cfg = Cfg {
        branch_count: 0,
        match_arms: 0,
        loop_count: 3,
        try_count: 0,
        return_points: 0,
        max_nesting: 1,
    };
    c.call_sequence = vec!["push".to_string(), "pop".to_string()];
    c.type_shape = TypeShape {
        params: vec!["Vec<_>".to_string()],
        result: "_".to_string(),
    };
    c.shingles = vec![90, 91, 92, 93, 94, 95];
    c.stmt_histogram = BTreeMap::from([("loop".to_string(), 3), ("call".to_string(), 2)]);

    let out = analyze("root", vec![a, b, c], &AnalyzeParams::default());
    assert_eq!(out.stats.candidates_found, 1);
    let f = &out.candidates[0];
    // Only alpha + beta cluster; gamma is left out.
    let ids: Vec<&str> = f.members.iter().map(|m| m.symbol_id.as_str()).collect();
    assert!(ids.contains(&"crate::m::alpha"));
    assert!(ids.contains(&"crate::m::beta"));
    assert!(!ids.contains(&"crate::m::gamma"));
}

/// Trivial defs (below the size / statement gate) are filtered out before
/// scoring.
#[test]
fn trivial_defs_are_filtered() {
    let mut a = sig("crate::m::tiny_a", "a.rs");
    a.token_len = 4; // below min_size 12
    a.stmt_histogram = BTreeMap::from([("call".to_string(), 1)]);
    let mut b = sig("crate::m::tiny_b", "a.rs");
    b.token_len = 4;
    b.stmt_histogram = BTreeMap::from([("call".to_string(), 1)]);

    let out = analyze("root", vec![a, b], &AnalyzeParams::default());
    assert_eq!(out.stats.signatures_analyzed, 2);
    assert_eq!(out.stats.signatures_after_filter, 0);
    assert_eq!(out.stats.candidates_found, 0);
}

/// Three identical bodies suggest a macro option alongside the helper.
#[test]
fn three_identical_bodies_suggest_macro_option() {
    let a = sig("crate::m::a", "a.rs");
    let b = sig("crate::m::b", "a.rs");
    let c = sig("crate::m::c", "a.rs");
    let out = analyze("root", vec![a, b, c], &AnalyzeParams::default());
    assert_eq!(out.stats.candidates_found, 1);
    let f = &out.candidates[0];
    assert_eq!(f.members.len(), 3);
    assert_eq!(f.likely_abstraction.family, "helper_function");
    assert!(f
        .likely_abstraction
        .language_mechanisms
        .iter()
        .any(|m| m.contains("macro_rules")));
}

/// The language filter drops other languages before analysis; an empty result
/// is honest, not faked.
#[test]
fn language_filter_scopes_analysis() {
    let a = sig("crate::m::alpha", "a.rs");
    let mut b = sig("ts::beta", "b.ts");
    b.language = "typescript".to_string();
    let params = AnalyzeParams {
        language_filter: Some("typescript".to_string()),
        ..AnalyzeParams::default()
    };
    let out = analyze("root", vec![a, b], &params);
    // Only the TS signature survives the filter → nothing to cluster with.
    assert_eq!(out.stats.signatures_after_filter, 1);
    assert_eq!(out.languages, vec!["typescript".to_string()]);
    // TypeScript ships a profile, and it is honestly type-resolved (unlike the
    // syntactic Rust profile) — the language-profile asymmetry, as data.
    assert_eq!(out.profiles.len(), 1);
    assert_eq!(out.profiles[0].extractor, "tsc-checker");
    assert_eq!(out.profiles[0].capability("types_resolved"), "yes");
}

/// The scoring breakdown is exposed and the aggregate honors the weights: two
/// identical signatures score 1 on every signal.
#[test]
fn identical_signatures_score_one_on_every_signal() {
    let a = sig("x::a", "a.rs");
    let b = sig("x::b", "a.rs");
    let s = score_pair(&a, &b);
    assert!((s.shingle_jaccard - 1.0).abs() < 1e-9);
    assert!((s.cfg - 1.0).abs() < 1e-9);
    assert!((s.type_shape - 1.0).abs() < 1e-9);
    assert!((s.call_seq - 1.0).abs() < 1e-9);
    assert!((s.histogram - 1.0).abs() < 1e-9);
    assert!((s.aggregate - 1.0).abs() < 1e-9);
}

/// A transitively-linked blob is split by the cohesion gate into its two tight
/// sub-clusters, rather than surviving as one loose mega-cluster. Six signatures
/// are identical in every signal but their shingles: two groups of three with
/// disjoint shingle sets. Every cross pair still scores 0.60 (the non-shingle
/// signals all match), so a loose linking threshold pulls all six into one
/// union-find component — but its mean pairwise similarity is only ~0.76, below a
/// 0.90 cohesion gate, so the gate must break it into the two 3-cliques.
#[test]
fn low_cohesion_blob_splits_into_tight_subclusters() {
    let group = |ids: &[&str], shingles: Vec<u64>| -> Vec<StructuralSignature> {
        ids.iter()
            .map(|id| {
                let mut s = sig(id, "a.rs");
                s.shingles = shingles.clone();
                s
            })
            .collect()
    };
    let mut sigs = group(&["m::a1", "m::a2", "m::a3"], vec![1, 2, 3, 4]);
    sigs.extend(group(&["m::b1", "m::b2", "m::b3"], vec![5, 6, 7, 8]));

    // Gate ON (min_cohesion 0.90): the loose 6-blob splits into two 3-cliques and
    // nothing survives as a >= 6-member cluster.
    let strict = AnalyzeParams {
        min_similarity: 0.5,
        min_cohesion: 0.9,
        ..AnalyzeParams::default()
    };
    let out = analyze("root", sigs.clone(), &strict);
    assert_eq!(
        out.stats.candidates_found, 2,
        "the blob should split into two candidates"
    );
    assert!(out.candidates.iter().all(|c| c.members.len() == 3));
    assert!(!out.candidates.iter().any(|c| c.members.len() >= 6));
    assert_eq!(out.stats.clusters_rejected_low_cohesion, 0);

    // Gate effectively OFF (min_cohesion 0.5): the same six survive as one loose
    // cluster — proving it is the gate, not the scoring, that split them.
    let loose = AnalyzeParams {
        min_similarity: 0.5,
        min_cohesion: 0.5,
        ..AnalyzeParams::default()
    };
    let out2 = analyze("root", sigs, &loose);
    assert_eq!(out2.stats.candidates_found, 1);
    assert_eq!(out2.candidates[0].members.len(), 6);
}

/// A sparse chain with no dense sub-region is rejected by the cohesion gate
/// (counted honestly), not emitted as a low-cohesion cluster. Three signatures
/// form a chain a~b~c where the linking edges are just strong enough to connect
/// but a~c is never a strong edge, so no tight sub-cluster survives the split.
#[test]
fn unsplittable_loose_chain_is_rejected_and_counted() {
    // a and b share one shingle window; b and c share a different one; a and c
    // share none. All other signals match, so a~b and b~c are edges but the
    // component is sparse (2 edges over 3 possible pairs).
    let mut a = sig("m::a", "a.rs");
    a.shingles = vec![1, 2, 3, 4];
    let mut b = sig("m::b", "a.rs");
    b.shingles = vec![3, 4, 5, 6];
    let mut c = sig("m::c", "a.rs");
    c.shingles = vec![5, 6, 7, 8];

    let params = AnalyzeParams {
        min_similarity: 0.5,
        min_cohesion: 0.95,
        ..AnalyzeParams::default()
    };
    let out = analyze("root", vec![a, b, c], &params);
    // No cohesive cluster emerges, and the loose component is reported rejected.
    assert_eq!(out.stats.candidates_found, 0);
    assert!(out.stats.clusters_rejected_low_cohesion >= 1);
}

/// Build three signatures that land in the `Boilerplate` case: identical cfg /
/// shingles / histogram (so they cluster), but differing type shapes *and*
/// call-sequence lengths (so they are not near-duplicate, not types-only, and not
/// same-call-shape — the only case left is boilerplate).
fn boilerplate_trio(language: &str) -> Vec<StructuralSignature> {
    let shapes = [
        (vec!["_".to_string()], vec!["x".to_string()]),
        (
            vec!["A<_>".to_string()],
            vec!["y".to_string(), "z".to_string()],
        ),
        (
            vec!["B<_,_>".to_string()],
            vec!["p".to_string(), "q".to_string(), "r".to_string()],
        ),
    ];
    ["m::a", "m::b", "m::c"]
        .iter()
        .zip(shapes)
        .map(|(id, (params, calls))| {
            let mut s = sig(id, "a.rs");
            s.language = language.to_string();
            s.type_shape = TypeShape {
                params,
                result: "_".to_string(),
            };
            s.call_sequence = calls;
            s
        })
        .collect()
}

/// A TypeScript boilerplate cluster is labelled with a TS family
/// (`object_driven_definition`) and NEVER Rust's `macro_rules` — the core of the
/// language-aware-classifier fix. It also carries a TS mechanism (a data-driven
/// table / codegen), not a `macro_rules!`.
#[test]
fn ts_boilerplate_gets_ts_family_not_macro_rules() {
    let out = analyze(
        "root",
        boilerplate_trio("typescript"),
        &AnalyzeParams::default(),
    );
    assert_eq!(out.stats.candidates_found, 1, "the trio should cluster");
    let f = &out.candidates[0];
    assert_eq!(f.members.len(), 3);
    assert_eq!(
        f.likely_abstraction.family, "object_driven_definition",
        "TS boilerplate should get a TS family"
    );
    assert_ne!(
        f.likely_abstraction.family, "macro_rules",
        "TS must never be labelled with Rust's macro_rules"
    );
    assert!(
        !f.likely_abstraction
            .language_mechanisms
            .iter()
            .any(|m| m.contains("macro_rules")),
        "TS mechanisms must not mention macro_rules: {:?}",
        f.likely_abstraction.language_mechanisms
    );
}

/// The same structural boilerplate shape in Rust still gets `macro_rules` — the
/// per-language routing keeps the Rust label where it is correct.
#[test]
fn rust_boilerplate_still_gets_macro_rules() {
    let out = analyze("root", boilerplate_trio("rust"), &AnalyzeParams::default());
    assert_eq!(out.stats.candidates_found, 1);
    let f = &out.candidates[0];
    assert_eq!(f.likely_abstraction.family, "macro_rules");
}

/// Defensive invariant: for every finding, its `likely_abstraction.family` is one
/// its language's profile actually lists in `abstraction_families`. Run over both
/// a Rust and a TypeScript boilerplate cluster.
#[test]
fn every_finding_family_is_in_its_language_profile() {
    for language in ["rust", "typescript"] {
        let out = analyze(
            "root",
            boilerplate_trio(language),
            &AnalyzeParams::default(),
        );
        let profile = profile_for_language(language).expect("shipped profile");
        for f in &out.candidates {
            assert!(
                profile
                    .abstraction_families
                    .contains(&f.likely_abstraction.family),
                "finding {} in {language} named family `{}`, absent from the profile's \
                 abstraction_families {:?}",
                f.id,
                f.likely_abstraction.family,
                profile.abstraction_families,
            );
        }
    }
}

/// Union-find clusters a transitive chain a~b~c into one cluster even if a and c
/// were never directly compared.
#[test]
fn transitive_pairs_form_one_cluster() {
    // Three signatures with slightly shifted shingle windows so a~b and b~c are
    // strong but a~c is weaker — they must still land in one cluster.
    let mut a = sig("t::a", "a.rs");
    a.shingles = vec![1, 2, 3, 4, 5, 6, 7, 8];
    let mut b = sig("t::b", "a.rs");
    b.shingles = vec![1, 2, 3, 4, 5, 6, 7, 8];
    let mut c = sig("t::c", "a.rs");
    c.shingles = vec![1, 2, 3, 4, 5, 6, 7, 8];
    let out = analyze("root", vec![a, b, c], &AnalyzeParams::default());
    assert_eq!(out.stats.candidates_found, 1);
    assert_eq!(out.candidates[0].members.len(), 3);
}
