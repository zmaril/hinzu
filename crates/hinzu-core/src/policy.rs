//! The policy check: fail any callable that can reach a forbidden effect from a
//! region that forbids it. Regions match on source provenance, so the policy
//! lives outside the source, not in annotations.

use std::collections::BTreeMap;

use crate::effects::EffectSummary;
use crate::facts::{Effect, FactSet, SymbolId};

/// An architectural region: a set of path prefixes and the effects forbidden
/// within them.
#[derive(Clone, Debug)]
pub struct Region {
    pub name: String,
    pub path_prefixes: Vec<String>,
    pub forbid: Vec<Effect>,
}

/// The full policy — a set of regions.
#[derive(Clone, Debug, Default)]
pub struct Policy {
    pub regions: Vec<Region>,
}

/// A callable that reaches a forbidden effect from a forbidding region, with
/// the evidence path that explains why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    pub symbol: SymbolId,
    pub display: String,
    pub file: String,
    pub region: String,
    pub effect: Effect,
    pub evidence: Vec<SymbolId>,
}

/// Check every definition's effect summary against the policy.
///
/// A definition violates when it sits in a region (its `file` is prefixed by
/// one of the region's `path_prefixes`) whose forbidden effect appears in the
/// definition's summary. Prototype matching is `starts_with`; slice 1 upgrades
/// these prefixes to real globs.
pub fn check(
    facts: &FactSet,
    summaries: &BTreeMap<SymbolId, EffectSummary>,
    policy: &Policy,
) -> Vec<Violation> {
    let mut violations = Vec::new();

    for def in facts.defs.values() {
        let Some(summary) = summaries.get(&def.id) else {
            continue;
        };

        for region in &policy.regions {
            let in_region = region
                .path_prefixes
                .iter()
                .any(|prefix| def.file.starts_with(prefix.as_str()));
            if !in_region {
                continue;
            }

            for effect in &region.forbid {
                if summary.effects.contains(effect) {
                    violations.push(Violation {
                        symbol: def.id.clone(),
                        display: def.display.clone(),
                        file: def.file.clone(),
                        region: region.name.clone(),
                        effect: *effect,
                        evidence: summary.evidence.get(effect).cloned().unwrap_or_default(),
                    });
                }
            }
        }
    }

    violations
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::propagate;
    use crate::facts::{Definition, EffectRoot, Language};
    use crate::test_support::edge as call;

    fn def(id: &str, file: &str) -> Definition {
        Definition {
            id: id.to_string(),
            display: id.to_string(),
            language: Language::Rust,
            file: file.to_string(),
            line_start: 1,
            line_end: 5,
        }
    }

    #[test]
    fn forbidden_region_reaching_fs_root_produces_one_violation() {
        let mut facts = FactSet::default();
        facts.add_def(def("core_fn", "crates/core/src/core.rs"));
        facts.add_def(def("adapter_fn", "crates/core/src/adapters/io.rs"));
        facts.add_edge(call("core_fn", "adapter_fn"));
        facts.add_edge(call("adapter_fn", "std::fs::read"));
        facts.add_root(EffectRoot {
            symbol: "std::fs::read".to_string(),
            effect: Effect::Fs,
        });

        let summaries = propagate(&facts);
        let policy = Policy {
            regions: vec![Region {
                name: "core".to_string(),
                path_prefixes: vec!["crates/core/src/core.rs".to_string()],
                forbid: vec![Effect::Fs, Effect::Net, Effect::Process],
            }],
        };

        let violations = check(&facts, &summaries, &policy);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].symbol, "core_fn");
        assert_eq!(violations[0].effect, Effect::Fs);
        assert_eq!(violations[0].region, "core");
        // adapter_fn lives outside the forbidden region -> no violation.
    }
}
