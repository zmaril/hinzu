//! Integration coverage for `hinzu model --emit stateright` — lowering the body
//! IR into a Rust Stateright `Model` skeleton.
//!
//! Like the Quint integration test, this feeds the committed body facts
//! (`tests/fixtures/ranges-demo/bodies.json`, the same fixture the range analysis
//! and the Quint emitter use) through the pure emitter via the CLI, with no
//! nightly toolchain required, and asserts the skeleton's structure: the `Model`
//! impl, the `DerivedState` struct + `DerivedAction` enum, one action per
//! function, a real state mutation lowered from a divide, and — the honesty check
//! — the guarded function's `SwitchInt` surfaced as a CFG `AGENT-TODO` hole
//! rather than an invented control-flow encoding.

use std::path::PathBuf;

use assert_cmd::Command;

/// Run `hinzu model --bodies <fixture> --emit stateright` and return its stdout.
/// The fixture lives under this crate's own `tests/` tree, so it is reached
/// straight from `CARGO_MANIFEST_DIR` (the hinzu-cli crate root).
fn emit_stateright() -> String {
    let bodies =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ranges-demo/bodies.json");
    let output = Command::cargo_bin("hinzu")
        .unwrap()
        .args(["model", ".", "--bodies"])
        .arg(bodies)
        .args(["--emit", "stateright"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(output).unwrap()
}

#[test]
fn model_emits_a_stateright_skeleton_for_the_demo_bodies() {
    let out = emit_stateright();

    // The module header names the emitter and counts the functions.
    assert!(
        out.contains("hinzu model --emit stateright"),
        "missing the emitter provenance header;\n{out}"
    );
    assert!(
        out.contains("represents 4 extracted function bodies"),
        "header should count the four demo functions;\n{out}"
    );

    // The Model impl and the two generated derived types.
    assert!(
        out.contains("impl Model for DerivedModel"),
        "missing the Model impl;\n{out}"
    );
    assert!(
        out.contains("pub struct DerivedState"),
        "missing the DerivedState struct;\n{out}"
    );
    assert!(
        out.contains("pub enum DerivedAction"),
        "missing the DerivedAction enum;\n{out}"
    );

    // The generated-region markers delimit the do-not-hand-edit content.
    assert!(
        out.contains("// ---- BEGIN GENERATED: state ----"),
        "missing the state region marker;\n{out}"
    );
    assert!(
        out.contains("// ---- BEGIN GENERATED: next_state ----"),
        "missing the next_state region marker;\n{out}"
    );

    // One action variant per demo function, keyed by the sanitized symbol id, and
    // pushed in actions().
    for key in [
        "ranges_demo__ratio",
        "ranges_demo__modulo",
        "ranges_demo__ratio_guarded",
        "ranges_demo__div_by_const",
    ] {
        assert!(
            out.contains(&format!("    {key}, // derived from")),
            "missing action variant for {key};\n{out}"
        );
        assert!(
            out.contains(&format!("actions.push(DerivedAction::{key});")),
            "actions() should push {key};\n{out}"
        );
    }

    // A typed state field for a known local: ratio's first param is an i64.
    assert!(
        out.contains("pub ranges_demo__ratio_l1: i64"),
        "missing typed state field for a known local;\n{out}"
    );
}

#[test]
fn an_entry_statement_lowers_to_a_real_state_mutation() {
    let out = emit_stateright();

    // Each function's entry block is a guard comparison (the divide itself lives
    // in a later block, so it stays in the CFG summary). The entry statement
    // lowers to a real Rust `next.<field> = …;` mutation in that action's arm —
    // div_by_const compares the constant divisor `2` against `0`.
    assert!(
        out.contains("DerivedAction::ranges_demo__div_by_const => {"),
        "missing div_by_const match arm;\n{out}"
    );
    assert!(
        out.contains("next.ranges_demo__div_by_const_l2 = 2 == 0;"),
        "div_by_const's entry comparison should lower to a real state mutation;\n{out}"
    );
    // And an arithmetic operator DOES lower to its Rust symbol when it sits in an
    // entry block — the unit tests exercise a straight-line `a / b`; here the
    // next_state region carries the `==` comparison operator faithfully.
    assert!(
        out.contains("next.ranges_demo__ratio_l3 = next.ranges_demo__ratio_l2 == 0;"),
        "ratio's entry comparison should lower with the `==` operator;\n{out}"
    );
}

#[test]
fn guarded_switchint_is_surfaced_as_a_cfg_agent_todo() {
    let out = emit_stateright();

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

    // Even so, the entry block's straight-line comparison still lowers to a real
    // Rust state mutation.
    assert!(
        out.contains(
            "next.ranges_demo__ratio_guarded_l3 = next.ranges_demo__ratio_guarded_l2 != 0;"
        ),
        "block 0's guard comparison should lower to a real mutation;\n{out}"
    );
}
