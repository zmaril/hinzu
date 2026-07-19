// straitjacket-allow-file[:duplication] — this is the Rust twin of `py_check.rs`
// / `ts_check.rs`: the per-language integration harnesses are deliberately
// parallel (the same repo-root helper, the same stable-facts-then-live-adapter
// pair, the same violation assertions) so a reader can compare a language's
// coverage line for line. What overlaps here is the test scaffolding, whose
// parallelism is the point.
//! Integration coverage for the native (StableMIR) Rust reference-edge rung.
//!
//! The stable test feeds pre-extracted facts
//! (`adapters/rust/tests/reference-fixture-facts.json`, committed — produced by
//! the `hinzu-rustc-driver` StableMIR run over the fixture crate) through the
//! same engine and policy as Python and TypeScript, with no nightly toolchain
//! required. It proves the reference edges the driver emits — a function passed
//! as a value, a closure handed off, a `LazyLock` import-time initializer — carry
//! their effects through propagation and the policy check. The live test runs the
//! nightly driver end to end and is `#[ignore]`d so the stable job stays green.

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

fn fixture_dir() -> PathBuf {
    repo_root().join("adapters/rust/tests/reference-fixture")
}

/// The reference rung's payoff on Rust, asserted on every higher-order effect it
/// recovers that a call-only graph never saw. Each core function reaches `fs` only
/// by handing off a function value: `schedule_audit` passes the effectful
/// `read_audit` as a callback; `defer_read` hands a closure to another function to
/// invoke; and the `BOOT_CONFIG` `LazyLock` initializer references a closure that
/// reads a file on first access (the import-time analogue). Under call-only all
/// three looked pure. The genuinely-pure `pure_total` and the fn-pointer trampoline
/// `run_with` must NOT be flagged — the rung is additive.
fn assert_reference_fixture_violations(stdout: &str) {
    assert!(
        stdout.contains("policy violations (5)"),
        "report was:\n{stdout}"
    );
    // Higher-order reference: a passed-as-value effectful function reaches fs,
    // with the evidence path down to the stdlib root.
    assert!(
        stdout.contains("schedule_audit forbids fs in region 'core'"),
        "report was:\n{stdout}"
    );
    assert!(
        stdout.contains(
            "reference_fixture::core::schedule_audit -> reference_fixture::effects::read_audit \
             -> std::fs::read_to_string::<&str>"
        ),
        "report was:\n{stdout}"
    );
    // Closure reference: a handed-off closure's body reaches fs.
    assert!(
        stdout.contains("defer_read forbids fs in region 'core'"),
        "report was:\n{stdout}"
    );
    assert!(
        stdout.contains(
            "reference_fixture::core::defer_read -> reference_fixture::core::defer_read::{closure#0} \
             -> reference_fixture::effects::read_audit"
        ),
        "report was:\n{stdout}"
    );
    // Import-time (lazy) reference: the LazyLock initializer reaches fs on first
    // access.
    assert!(
        stdout.contains("BOOT_CONFIG forbids fs in region 'core'"),
        "report was:\n{stdout}"
    );
    // The genuinely-pure core function and the pure fn-pointer trampoline are not
    // flagged — additive, not a blanket taint.
    assert!(
        !stdout.contains("pure_total forbids"),
        "report was:\n{stdout}"
    );
    assert!(
        !stdout.contains("run_with forbids"),
        "report was:\n{stdout}"
    );
}

/// Pre-extracted Rust facts run through the shared engine and the reference
/// policy, no nightly toolchain required. This is the stable-CI coverage: it
/// proves the driver's `reference` edges (fn-value, closure, import-time lazy)
/// propagate their effects and the policy check flags exactly the higher-order
/// recoveries call-only missed.
#[test]
fn check_rust_reference_facts_reports_higher_order_effects() {
    let facts = repo_root().join("adapters/rust/tests/reference-fixture-facts.json");
    let mut cmd = Command::cargo_bin("hinzu").unwrap();
    let assert = cmd
        .arg("check")
        .arg(".")
        .arg("--facts")
        .arg(facts)
        .arg("--policy")
        .arg(fixture_dir().join("hinzu.toml"))
        .assert()
        .failure();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert_reference_fixture_violations(&out);
}

/// The live driver path: extract facts from the fixture crate with the StableMIR
/// driver, then run the full pipeline. Ignored by default so the stable job (no
/// nightly + `rustc_private`) stays green; run it locally with the driver built
/// on its pinned nightly and `cargo test -- --ignored`.
#[test]
#[ignore = "needs the nightly StableMIR driver (crates/hinzu-rustc-driver) built"]
fn check_rust_reference_project_end_to_end() {
    let fixture = fixture_dir();
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

/// Structural regression tests over the committed driver output, guarding the
/// three shape invariants of the reference-edge rung directly (independent of the
/// engine, so a driver change that broke them is caught even if propagation still
/// happens to flag something). These are the stable-CI stand-in for the driver's
/// own visitor, which needs the nightly compiler to exercise.
mod driver_output {
    use super::repo_root;
    use hinzu_core::facts::{EdgeKind, FactSet};

    fn facts() -> FactSet {
        let path = repo_root().join("adapters/rust/tests/reference-fixture-facts.json");
        let json = std::fs::read_to_string(path).unwrap();
        FactSet::from_json(&json).unwrap()
    }

    fn has_edge(facts: &FactSet, caller: &str, callee: &str, kind: EdgeKind) -> bool {
        facts
            .edges
            .iter()
            .any(|e| e.caller == caller && e.callee == callee && e.kind == kind)
    }

    /// A function passed as a call argument (`run_with(read_audit)`) emits a
    /// `reference` edge from the passer to the passed function.
    #[test]
    fn fn_value_argument_emits_a_reference_edge() {
        let facts = facts();
        assert!(
            has_edge(
                &facts,
                "reference_fixture::core::schedule_audit",
                "reference_fixture::effects::read_audit",
                EdgeKind::Reference,
            ),
            "expected a reference edge schedule_audit -> read_audit"
        );
    }

    /// The callee of a `Call` is recorded once, as a `call` — never also as a
    /// `reference`. `schedule_audit` calls `run_with`, so that pair is a call and
    /// must not be duplicated as a reference; and no edge is ever both.
    #[test]
    fn call_callee_is_not_double_emitted_as_a_reference() {
        let facts = facts();
        assert!(
            has_edge(
                &facts,
                "reference_fixture::core::schedule_audit",
                "reference_fixture::core::run_with",
                EdgeKind::Call,
            ),
            "schedule_audit -> run_with should be a call edge"
        );
        assert!(
            !has_edge(
                &facts,
                "reference_fixture::core::schedule_audit",
                "reference_fixture::core::run_with",
                EdgeKind::Reference,
            ),
            "a call callee must not also be emitted as a reference"
        );
        // No (caller, callee) pair carries both a call and a reference edge.
        for e in &facts.edges {
            if e.kind == EdgeKind::Reference {
                assert!(
                    !has_edge(&facts, &e.caller, &e.callee, EdgeKind::Call),
                    "{} -> {} is both a call and a reference",
                    e.caller,
                    e.callee
                );
            }
        }
    }

    /// An import-time (`LazyLock`) initializer is attributed to the static's own
    /// id: its body references the initializer closure, whose body reaches the
    /// effectful leaf — so the static, not some caller, owns the import-time
    /// effect.
    #[test]
    fn effectful_import_time_static_is_attributed_to_the_static() {
        let facts = facts();
        assert!(
            has_edge(
                &facts,
                "reference_fixture::core::BOOT_CONFIG",
                "reference_fixture::core::BOOT_CONFIG::{closure#0}",
                EdgeKind::Reference,
            ),
            "the LazyLock static should reference its initializer closure"
        );
        assert!(
            has_edge(
                &facts,
                "reference_fixture::core::BOOT_CONFIG::{closure#0}",
                "reference_fixture::effects::read_audit",
                EdgeKind::Call,
            ),
            "the initializer closure body should reach the effectful leaf"
        );
    }
}
