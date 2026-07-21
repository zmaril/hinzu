//! Integration coverage for `hinzu ranges` — freerange-style numeric range /
//! divide-by-zero analysis.
//!
//! The stable test feeds pre-extracted body facts
//! (`tests/fixtures/ranges-demo/bodies.json`, committed — produced by the
//! `hinzu-rustc-driver` StableMIR run over the demo crate) through the pure
//! abstract-interpretation engine, with no nightly toolchain required. It proves
//! the engine catches a real integer divide-by-zero and remainder-by-zero on MIR
//! extracted from actual Rust source, and — the honesty check — does NOT flag a
//! guarded divide (`if c != 0 { .. / c }`) or a divide by a nonzero constant. The
//! live test runs the nightly driver end to end and is `#[ignore]`d so the stable
//! job stays green.

use std::path::PathBuf;

use assert_cmd::Command;

/// The repo root, two parents up from this crate's manifest
/// (`crates/hinzu-cli`).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .to_path_buf()
}

/// The demo fixture crate directory.
fn fixture_dir() -> PathBuf {
    repo_root().join("crates/hinzu-cli/tests/fixtures/ranges-demo")
}

/// Parse the JSON report from `hinzu ranges` stdout.
fn parse_report(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout).expect("ranges report is valid JSON")
}

/// Whether the report flags a hazard of `kind` in the function whose id ends
/// with `suffix`.
fn has_hazard(report: &serde_json::Value, suffix: &str, kind: &str) -> bool {
    report["hazards"].as_array().unwrap().iter().any(|h| {
        h["function"].as_str().unwrap().ends_with(suffix) && h["kind"].as_str().unwrap() == kind
    })
}

/// The heart of the analysis, asserted on the committed body facts: the two
/// unguarded operations are flagged with the right hazard kind, and the guarded
/// divide and the constant divide are not — no false positives.
fn assert_demo_report(stdout: &str) {
    let report = parse_report(stdout);

    // The unguarded divide and remainder each panic if their divisor is zero.
    assert!(
        has_hazard(&report, "::ratio", "divide-by-zero"),
        "ratio's unguarded divide should be flagged; report:\n{stdout}"
    );
    assert!(
        has_hazard(&report, "::modulo", "remainder-by-zero"),
        "modulo's unguarded remainder should be flagged; report:\n{stdout}"
    );

    // The guard `if c != 0` discharges the divisor; a divide by a nonzero
    // constant can never be zero. Neither is a hazard.
    assert!(
        !has_hazard(&report, "::ratio_guarded", "divide-by-zero"),
        "a guarded divide must not be flagged; report:\n{stdout}"
    );
    assert!(
        !has_hazard(&report, "::div_by_const", "divide-by-zero"),
        "a divide by a nonzero constant must not be flagged; report:\n{stdout}"
    );

    // Exactly the two real hazards, nothing else.
    assert_eq!(
        report["hazards"].as_array().unwrap().len(),
        2,
        "expected exactly two hazards; report:\n{stdout}"
    );

    // The divide-by-zero carries its evidence: the divisor range.
    let ratio_hazard = report["hazards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["function"].as_str().unwrap().ends_with("::ratio"))
        .unwrap();
    assert!(
        ratio_hazard["divisor_range"]
            .as_str()
            .unwrap()
            .contains("integer"),
        "the hazard should carry the divisor range as evidence; report:\n{stdout}"
    );
}

/// Pre-extracted body facts run through the pure engine, no nightly toolchain
/// required. This is the stable-CI coverage.
#[test]
fn ranges_flags_divide_by_zero_from_committed_bodies() {
    let bodies = fixture_dir().join("bodies.json");
    let mut cmd = Command::cargo_bin("hinzu").unwrap();
    // A found hazard makes the command exit non-zero (CI-gate behavior).
    let assert = cmd
        .arg("ranges")
        .arg(".")
        .arg("--bodies")
        .arg(bodies)
        .assert()
        .failure();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert_demo_report(&out);
}

/// The live driver path: extract MIR bodies from the demo crate with the
/// StableMIR driver, then run the analysis. Ignored by default so the stable job
/// (no nightly + `rustc_private`) stays green; run it locally with the driver
/// built on its pinned nightly and `HINZU_RUSTC_DRIVER` set, via
/// `cargo test -- --ignored`.
#[test]
#[ignore = "needs the nightly StableMIR driver (crates/hinzu-rustc-driver) built"]
fn ranges_analyzes_the_demo_crate_end_to_end() {
    let fixture = fixture_dir();
    let mut cmd = Command::cargo_bin("hinzu").unwrap();
    let assert = cmd.arg("ranges").arg(&fixture).assert().failure();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert_demo_report(&out);
}

/// A structural regression test over the committed driver output: the demo
/// crate's four functions are all present, guarding the body-fact schema
/// independent of the engine (a driver change that broke the schema is caught
/// even if the engine still happens to flag something).
#[test]
fn committed_bodies_cover_the_demo_functions() {
    let path = fixture_dir().join("bodies.json");
    let json = std::fs::read_to_string(path).unwrap();
    let bodies = hinzu_core::absint::body::BodyFacts::from_json(&json).unwrap();
    let ids: Vec<&str> = bodies.functions.iter().map(|f| f.id.as_str()).collect();
    for name in ["ratio", "modulo", "ratio_guarded", "div_by_const"] {
        assert!(
            ids.iter().any(|id| id.ends_with(name)),
            "expected a body for {name}, got {ids:?}"
        );
    }
}
