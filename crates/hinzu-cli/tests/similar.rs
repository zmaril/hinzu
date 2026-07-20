//! Integration coverage for `hinzu similar`, driven by a committed structural
//! fixture (`tests/fixtures/similar-fixture.json`) so the test is toolchain-free:
//! it feeds pre-extracted signatures through the pure similarity engine with no
//! live `syn` extraction, the same fixture-driven convention `--facts` tests use.
//!
//! The fixture holds two `parse_*` functions with an identical control-flow
//! skeleton and call sequence that differ only in their signature types, plus an
//! unrelated loop-shaped `sum_list`. The run must cluster the two parse functions
//! into one candidate, name it `generic_function` (types vary, everything else is
//! constant), and leave `sum_list` out.

use std::path::PathBuf;

use assert_cmd::Command;
use serde_json::Value;

/// This crate's manifest dir (`crates/hinzu-cli`).
fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture() -> PathBuf {
    crate_dir().join("tests/fixtures/similar-fixture.json")
}

fn ts_fixture() -> PathBuf {
    crate_dir().join("tests/fixtures/similar-ts-fixture.json")
}

/// Run `hinzu similar --structural <fixture>`, assert the process succeeds, and
/// parse its stdout as JSON. Shared by the two fixture-driven cases.
fn run_similar_structural(fixture: PathBuf) -> Value {
    let mut cmd = Command::cargo_bin("hinzu").unwrap();
    let assert = cmd
        .arg("similar")
        .arg("--structural")
        .arg(fixture)
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    serde_json::from_str(&out).expect("stdout is JSON")
}

/// The `--structural` fixture path: two type-varying `parse_*` functions cluster
/// into one `generic_function` candidate; the unrelated `sum_list` does not join.
#[test]
fn similar_clusters_the_type_varying_parse_functions() {
    let doc = run_similar_structural(fixture());

    assert_eq!(doc["hinzu_similarity_version"], 1);
    assert_eq!(doc["stats"]["signatures_analyzed"], 3);
    assert_eq!(doc["stats"]["candidates_found"], 1);

    // The Rust/syn profile is present and honest about being syntactic.
    let profiles = doc["profiles"].as_array().unwrap();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0]["extractor"], "syn");
    assert_eq!(profiles[0]["capabilities"]["types_resolved"], "syntactic");

    let cand = &doc["candidates"][0];
    assert_eq!(cand["id"], "cand-1");
    assert_eq!(cand["likely_abstraction"]["family"], "generic_function");

    // Exactly the two parse functions, and not sum_list.
    let members: Vec<String> = cand["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["symbol_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(members.len(), 2);
    assert!(members.iter().any(|m| m.ends_with("parse_u8")));
    assert!(members.iter().any(|m| m.ends_with("parse_u16")));
    assert!(!members.iter().any(|m| m.contains("sum_list")));

    // The differing types are surfaced as the abstraction axis, and the honest
    // "types differ" caution is present in the counter-evidence.
    let differences = cand["differences"].as_array().unwrap();
    assert!(differences
        .iter()
        .any(|d| d.as_str().unwrap().contains("type shapes vary")));
    let counter = cand["counter_evidence"].as_array().unwrap();
    assert!(counter
        .iter()
        .any(|c| c.as_str().unwrap().contains("syntactic match only")));

    // Confidence is capped below 1 by the syntactic profile.
    let confidence = cand["confidence"].as_f64().unwrap();
    assert!(confidence <= 0.85 + 1e-9, "confidence was {confidence}");
}

/// The committed TypeScript `--structural` fixture (produced by
/// `adapters/typescript/structural.mjs`, so this test needs no Node toolchain)
/// drives the same engine: two structurally near-identical async loaders cluster
/// into one candidate, and the TypeScript/`tsc-checker` profile is present and
/// honestly *type-resolved* — which lifts the confidence above the syntactic cap
/// the Rust profile is bound by, the whole point of the language-profile concept.
#[test]
fn similar_clusters_typescript_loaders_via_resolved_types() {
    let doc = run_similar_structural(ts_fixture());

    assert_eq!(doc["hinzu_similarity_version"], 1);
    assert_eq!(doc["languages"][0], "typescript");
    assert_eq!(doc["stats"]["candidates_found"], 1);

    // The TypeScript/tsc-checker profile is present and honestly type-resolved.
    let profiles = doc["profiles"].as_array().unwrap();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0]["language"], "typescript");
    assert_eq!(profiles[0]["extractor"], "tsc-checker");
    assert_eq!(profiles[0]["capabilities"]["types_resolved"], "yes");

    let cand = &doc["candidates"][0];
    let members: Vec<String> = cand["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["display"].as_str().unwrap().to_string())
        .collect();
    assert!(members.iter().any(|m| m == "loadUser"));
    assert!(members.iter().any(|m| m == "loadOrder"));
    assert!(!members.iter().any(|m| m == "sumTotals"));

    // The resolved return types (`Promise<User>` / `Promise<Order>`) erased to the
    // same shape `Promise<_>` and are cited as a shared feature.
    let shared = cand["pattern"]["shared_features"].as_array().unwrap();
    assert!(shared
        .iter()
        .any(|f| f.as_str().unwrap().contains("Promise<_>")));

    // Because the profile resolves types (`types_resolved=yes`), the confidence is
    // NOT bound by the 0.85 syntactic cap the Rust profile carries.
    let confidence = cand["confidence"].as_f64().unwrap();
    assert!(confidence > 0.85, "confidence was {confidence}");
}

/// A non-cargo path without `--structural` fails honestly rather than faking an
/// analysis.
#[test]
fn similar_without_cargo_or_structural_fails_honestly() {
    let tmp = std::env::temp_dir().join("hinzu-similar-empty");
    std::fs::create_dir_all(&tmp).unwrap();
    let mut cmd = Command::cargo_bin("hinzu").unwrap();
    let assert = cmd.arg("similar").arg(&tmp).assert().failure();
    let err = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        err.contains("neither a cargo project nor a TypeScript project"),
        "stderr was:\n{err}"
    );
}
