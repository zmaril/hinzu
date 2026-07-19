//! Integration coverage for the TypeScript path of `hinzu check`.
//!
//! The first test is stable-CI-safe: it feeds pre-extracted TypeScript facts
//! (`adapters/typescript/tests/sample-facts.json`, committed) through the same
//! engine and policy as Rust, with no Node toolchain required — proving the
//! shared pipeline ingests TypeScript facts and the language-aware root seeding
//! (`node.toml`) resolves the Node built-in. The second test runs the live Node
//! adapter and is `#[ignore]`d so it stays off the stable job; the `ts-check` CI
//! job (and a local `cargo test -- --ignored`) exercises it with Node present.

use std::path::PathBuf;

use assert_cmd::Command;

/// The repo root, three parents up from this crate's manifest
/// (`crates/hinzu-cli`).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .to_path_buf()
}

fn adapter_tests() -> PathBuf {
    repo_root().join("adapters/typescript/tests")
}

/// The fixture's functional-core policy flags `loadAndSummarize` — a core
/// function that reaches the filesystem through the adapter — with the evidence
/// path down to the `node:fs` root.
fn assert_fixture_violation(stdout: &str) {
    assert!(
        stdout.contains("policy violations (1)"),
        "report was:\n{stdout}"
    );
    assert!(
        stdout.contains("loadAndSummarize forbids fs in region 'core'"),
        "report was:\n{stdout}"
    );
    assert!(
        stdout.contains("src/core#loadAndSummarize -> src/io#readConfig -> node:fs::readFileSync"),
        "report was:\n{stdout}"
    );
    // readConfig lives in the io carve-out, so it is not flagged.
    assert!(
        !stdout.contains("readConfig forbids"),
        "report was:\n{stdout}"
    );
}

/// Pre-extracted TypeScript facts run through the shared engine and the
/// functional-core policy, no Node required. This is the stable-CI coverage.
#[test]
fn check_typescript_facts_reports_the_core_violation() {
    let tests = adapter_tests();
    let mut cmd = Command::cargo_bin("hinzu").unwrap();
    let assert = cmd
        .arg("check")
        .arg(".")
        .arg("--facts")
        .arg(tests.join("sample-facts.json"))
        .arg("--policy")
        .arg(tests.join("fixture/hinzu.toml"))
        .assert()
        .failure();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert_fixture_violation(&out);
}

/// The live adapter path: extract facts from the TypeScript fixture with the
/// Node compiler-API adapter, then run the full pipeline. Ignored by default so
/// the stable Rust job (no Node) stays green; the `ts-check` job runs it.
#[test]
#[ignore = "needs Node and the adapter's npm install"]
fn check_typescript_project_end_to_end() {
    let fixture = repo_root().join("adapters/typescript/tests/fixture");
    let mut cmd = Command::cargo_bin("hinzu").unwrap();
    let assert = cmd
        .arg("check")
        .arg(&fixture)
        .arg("--policy")
        .arg(fixture.join("hinzu.toml"))
        .assert()
        .failure();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert_fixture_violation(&out);
}

/// The reference-edge rung's payoff, asserted on both violations it recovers that
/// call-only never saw. The higher-order reference: `scheduleAudit` passes the
/// filesystem function `readFile` (from `node:fs`) as a value, so it reaches
/// `fs`, with the evidence path down to `node:fs::readFile`. The module-level
/// (import-time) network effect: `models.ts` issues a `fetch` at module scope,
/// attributed to that file's synthetic `<module>` node, reaching `net`. Under
/// call-only both were pure — `scheduleAudit` never saw the value it passed and
/// module scope emitted no edges at all — so this is the higher-order and
/// import-time surface the tsc call resolver could not reach.
fn assert_reference_fixture_violations(stdout: &str) {
    assert!(
        stdout.contains("policy violations (2)"),
        "report was:\n{stdout}"
    );
    // Higher-order reference: a passed-as-value effectful function reaches fs.
    assert!(
        stdout.contains("scheduleAudit forbids fs in region 'core'"),
        "report was:\n{stdout}"
    );
    assert!(
        stdout.contains("src/core#scheduleAudit -> node:fs::readFile"),
        "report was:\n{stdout}"
    );
    // Module-level import-time network: `fetch` surfaces net on the file's
    // `<module>` node.
    assert!(
        stdout.contains("<module> forbids net in region 'core'"),
        "report was:\n{stdout}"
    );
    assert!(
        stdout.contains("<module>@src/models.ts -> global::fetch"),
        "report was:\n{stdout}"
    );
    // The genuinely-pure core functions are not flagged.
    assert!(
        !stdout.contains("pureTotal forbids"),
        "report was:\n{stdout}"
    );
    assert!(
        !stdout.contains("register forbids"),
        "report was:\n{stdout}"
    );
}

/// Stable-CI coverage for the reference rung: pre-extracted facts
/// (`reference-fixture-facts.json`, committed) run through the shared engine and
/// the reference-fixture policy, no Node required. Proves the higher-order
/// `reference` edge and the module-level `<module>` node carry their effects
/// through propagation and the policy check.
#[test]
fn check_typescript_reference_facts_reports_higher_order_and_module_level() {
    let tests = adapter_tests();
    let mut cmd = Command::cargo_bin("hinzu").unwrap();
    let assert = cmd
        .arg("check")
        .arg(".")
        .arg("--facts")
        .arg(tests.join("reference-fixture-facts.json"))
        .arg("--policy")
        .arg(tests.join("reference-fixture/hinzu.toml"))
        .assert()
        .failure();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert_reference_fixture_violations(&out);
}

/// The live reference path: extract facts from the reference fixture with the
/// Node compiler-API adapter (which runs the reference pass), then run the full
/// pipeline. Ignored by default; the `ts-check` job runs it with Node present.
#[test]
#[ignore = "needs Node and the adapter's npm install"]
fn check_typescript_reference_project_end_to_end() {
    let fixture = repo_root().join("adapters/typescript/tests/reference-fixture");
    let mut cmd = Command::cargo_bin("hinzu").unwrap();
    let assert = cmd
        .arg("check")
        .arg(&fixture)
        .arg("--policy")
        .arg(fixture.join("hinzu.toml"))
        .assert()
        .failure();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert_reference_fixture_violations(&out);
}
