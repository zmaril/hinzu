//! The hinzu engine. The CLI is a thin shell over this crate; the real work
//! lands here so it stays testable without going through argv.
//!
//! Slice 0 (this prototype) proves the *language-independent* core end to end:
//! a fact schema ([`facts`]), a propagation engine ([`effects`]), and a policy
//! check ([`policy`]), exercised on a synthetic fact set. No adapter, no
//! external toolchain — the Rust (SCIP) and TypeScript (compiler-API) adapters
//! that feed real facts in are slice 1 and slice 2. See
//! `notes/getting-started.md`.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::Result;

pub mod effects;
pub mod facts;
pub mod policy;
pub mod roots;
pub mod store;

/// Shared builders for the unit tests across the engine's modules, kept in one
/// place so the `Edge` construction isn't copy-pasted per test module.
#[cfg(test)]
pub(crate) mod test_support {
    use crate::facts::Edge;

    /// A `Call` edge from `caller` to `callee` with placeholder provenance.
    pub(crate) fn edge(caller: &str, callee: &str) -> Edge {
        Edge::call(caller, callee, "x.rs", 1)
    }
}

use effects::{EffectEngine, NaiveEngine};
use facts::{Definition, Edge, Effect, EffectRoot, FactSet, Language};
use policy::{check, Policy};

/// Engine entry point. Builds a synthetic demo fact set that mirrors a
/// functional-core violation — an in-core function that reaches the filesystem
/// through an adapter — runs the propagation engine and the policy check over
/// it, and returns a human-readable report.
pub fn run() -> Result<String> {
    let facts = demo_facts();
    let summaries = NaiveEngine.propagate(&facts);
    let policy = demo_policy()?;
    let violations = check(&facts, &summaries, &policy);
    format_report(
        "hinzu effect analysis (demo)",
        &facts,
        &summaries,
        &violations,
    )
}

/// The outcome of a `hinzu check`: the human-readable report and the number of
/// policy violations found, so a caller can set a non-zero exit code.
pub struct CheckOutcome {
    pub report: String,
    pub violations: usize,
}

/// Run the full check pipeline over a fact set: persist the facts to the store,
/// propagate effects with `engine`, persist the derived summaries, check them
/// against the policy, and format the report. When `db` is `None` the store is
/// in-memory (the facts and summaries are not kept after the run). The engine
/// is caller-chosen (`NaiveEngine` reference, or the DBSP engine) so the store
/// and report never learn which one ran.
pub fn check_facts(
    facts: FactSet,
    db: Option<&Path>,
    policy: &Policy,
    engine: &dyn EffectEngine,
) -> Result<CheckOutcome> {
    let mut store = match db {
        Some(path) => store::Store::open(path)?,
        None => store::Store::open_in_memory()?,
    };
    store.insert_facts(&facts)?;

    // Load the facts back from the store so the analysis runs on exactly what
    // was persisted — the same path a re-run over an existing `--db` takes.
    let facts = store.load_facts()?;
    let summaries = engine.propagate(&facts);
    store.write_summaries(&summaries)?;

    let violations = check(&facts, &summaries, policy);
    let report = format_report("hinzu effect analysis", &facts, &summaries, &violations)?;
    // Only errors fail the run. `on_unknown = "warn"` produces reported-but-
    // non-failing warnings, so the count that drives the exit code is the
    // number of error-severity findings.
    let errors = violations.iter().filter(|v| v.is_error()).count();
    Ok(CheckOutcome {
        report,
        violations: errors,
    })
}

/// Format the effect summaries and any policy violations into the report both
/// `run` and `check_facts` print — one place so the layout stays consistent.
fn format_report(
    title: &str,
    facts: &FactSet,
    summaries: &std::collections::BTreeMap<facts::SymbolId, effects::EffectSummary>,
    violations: &[policy::Violation],
) -> Result<String> {
    let mut out = String::new();
    writeln!(out, "{title}")?;
    writeln!(out)?;
    writeln!(out, "effect summaries:")?;
    for def in facts.defs.values() {
        let summary = summaries.get(&def.id);
        let effects: Vec<&str> = summary
            .map(|s| s.effects.iter().map(Effect::as_str).collect())
            .unwrap_or_default();
        let rendered = if effects.is_empty() {
            "pure".to_string()
        } else {
            effects.join(", ")
        };
        writeln!(out, "  {} [{}]: {}", def.display, def.file, rendered)?;
    }

    writeln!(out)?;
    let errors: Vec<&policy::Violation> = violations.iter().filter(|v| v.is_error()).collect();
    let warnings: Vec<&policy::Violation> = violations.iter().filter(|v| !v.is_error()).collect();

    if errors.is_empty() {
        writeln!(out, "policy: no violations")?;
    } else {
        writeln!(out, "policy violations ({}):", errors.len())?;
        for v in &errors {
            writeln!(out, "  {}", describe_violation(v))?;
        }
    }
    if !warnings.is_empty() {
        writeln!(out)?;
        writeln!(out, "warnings ({}):", warnings.len())?;
        for v in &warnings {
            writeln!(out, "  {}", describe_violation(v))?;
        }
    }

    Ok(out)
}

/// One line describing a finding, distinguishing a forbidden-effect violation
/// from an unknown-external one so the two never read the same.
fn describe_violation(v: &policy::Violation) -> String {
    use policy::{Finding, UnknownFlavor};
    match &v.finding {
        Finding::ForbiddenEffect => format!(
            "{} forbids {} in region '{}': {}",
            v.display,
            v.effect.as_str(),
            v.region,
            v.evidence.join(" -> "),
        ),
        Finding::Unknown { callee, flavor } => {
            let what = match flavor {
                UnknownFlavor::Effect => format!("unknown external `{callee}`"),
                UnknownFlavor::Target => "an unresolved call target".to_string(),
            };
            format!(
                "{} cannot certify in region '{}': reaches {} — {}",
                v.display,
                v.region,
                what,
                v.evidence.join(" -> "),
            )
        }
    }
}

/// The synthetic scenario: `handle_request` (in the functional core) calls
/// `load_file` (an adapter), which calls `std::fs::read_to_string` (an fs
/// root). `parse_config` is pure. The policy forbids fs/net/process in the
/// core file, so `handle_request` violates transitively while `load_file`
/// (living in adapters) does not.
fn demo_facts() -> FactSet {
    let mut facts = FactSet::default();

    let core_file = "crates/hinzu-core/src/core.rs";
    let adapter_file = "crates/hinzu-core/src/adapters/io.rs";

    facts.add_def(Definition {
        id: "crate::core::parse_config".to_string(),
        display: "parse_config".to_string(),
        language: Language::Rust,
        file: core_file.to_string(),
        line_start: 1,
        line_end: 8,
    });
    facts.add_def(Definition {
        id: "crate::core::handle_request".to_string(),
        display: "handle_request".to_string(),
        language: Language::Rust,
        file: core_file.to_string(),
        line_start: 10,
        line_end: 20,
    });
    facts.add_def(Definition {
        id: "crate::io::load_file".to_string(),
        display: "load_file".to_string(),
        language: Language::Rust,
        file: adapter_file.to_string(),
        line_start: 1,
        line_end: 6,
    });

    facts.add_edge(Edge::call(
        "crate::core::handle_request",
        "crate::io::load_file",
        core_file,
        14,
    ));
    facts.add_edge(Edge::call(
        "crate::io::load_file",
        "std::fs::read_to_string",
        adapter_file,
        3,
    ));
    // parse_config has no outgoing edges — it stays pure.

    facts.add_root(EffectRoot {
        symbol: "std::fs::read_to_string".to_string(),
        effect: Effect::Fs,
    });

    facts
}

/// The demo policy: the core tree forbids fs/net/process, the adapters
/// carve-out allows them. Parsed from the same `hinzu.toml` shape the CLI reads.
fn demo_policy() -> Result<Policy> {
    Policy::from_toml(
        r#"
[region.core]
paths  = ["crates/*/src/**"]
forbid = ["fs", "net", "process"]

[region.adapters]
paths = ["crates/*/src/adapters/**"]
allow = ["fs", "net", "process", "env"]
"#,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_returns_a_message() {
        let report = run().unwrap();
        assert!(report.contains("effect"));
        // The demo must surface the functional-core violation.
        assert!(report.contains("violation"));
        assert!(report.contains("handle_request"));
    }

    #[test]
    fn demo_flags_handle_request_but_not_load_file_or_parse_config() {
        let facts = demo_facts();
        let summaries = NaiveEngine.propagate(&facts);
        let violations = check(&facts, &summaries, &demo_policy().unwrap());

        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].display, "handle_request");
        assert_eq!(violations[0].effect, Effect::Fs);
        // Evidence path threads core -> adapter -> fs root.
        assert_eq!(
            violations[0].evidence,
            vec![
                "crate::core::handle_request".to_string(),
                "crate::io::load_file".to_string(),
                "std::fs::read_to_string".to_string(),
            ]
        );

        // parse_config is pure.
        let parse = summaries.get("crate::core::parse_config").cloned();
        assert!(parse.map(|s| s.effects.is_empty()).unwrap_or(true));
    }

    /// The performance use case: a region that forbids `alloc` flags a function
    /// that pushes onto a `Vec`, with the evidence path down to the allocation.
    #[test]
    fn alloc_forbidding_region_flags_a_vec_push() {
        use crate::roots::RootSeeds;

        let mut facts = FactSet::default();
        facts.add_def(Definition {
            id: "app::hot_loop".to_string(),
            display: "hot_loop".to_string(),
            language: Language::Rust,
            file: "src/hot.rs".to_string(),
            line_start: 1,
            line_end: 4,
        });
        // The call the driver emits for `v.push(i)` — a no-body std leaf.
        facts.add_edge(Edge::call(
            "app::hot_loop",
            "std::vec::Vec::<usize>::push",
            "src/hot.rs",
            2,
        ));
        // Seed roots from the shipped std annotations: Vec::push -> alloc.
        RootSeeds::default().seed(&mut facts);

        let summaries = NaiveEngine.propagate(&facts);
        assert!(summaries["app::hot_loop"].effects.contains(&Effect::Alloc));

        let policy =
            Policy::from_toml("[region.hot]\npaths = [\"src/**\"]\nforbid = [\"alloc\"]\n")
                .unwrap();
        let violations = check(&facts, &summaries, &policy);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].effect, Effect::Alloc);
        assert_eq!(
            violations[0].evidence,
            vec![
                "app::hot_loop".to_string(),
                "std::vec::Vec::<usize>::push".to_string(),
            ]
        );
    }
}
