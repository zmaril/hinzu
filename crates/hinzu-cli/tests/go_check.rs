// straitjacket-allow-file[:duplication] — this is the Go twin of `py_check.rs`
// and `ts_check.rs`: the per-language integration harnesses are deliberately
// parallel (the same repo-root helper, the same stable-facts-then-live-adapter
// pair, the same violation assertions) so a reader can compare a language's
// coverage line for line. The shared shell-out/parse code already lives once in
// `adapter_harness.rs`; what overlaps here is the test scaffolding, whose
// parallelism is the point.
//! Integration coverage for the Go path of `hinzu check`.
//!
//! The first test is stable-CI-safe: it feeds pre-extracted Go facts
//! (`adapters/go/tests/sample-facts.json`, committed) through the same engine
//! and policy as Rust, TypeScript, and Python, with no Go toolchain required —
//! proving the shared pipeline ingests Go facts and the language-aware root
//! seeding (`go.toml`) resolves the stdlib effects. The second test runs the
//! live gopls adapter and is `#[ignore]`d so it stays off the stable job; the
//! `go-check` CI job (and a local `cargo test -- --ignored`) exercises it with
//! Go and the `gopls` binary present.

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
    repo_root().join("adapters/go/tests")
}

/// The fixture's functional-core policy flags two core functions: one reaching
/// the filesystem (`LoadAndSummarize`) and one spawning a subprocess
/// (`BuildAndReport`), each through the effects adapter, with the evidence path
/// down to the stdlib root.
fn assert_fixture_violations(stdout: &str) {
    assert!(
        stdout.contains("policy violations (2)"),
        "report was:\n{stdout}"
    );
    assert!(
        stdout.contains("LoadAndSummarize forbids fs in region 'core'"),
        "report was:\n{stdout}"
    );
    assert!(
        stdout.contains(
            "core/core.go#LoadAndSummarize -> effects/effects.go#ReadConfig -> os::ReadFile"
        ),
        "report was:\n{stdout}"
    );
    assert!(
        stdout.contains("BuildAndReport forbids process in region 'core'"),
        "report was:\n{stdout}"
    );
    assert!(
        stdout.contains(
            "core/core.go#BuildAndReport -> effects/effects.go#RunTool -> os/exec::Command"
        ),
        "report was:\n{stdout}"
    );
    // ReadConfig / RunTool live in the effects/ carve-out, so they are not
    // flagged.
    assert!(
        !stdout.contains("ReadConfig forbids"),
        "report was:\n{stdout}"
    );
}

/// Pre-extracted Go facts run through the shared engine and the functional-core
/// policy, no Go toolchain required. This is the stable-CI coverage.
#[test]
fn check_go_facts_reports_the_core_violations() {
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

/// The live adapter path: extract facts from the Go fixture with gopls, then run
/// the full pipeline. Ignored by default so the stable Rust job (no Go/gopls)
/// stays green; the `go-check` job runs it.
#[test]
#[ignore = "needs the go toolchain and the `gopls` binary"]
fn check_go_project_end_to_end() {
    let fixture = repo_root().join("adapters/go/tests/fixture");
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
