//! Integration coverage for `hinzu check` over pre-extracted facts.

use std::path::PathBuf;

use assert_cmd::Command;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Run `hinzu check .` over the fixture facts and policy with the given engine
/// (`None` for the default), expect the violation exit code, and return stdout.
/// Shared by the report and engine-agreement tests so the argv chain lives once.
fn check_fixture(engine: Option<&str>) -> String {
    let mut cmd = Command::cargo_bin("hinzu").unwrap();
    cmd.arg("check")
        .arg(".")
        .arg("--facts")
        .arg(fixture("facts.json"))
        .arg("--policy")
        .arg(fixture("policy.toml"));
    if let Some(engine) = engine {
        cmd.arg("--engine").arg(engine);
    }
    // A violation was found, so the command exits non-zero (CI-usable).
    let assert = cmd.assert().failure();
    String::from_utf8(assert.get_output().stdout.clone()).unwrap()
}

#[test]
fn check_reports_the_functional_core_violation() {
    let out = check_fixture(None);

    assert!(out.contains("policy violations (1)"), "report was:\n{out}");
    // handle_request in the core reaches fs through the adapter.
    assert!(out.contains("handle_request forbids fs in region 'core'"));
    // The evidence path threads core -> adapter -> fs root.
    assert!(out.contains(
        "crate::core::handle_request -> crate::io::load_file -> std::fs::read_to_string"
    ));
    // load_file lives in the adapters carve-out, so it is not flagged.
    assert!(!out.contains("load_file forbids"));
}

#[test]
fn check_without_facts_on_a_non_cargo_path_fails_honestly() {
    // A directory with no Cargo.toml is not extractable and no facts were
    // given, so the command fails honestly instead of faking an analysis. Using
    // a temp dir keeps this off the nightly StableMIR path, so CI stays stable.
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = Command::cargo_bin("hinzu").unwrap();
    let assert = cmd
        .arg("check")
        .arg(dir.path())
        .arg("--policy")
        .arg(fixture("policy.toml"))
        .assert()
        .failure();

    let err = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        err.contains("is not a cargo, TypeScript, Python, or Go project"),
        "stderr was:\n{err}"
    );
}

/// The reference `naive` engine produces the same violation as the default
/// `dbsp` engine over the fixture facts — the CLI-level cross-check.
#[test]
fn naive_engine_flag_agrees_with_dbsp_on_the_fixture() {
    let dbsp = check_fixture(None);
    let naive = check_fixture(Some("naive"));
    assert!(
        naive.contains("handle_request forbids fs in region 'core'"),
        "naive report was:\n{naive}"
    );
    assert_eq!(dbsp, naive, "dbsp and naive reports diverge");
}

#[test]
fn run_demo_still_works() {
    let mut cmd = Command::cargo_bin("hinzu").unwrap();
    let assert = cmd.arg("run").assert().success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(out.contains("handle_request"), "demo output was:\n{out}");
}
