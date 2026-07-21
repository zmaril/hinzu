//! Unit tests for the curated-library tier: the derive predicates over
//! constructed impl/enum facts, the combinator body pattern, and the Tier-A
//! virtual-signature match — all toolchain-free on stable.

use super::*;
use crate::similarity::{Arity, Cfg, StructuralSignature, TypeShape};
use std::collections::BTreeMap;

/// A curated selection activating a crate's derive + function patterns at a
/// given trust.
fn sel(trust: f64) -> CuratedSelection {
    CuratedSelection {
        trust,
        derive: true,
        function: true,
    }
}

/// Params activating one curated crate.
fn params_for(crate_name: &str, trust: f64) -> LibraryParams {
    let mut curated = BTreeMap::new();
    curated.insert(crate_name.to_string(), sel(trust));
    LibraryParams {
        curated_crates: curated,
        min_virtual_match: 0.6,
    }
}

/// A trait impl fact.
fn timpl(name: &str, full: &str, line: u32) -> TraitImpl {
    TraitImpl {
        trait_name: name.to_string(),
        trait_full: full.to_string(),
        from_arg_shape: None,
        body_is_match_self: false,
        is_wrapping: false,
        line_start: line,
        line_end: line + 5,
    }
}

/// An enum error type with a hand-written Display + Error — the thiserror shape.
fn prompt_error_facts() -> TypeImplFacts {
    TypeImplFacts {
        type_name: "PromptError".to_string(),
        file: "turn.rs".to_string(),
        line_start: 80,
        line_end: 94,
        is_enum: true,
        variant_count: 2,
        traits: vec![
            {
                let mut d = timpl("Display", "std::fmt::Display", 85);
                d.body_is_match_self = true;
                d
            },
            timpl("Error", "std::error::Error", 94),
        ],
    }
}

#[test]
fn thiserror_matches_handwritten_display_plus_error() {
    let out = match_libraries(
        &[],
        &[prompt_error_facts()],
        &[],
        &params_for("thiserror", 0.8),
    );
    assert_eq!(out.len(), 1, "expected one thiserror finding");
    let f = &out[0];
    assert_eq!(f.id, "lib-1");
    assert_eq!(f.external.library, "thiserror");
    assert_eq!(f.external.item, "Error");
    assert_eq!(f.external.kind, ExternalKind::Derive);
    assert_eq!(f.external.source, ExternalSource::Curated);
    assert_eq!(f.likely_abstraction.family, "adopt_library");
    // confidence = 0.8 * 0.9 * 0.7 = 0.504
    assert!(
        (f.confidence - 0.50).abs() < 0.02,
        "confidence {}",
        f.confidence
    );
    // The impls it eliminates are cited as local members.
    assert!(f.local.iter().any(|m| m.display.contains("impl Display")));
    assert!(f.local.iter().any(|m| m.display.contains("impl Error")));
    // The universal caveats are present.
    assert!(f
        .counter_evidence
        .iter()
        .any(|c| c.contains("semantics unverified")));
    assert!(f
        .counter_evidence
        .iter()
        .any(|c| c.contains("adds a dependency")));
    assert!(f
        .counter_evidence
        .iter()
        .any(|c| c.contains("version skew")));
    assert!(f
        .counter_evidence
        .iter()
        .any(|c| c.contains("curated-pattern incompleteness")));
    // The profile is honest that it is a shape match.
    assert!(f
        .profile
        .limitations
        .iter()
        .any(|l| l.contains("shape match")));
}

#[test]
fn thiserror_needs_both_impls() {
    // Display alone (no Error impl) does not match thiserror.
    let mut facts = prompt_error_facts();
    facts.traits.retain(|t| t.trait_name != "Error");
    let out = match_libraries(&[], &[facts], &[], &params_for("thiserror", 0.8));
    assert!(
        out.is_empty(),
        "Display without Error should not match thiserror"
    );
}

#[test]
fn derive_more_from_matches_a_wrapping_from() {
    let facts = TypeImplFacts {
        type_name: "UserMessageContent".to_string(),
        file: "queue.rs".to_string(),
        line_start: 67,
        line_end: 72,
        is_enum: true,
        variant_count: 2,
        traits: vec![{
            let mut f = timpl("From", "From", 80);
            f.is_wrapping = true;
            f.from_arg_shape = Some("_".to_string());
            f
        }],
    };
    let out = match_libraries(&[], &[facts], &[], &params_for("derive_more", 0.7));
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].external.item, "From");
    assert!(out[0].match_basis[0].contains("wraps its argument"));
}

#[test]
fn derive_more_from_ignores_a_transforming_from() {
    // A non-wrapping From (transforms its arg) must not match.
    let facts = TypeImplFacts {
        type_name: "FactSet".to_string(),
        file: "facts.rs".to_string(),
        line_start: 1,
        line_end: 10,
        is_enum: false,
        variant_count: 0,
        traits: vec![timpl("From", "From", 5)], // is_wrapping = false
    };
    let out = match_libraries(&[], &[facts], &[], &params_for("derive_more", 0.7));
    assert!(
        out.is_empty(),
        "a transforming From must not match derive_more::From"
    );
}

/// A local function whose body is a fallible accumulation loop.
fn accumulating_loop_sig() -> StructuralSignature {
    let mut hist = BTreeMap::new();
    hist.insert("loop".to_string(), 1);
    hist.insert("try".to_string(), 1);
    hist.insert("method_call".to_string(), 1);
    StructuralSignature {
        symbol_id: "convert.rs::py_to_json".to_string(),
        display: "py_to_json".to_string(),
        language: "rust".to_string(),
        kind: "function".to_string(),
        file: "convert.rs".to_string(),
        line_start: 76,
        line_end: 88,
        arity: Arity {
            params: 1,
            results: 1,
            generics: 0,
        },
        cfg: Cfg {
            branch_count: 0,
            match_arms: 0,
            loop_count: 1,
            try_count: 1,
            return_points: 1,
            max_nesting: 3,
        },
        stmt_histogram: hist,
        call_sequence: vec![
            "with_capacity".to_string(),
            "iter".to_string(),
            "push".to_string(),
        ],
        type_shape: TypeShape {
            params: vec!["&_".to_string()],
            result: "Result<_,_>".to_string(),
        },
        shingles: vec![1, 2, 3, 4],
        token_len: 20,
        features: BTreeMap::new(),
    }
}

#[test]
fn itertools_matches_a_fallible_accumulation_loop() {
    let out = match_libraries(
        &[accumulating_loop_sig()],
        &[],
        &[],
        &params_for("itertools", 0.9),
    );
    assert_eq!(out.len(), 1);
    let f = &out[0];
    assert_eq!(f.external.library, "itertools");
    assert_eq!(f.external.item, "process_results");
    assert_eq!(f.external.kind, ExternalKind::Function);
    assert!(f.match_basis[0].contains("accumulates with `?`"));
    // confidence = 0.9 * 0.7 * 0.7 = 0.441
    assert!(
        (f.confidence - 0.44).abs() < 0.02,
        "confidence {}",
        f.confidence
    );
    // honest that the ? may not be inside the loop
    assert!(f
        .counter_evidence
        .iter()
        .any(|c| c.contains("may not sit inside the loop")));
}

#[test]
fn itertools_ignores_a_plain_loop_without_try() {
    let mut sig = accumulating_loop_sig();
    sig.cfg.try_count = 0;
    let out = match_libraries(&[sig], &[], &[], &params_for("itertools", 0.9));
    assert!(
        out.is_empty(),
        "a loop with no `?` is not a fallible accumulation"
    );
}

#[test]
fn tier_a_virtual_signature_matches_by_shape() {
    // A rustdoc-sourced virtual signature: a generic fn `(_ , F) -> Result<_,_>`.
    let vsig = VirtualSignature {
        external: ExternalRef {
            library: "itertools".to_string(),
            item: "process_results".to_string(),
            kind: ExternalKind::Function,
            source: ExternalSource::Rustdoc,
            version: Some("0.13".to_string()),
        },
        trust: 0.9,
        match_mode: MatchMode::Signature,
        signature: {
            let mut s = accumulating_loop_sig();
            s.type_shape = TypeShape {
                params: vec!["_".to_string(), "_".to_string()],
                result: "Result<_,_>".to_string(),
            };
            s.arity = Arity {
                params: 2,
                results: 1,
                generics: 3,
            };
            s
        },
        eliminates: "a manual fallible fold".to_string(),
    };
    // A local fn with the same result shape and two params.
    let mut local = accumulating_loop_sig();
    local.type_shape = TypeShape {
        params: vec!["_".to_string(), "_".to_string()],
        result: "Result<_,_>".to_string(),
    };
    local.arity = Arity {
        params: 2,
        results: 1,
        generics: 1,
    };
    let params = LibraryParams {
        curated_crates: BTreeMap::new(),
        min_virtual_match: 0.6,
    };
    let out = match_libraries(&[local], &[], &[vsig], &params);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].external.source, ExternalSource::Rustdoc);
    // rustdoc profile is present and honest about signature-only visibility
    assert!(out[0]
        .profile
        .limitations
        .iter()
        .any(|l| l.contains("Signature-shape only")));
}

#[test]
fn inactive_crate_yields_nothing() {
    // thiserror facts, but the config activated only derive_more → no finding.
    let out = match_libraries(
        &[],
        &[prompt_error_facts()],
        &[],
        &params_for("derive_more", 0.9),
    );
    assert!(
        out.is_empty(),
        "an inactive crate must produce no findings (fail-closed)"
    );
}

#[test]
fn empty_inputs_yield_nothing() {
    let out = match_libraries(&[], &[], &[], &LibraryParams::default());
    assert!(out.is_empty());
}
