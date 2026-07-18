//! Integration coverage for the Python path of `hinzu check`.
//!
//! The first test is stable-CI-safe: it feeds pre-extracted Python facts
//! (`adapters/python/tests/sample-facts.json`, committed) through the same
//! engine and policy as Rust and TypeScript, with no Python toolchain required —
//! proving the shared pipeline ingests Python facts and the language-aware root
//! seeding (`python.toml`) resolves the stdlib effects. The second test runs the
//! live Jedi adapter and is `#[ignore]`d so it stays off the stable job; the
//! `py-check` CI job (and a local `cargo test -- --ignored`) exercises it with
//! Python and Jedi present.

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

/// The live adapter path: extract facts from the Python fixture with the Jedi
/// adapter, then run the full pipeline. Ignored by default so the stable Rust job
/// (no Python/Jedi) stays green; the `py-check` job runs it.
#[test]
#[ignore = "needs python3 and the adapter's jedi dependency"]
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
