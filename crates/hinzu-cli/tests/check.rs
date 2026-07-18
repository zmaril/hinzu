//! Integration coverage for `hinzu check` over pre-extracted facts.

use std::path::PathBuf;

use assert_cmd::Command;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn check_reports_the_functional_core_violation() {
    let mut cmd = Command::cargo_bin("hinzu").unwrap();
    let assert = cmd
        .arg("check")
        .arg(".")
        .arg("--facts")
        .arg(fixture("facts.json"))
        .arg("--policy")
        .arg(fixture("policy.toml"))
        .assert()
        // A violation was found, so the command exits non-zero (CI-usable).
        .failure();

    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

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
fn check_without_facts_fails_honestly_instead_of_faking() {
    let mut cmd = Command::cargo_bin("hinzu").unwrap();
    let assert = cmd
        .arg("check")
        .arg(".")
        .arg("--policy")
        .arg(fixture("policy.toml"))
        .assert()
        .failure();

    let err = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        err.contains("no Rust adapter wired yet"),
        "stderr was:\n{err}"
    );
}

#[test]
fn run_demo_still_works() {
    let mut cmd = Command::cargo_bin("hinzu").unwrap();
    let assert = cmd.arg("run").assert().success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(out.contains("handle_request"), "demo output was:\n{out}");
}
