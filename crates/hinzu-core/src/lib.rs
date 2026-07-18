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

use anyhow::Result;

pub mod effects;
pub mod facts;
pub mod policy;

use effects::propagate;
use facts::{Definition, Edge, EdgeKind, Effect, EffectRoot, FactSet, Language};
use policy::{check, Policy, Region};

/// Engine entry point. Builds a synthetic demo fact set that mirrors a
/// functional-core violation — an in-core function that reaches the filesystem
/// through an adapter — runs the propagation engine and the policy check over
/// it, and returns a human-readable report.
pub fn run() -> Result<String> {
    let facts = demo_facts();
    let summaries = propagate(&facts);
    let policy = demo_policy();
    let violations = check(&facts, &summaries, &policy);

    let mut out = String::new();
    writeln!(out, "hinzu effect analysis (demo)")?;
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
    if violations.is_empty() {
        writeln!(out, "policy: no violations")?;
    } else {
        writeln!(out, "policy violations ({}):", violations.len())?;
        for v in &violations {
            writeln!(
                out,
                "  {} forbids {} in region '{}': {}",
                v.display,
                v.effect.as_str(),
                v.region,
                v.evidence.join(" -> "),
            )?;
        }
    }

    Ok(out)
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

    facts.add_edge(Edge {
        caller: "crate::core::handle_request".to_string(),
        callee: "crate::io::load_file".to_string(),
        kind: EdgeKind::Call,
        evidence_file: core_file.to_string(),
        evidence_line: 14,
    });
    facts.add_edge(Edge {
        caller: "crate::io::load_file".to_string(),
        callee: "std::fs::read_to_string".to_string(),
        kind: EdgeKind::Call,
        evidence_file: adapter_file.to_string(),
        evidence_line: 3,
    });
    // parse_config has no outgoing edges — it stays pure.

    facts.add_root(EffectRoot {
        symbol: "std::fs::read_to_string".to_string(),
        effect: Effect::Fs,
    });

    facts
}

/// One region: the core file forbids fs/net/process, however deep the chain.
fn demo_policy() -> Policy {
    Policy {
        regions: vec![Region {
            name: "core".to_string(),
            path_prefixes: vec!["crates/hinzu-core/src/core.rs".to_string()],
            forbid: vec![Effect::Fs, Effect::Net, Effect::Process],
        }],
    }
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
        let summaries = propagate(&facts);
        let violations = check(&facts, &summaries, &demo_policy());

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
}
