//! Integration coverage for `hinzu model --emit quint` — lowering the body IR
//! into a Quint model skeleton.
//!
//! The stable test feeds the committed body facts
//! (`tests/fixtures/ranges-demo/bodies.json`, the same fixture the range
//! analysis uses) through the pure emitter via the CLI, with no nightly
//! toolchain required, and asserts the skeleton's structure: the module header,
//! one action per function, typed state vars, the generated-region markers, and
//! — the honesty check — the guarded function's `SwitchInt` surfaced as a CFG
//! `AGENT-TODO` hole rather than an invented control-flow encoding.

use std::path::PathBuf;

use assert_cmd::Command;

/// The demo fixture crate directory (repo root is two parents up from this
/// crate's manifest).
fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .join("crates/hinzu-cli/tests/fixtures/ranges-demo")
}

/// Run `hinzu model --bodies <fixture> --emit quint` and return its stdout.
fn emit_fixture_quint() -> String {
    let bodies = fixture_dir().join("bodies.json");
    let mut cmd = Command::cargo_bin("hinzu").unwrap();
    let assert = cmd
        .arg("model")
        .arg(".")
        .arg("--bodies")
        .arg(bodies)
        .arg("--emit")
        .arg("quint")
        .assert()
        .success();
    String::from_utf8(assert.get_output().stdout.clone()).unwrap()
}

#[test]
fn model_emits_a_quint_skeleton_for_the_demo_bodies() {
    let out = emit_fixture_quint();

    // The module header names the emitter and counts the functions.
    assert!(
        out.contains("module derived"),
        "missing module header;\n{out}"
    );
    assert!(
        out.contains("Lowered from 4 function bodies"),
        "header should count the four demo functions;\n{out}"
    );

    // The generated-region markers delimit the do-not-hand-edit content.
    assert!(
        out.contains("// ---- BEGIN GENERATED: state vars ----"),
        "missing state-var region marker;\n{out}"
    );
    assert!(
        out.contains("// ---- END GENERATED ----"),
        "missing END GENERATED marker;\n{out}"
    );

    // One action per demo function, keyed by the sanitized symbol id.
    for key in [
        "ranges_demo__ratio",
        "ranges_demo__modulo",
        "ranges_demo__ratio_guarded",
        "ranges_demo__div_by_const",
    ] {
        assert!(
            out.contains(&format!("action {key} = all {{")),
            "missing action for {key};\n{out}"
        );
    }

    // A typed state var for a known local: ratio's first param is an int.
    assert!(
        out.contains("var ranges_demo__ratio_l1: int"),
        "missing typed state var for a known local;\n{out}"
    );
}

#[test]
fn guarded_switchint_is_surfaced_as_a_cfg_agent_todo() {
    let out = emit_fixture_quint();

    // ratio_guarded is a six-block CFG; the emitter must NOT invent a
    // program-counter machine — it prints a CFG summary that includes the
    // SwitchInt and leaves an AGENT-TODO hole for the control-flow encoding.
    assert!(
        out.contains("// ---- CFG (6 blocks) ----"),
        "ratio_guarded's CFG should be summarized;\n{out}"
    );
    assert!(
        out.contains("SwitchInt"),
        "the guarded function's SwitchInt should appear in the CFG summary;\n{out}"
    );
    assert!(
        out.contains("AGENT-TODO: encode control flow"),
        "the CFG should carry a control-flow AGENT-TODO hole;\n{out}"
    );

    // Even so, the entry block's straight-line comparison still lowers to real
    // derived Quint content.
    assert!(
        out.contains("ranges_demo__ratio_guarded_l3' = ranges_demo__ratio_guarded_l2 != 0"),
        "block 0's guard comparison should lower to real Quint;\n{out}"
    );
}
