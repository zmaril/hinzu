// straitjacket-allow-file[:duplication] — this is the Python twin of
// `ts_check.rs`: the two per-language integration harnesses are deliberately
// parallel (the same repo-root helper, the same stable-facts-then-live-adapter
// pair, the same violation assertions) so a reader can compare a language's
// coverage line for line. The shared shell-out/parse code already lives once in
// `adapter_harness.rs`; what overlaps here is the test scaffolding, whose
// parallelism is the point.
//! Integration coverage for the Python path of `hinzu check`.
//!
//! The first test is stable-CI-safe: it feeds pre-extracted Python facts
//! (`adapters/python/tests/sample-facts.json`, committed) through the same
//! engine and policy as Rust and TypeScript, with no Python toolchain required —
//! proving the shared pipeline ingests Python facts and the language-aware root
//! seeding (`python.toml`) resolves the stdlib effects. The second test runs the
//! live ty adapter and is `#[ignore]`d so it stays off the stable job; the
//! `py-check` CI job (and a local `cargo test -- --ignored`) exercises it with
//! Python and the `ty` binary present.

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

fn adapter_tests() -> PathBuf {
    repo_root().join("adapters/python/tests")
}

/// The fixture's functional-core policy flags two core functions: one reaching
/// the filesystem (`load_and_summarize`) and one spawning a subprocess
/// (`build_and_report`), each through the adapter layer, with the evidence path
/// down to the stdlib root.
fn assert_fixture_violations(stdout: &str) {
    assert!(
        stdout.contains("policy violations (2)"),
        "report was:\n{stdout}"
    );
    assert!(
        stdout.contains("load_and_summarize forbids fs in region 'core'"),
        "report was:\n{stdout}"
    );
    assert!(
        stdout.contains("core.py#load_and_summarize -> effects.py#read_config -> builtins::open"),
        "report was:\n{stdout}"
    );
    assert!(
        stdout.contains("build_and_report forbids process in region 'core'"),
        "report was:\n{stdout}"
    );
    assert!(
        stdout.contains("core.py#build_and_report -> effects.py#run_tool -> subprocess::run"),
        "report was:\n{stdout}"
    );
    // read_config / run_tool live in the effects.py carve-out, so they are not
    // flagged.
    assert!(
        !stdout.contains("read_config forbids"),
        "report was:\n{stdout}"
    );
}

/// Pre-extracted Python facts run through the shared engine and the
/// functional-core policy, no Python required. This is the stable-CI coverage.
#[test]
fn check_python_facts_reports_the_core_violations() {
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
    assert_fixture_violations(&out);
}

/// The live adapter path: extract facts from the Python fixture with the ty
/// adapter, then run the full pipeline. Ignored by default so the stable Rust job
/// (no Python/ty) stays green; the `py-check` job runs it.
#[test]
#[ignore = "needs python3 and the `ty` binary"]
fn check_python_project_end_to_end() {
    let fixture = repo_root().join("adapters/python/tests/fixture");
    let mut cmd = Command::cargo_bin("hinzu").unwrap();
    let assert = cmd
        .arg("check")
        .arg(&fixture)
        .arg("--policy")
        .arg(fixture.join("hinzu.toml"))
        .assert()
        .failure();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert_fixture_violations(&out);
}

/// The reference-edge rung's payoff, asserted on both violations it recovers that
/// call-only never saw. The higher-order reference: `schedule_audit` passes the
/// filesystem function `write_audit` as a value, so it reaches `fs`, with the
/// evidence path down to `builtins::open`. The module-level (import-time) database
/// effect: `models.py` builds a SQLAlchemy engine at module scope, attributed to
/// that file's synthetic `<module>` node, reaching `db`. Under call-only both files
/// were pure — `schedule_audit` never saw the value it passed and module scope
/// emitted no edges at all — so this is exactly the loop the SQLAlchemy annotation
/// pack flagged as latent behind call-only.
fn assert_reference_fixture_violations(stdout: &str) {
    assert!(
        stdout.contains("policy violations (2)"),
        "report was:\n{stdout}"
    );
    // Higher-order reference: a passed-as-value effectful function reaches fs.
    assert!(
        stdout.contains("schedule_audit forbids fs in region 'core'"),
        "report was:\n{stdout}"
    );
    assert!(
        stdout.contains("core.py#schedule_audit -> effects.py#write_audit -> builtins::open"),
        "report was:\n{stdout}"
    );
    // Module-level SQLAlchemy: import-time `create_engine` surfaces db on the
    // file's `<module>` node.
    assert!(
        stdout.contains("<module> forbids db in region 'core'"),
        "report was:\n{stdout}"
    );
    assert!(
        stdout.contains("<module>@models.py -> sqlalchemy"),
        "report was:\n{stdout}"
    );
    // The genuinely-pure core function and the adapter carve-out are not flagged.
    assert!(
        !stdout.contains("pure_total forbids"),
        "report was:\n{stdout}"
    );
    assert!(
        !stdout.contains("write_audit forbids"),
        "report was:\n{stdout}"
    );
}

/// Stable-CI coverage for the reference rung: pre-extracted facts
/// (`reference-fixture-facts.json`, committed) run through the shared engine and
/// the reference-fixture policy, no Python/ty/SQLAlchemy required. Proves the
/// higher-order `reference` edge and the module-level `<module>` node carry their
/// effects through propagation and the policy check.
#[test]
fn check_python_reference_facts_reports_higher_order_and_module_level() {
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

/// The live reference path: extract facts from the reference fixture with the ty
/// adapter (which runs the tree-sitter reference pass), then run the full
/// pipeline. Ignored by default; needs `ty` AND an installed SQLAlchemy for ty to
/// resolve `create_engine` into `sqlalchemy`. Not wired into the `--exact`
/// py-check CI invocation, so it stays a local reproduction of the committed
/// facts above.
#[test]
#[ignore = "needs python3, the `ty` binary, and an installed sqlalchemy"]
fn check_python_reference_project_end_to_end() {
    let fixture = repo_root().join("adapters/python/tests/reference-fixture");
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
