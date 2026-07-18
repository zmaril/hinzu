//! The policy check: fail any callable that can reach a forbidden effect from a
//! region that forbids it. Regions match source files with globs, so the policy
//! lives outside the source (`hinzu.toml`), not in annotations.

use std::collections::BTreeMap;
use std::str::FromStr;

use anyhow::{Context, Result};
use glob::Pattern;
use serde::Deserialize;

use crate::effects::EffectSummary;
use crate::facts::{Effect, FactSet, SymbolId};

/// An architectural region: a set of path globs and the effects forbidden
/// within them. A region either forbids a set of effects or allows a set — an
/// `allow` list forbids every *other* category.
#[derive(Clone, Debug)]
pub struct Region {
    pub name: String,
    pub paths: Vec<Pattern>,
    pub forbid: Vec<Effect>,
}

impl Region {
    /// Whether `file` falls in this region (matches any of its path globs).
    pub fn matches(&self, file: &str) -> bool {
        self.specificity(file).is_some()
    }

    /// How specifically this region claims `file`: the length of the longest of
    /// its globs that matches, or `None` if none match. A nested carve-out like
    /// `crates/*/src/adapters/**` scores higher than the broader
    /// `crates/*/src/**`, so the carve-out governs files it covers.
    fn specificity(&self, file: &str) -> Option<usize> {
        self.paths
            .iter()
            .filter(|p| p.matches(file))
            .map(|p| p.as_str().len())
            .max()
    }
}

/// The full policy — regions plus the globs that exclude files entirely.
#[derive(Clone, Debug, Default)]
pub struct Policy {
    pub regions: Vec<Region>,
    pub ignore: Vec<Pattern>,
}

impl Policy {
    /// Parse a policy from a `hinzu.toml` string.
    pub fn from_toml(src: &str) -> Result<Self> {
        let doc: PolicyDoc = toml::from_str(src).context("parsing hinzu.toml")?;
        doc.into_policy()
    }

    /// Whether a file is excluded from analysis by the `[analysis] ignore` globs.
    pub fn is_ignored(&self, file: &str) -> bool {
        self.ignore.iter().any(|p| p.matches(file))
    }

    /// The region(s) that govern `file`: those tied for the most-specific glob
    /// match. Usually one; ties (equally specific globs) are all returned so an
    /// ambiguous overlap is reported rather than silently resolved.
    pub fn governing_regions(&self, file: &str) -> Vec<&Region> {
        let best = self
            .regions
            .iter()
            .filter_map(|r| r.specificity(file))
            .max();
        match best {
            Some(best) => self
                .regions
                .iter()
                .filter(|r| r.specificity(file) == Some(best))
                .collect(),
            None => Vec::new(),
        }
    }
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
/// Each definition is governed by the single most-specific region whose globs
/// match its file (a nested `adapters` carve-out overrides the broader `core`),
/// unless the policy's `ignore` globs exclude the file. The definition violates
/// when that region forbids an effect its summary reaches.
pub fn check(
    facts: &FactSet,
    summaries: &BTreeMap<SymbolId, EffectSummary>,
    policy: &Policy,
) -> Vec<Violation> {
    let mut violations = Vec::new();

    for def in facts.defs.values() {
        if policy.is_ignored(&def.file) {
            continue;
        }
        let Some(summary) = summaries.get(&def.id) else {
            continue;
        };

        for region in policy.governing_regions(&def.file) {
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

/// Every effect category, in policy-file order — the universe an `allow` list
/// is subtracted from to derive what a region forbids.
const ALL_EFFECTS: [Effect; 7] = [
    Effect::Fs,
    Effect::Net,
    Effect::Db,
    Effect::Clock,
    Effect::Random,
    Effect::Process,
    Effect::Env,
];

// --- the `hinzu.toml` wire shape ---------------------------------------------

#[derive(Deserialize)]
struct PolicyDoc {
    #[serde(default)]
    analysis: AnalysisDoc,
    #[serde(default)]
    region: BTreeMap<String, RegionDoc>,
}

#[derive(Default, Deserialize)]
struct AnalysisDoc {
    #[serde(default)]
    #[allow(dead_code)]
    confidence_threshold: Option<String>,
    #[serde(default)]
    ignore: Vec<String>,
}

#[derive(Deserialize)]
struct RegionDoc {
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    forbid: Vec<String>,
    #[serde(default)]
    allow: Vec<String>,
}

impl PolicyDoc {
    fn into_policy(self) -> Result<Policy> {
        let ignore = compile_globs(&self.analysis.ignore)?;

        let mut regions = Vec::with_capacity(self.region.len());
        for (name, region) in self.region {
            let paths = compile_globs(&region.paths)?;
            let forbid = region.forbidden_effects(&name)?;
            regions.push(Region {
                name,
                paths,
                forbid,
            });
        }

        Ok(Policy { regions, ignore })
    }
}

impl RegionDoc {
    /// The effects this region forbids: its `forbid` list directly, or — for an
    /// `allow`-list region — every category *not* allowed. Setting both is an
    /// error, since they would contradict.
    fn forbidden_effects(&self, name: &str) -> Result<Vec<Effect>> {
        match (self.forbid.is_empty(), self.allow.is_empty()) {
            (false, true) => parse_effects(&self.forbid),
            (true, false) => {
                let allowed = parse_effects(&self.allow)?;
                Ok(ALL_EFFECTS
                    .into_iter()
                    .filter(|e| !allowed.contains(e))
                    .collect())
            }
            (true, true) => Ok(Vec::new()),
            (false, false) => {
                anyhow::bail!("region '{name}' sets both `forbid` and `allow`; pick one")
            }
        }
    }
}

/// Compile a list of glob strings into matchers.
fn compile_globs(globs: &[String]) -> Result<Vec<Pattern>> {
    globs
        .iter()
        .map(|g| Pattern::new(g).with_context(|| format!("bad glob pattern: {g}")))
        .collect()
}

/// Parse a list of effect spellings into categories.
fn parse_effects(names: &[String]) -> Result<Vec<Effect>> {
    names.iter().map(|n| Effect::from_str(n)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{EffectEngine, NaiveEngine};
    use crate::facts::{Definition, Edge, EffectRoot, Language};

    const FIXTURE: &str = r#"
[analysis]
confidence_threshold = "inferred"
ignore = ["**/tests/**"]

[region.core]
paths  = ["crates/*/src/**"]
forbid = ["fs", "net", "db", "process"]

[region.adapters]
paths = ["crates/*/src/adapters/**"]
allow = ["fs", "net", "process", "env"]
"#;

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
    fn parses_regions_ignore_and_allow_negation() {
        let policy = Policy::from_toml(FIXTURE).unwrap();
        assert_eq!(policy.regions.len(), 2);
        assert!(policy.is_ignored("crates/x/tests/foo.rs"));

        let adapters = policy
            .regions
            .iter()
            .find(|r| r.name == "adapters")
            .unwrap();
        // allow = fs/net/process/env -> forbids the rest: db, clock, random.
        assert!(adapters.forbid.contains(&Effect::Db));
        assert!(adapters.forbid.contains(&Effect::Clock));
        assert!(adapters.forbid.contains(&Effect::Random));
        assert!(!adapters.forbid.contains(&Effect::Fs));
    }

    #[test]
    fn glob_matching_places_files_in_regions() {
        let policy = Policy::from_toml(FIXTURE).unwrap();
        let core = policy.regions.iter().find(|r| r.name == "core").unwrap();
        assert!(core.matches("crates/hinzu-core/src/core.rs"));
        assert!(core.matches("crates/hinzu-core/src/adapters/io.rs"));
        assert!(!core.matches("notes/design.md"));
    }

    #[test]
    fn effectful_core_def_violates_but_allowed_region_def_does_not() {
        let policy = Policy::from_toml(FIXTURE).unwrap();

        let mut facts = FactSet::default();
        // A core function that reaches fs through an adapter.
        facts.add_def(def("core_fn", "crates/hinzu-core/src/core.rs"));
        facts.add_def(def("adapter_fn", "crates/hinzu-core/src/adapters/io.rs"));
        facts.add_edge(Edge::call(
            "core_fn",
            "adapter_fn",
            "crates/hinzu-core/src/core.rs",
            3,
        ));
        facts.add_edge(Edge::call(
            "adapter_fn",
            "std::fs::read",
            "crates/hinzu-core/src/adapters/io.rs",
            2,
        ));
        facts.add_root(EffectRoot {
            symbol: "std::fs::read".to_string(),
            effect: Effect::Fs,
        });

        let summaries = NaiveEngine.propagate(&facts);
        let violations = check(&facts, &summaries, &policy);

        // core_fn is governed by `core`, which forbids fs -> one violation.
        // adapter_fn's most-specific region is `adapters` (a nested carve-out),
        // which allows fs -> no violation. So exactly one violation, on core_fn.
        assert_eq!(violations.len(), 1);
        let core_v = &violations[0];
        assert_eq!(core_v.symbol, "core_fn");
        assert_eq!(core_v.effect, Effect::Fs);
        assert_eq!(core_v.region, "core");
        assert_eq!(
            core_v.evidence,
            vec![
                "core_fn".to_string(),
                "adapter_fn".to_string(),
                "std::fs::read".to_string()
            ]
        );
    }
}
