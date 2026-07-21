//! The policy check: fail any callable that can reach a forbidden effect from a
//! region that forbids it. Regions match source files with globs, so the policy
//! lives outside the source (`hinzu.toml`), not in annotations.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use anyhow::{Context, Result};
use glob::Pattern;
use serde::Deserialize;

use crate::effects::EffectSummary;
use crate::facts::{Effect, FactSet, SymbolId};

/// What to do when a callable in a checked region is (transitively) `Unknown` —
/// it reaches an unseen external call the analysis could not resolve. Set by
/// `[analysis] on_unknown`; the default is [`OnUnknown::Fail`], because a
/// functional core cannot be certified pure while it reaches code we cannot see.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum OnUnknown {
    /// An `Unknown`-reaching callable in a forbidding region is a violation and
    /// exits nonzero. The safe default.
    #[default]
    Fail,
    /// Report the `Unknown`, but do not fail the run (exit stays zero).
    Warn,
    /// Treat `Unknown` as pure — the old, unsound behavior. Opt in explicitly.
    Ignore,
}

impl FromStr for OnUnknown {
    type Err = anyhow::Error;

    /// Parse the `[analysis] on_unknown` spelling.
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "fail" => Ok(OnUnknown::Fail),
            "warn" => Ok(OnUnknown::Warn),
            "ignore" => Ok(OnUnknown::Ignore),
            other => anyhow::bail!("unknown on_unknown value '{other}': use fail, warn, or ignore"),
        }
    }
}

/// Whether a finding fails the run (`Error`) or is merely reported (`Warning`).
/// Forbidden-effect violations are always errors; an `Unknown` finding is an
/// error under `on_unknown = "fail"` and a warning under `"warn"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// Why a callable was flagged: it reaches a forbidden real effect, or it reaches
/// an `Unknown` external the analysis could not resolve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Finding {
    /// Reaches a forbidden real effect (`fs`, `net`, …).
    ForbiddenEffect,
    /// Cannot be certified: reaches an unseen external. `callee` is the offending
    /// external symbol (the tail of the evidence path); `flavor` says whether the
    /// callee's *effect* was unknown or its *target* was unresolved.
    Unknown {
        callee: SymbolId,
        flavor: UnknownFlavor,
    },
}

/// The two ways a call becomes `Unknown`: a foreign callee with no body that no
/// annotation, root, or trusted-pure baseline covered (`UnknownEffect`), or an
/// indirect call (function pointer / `dyn`) whose target could not be resolved
/// (`UnknownTarget`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnknownFlavor {
    /// We saw the callee's path but not its body, and nothing vouched for it.
    Effect,
    /// We could not even resolve which function the call site targets.
    Target,
}

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

/// Configuration for the rule engine, parsed from the `[rules]` section of
/// `hinzu.toml`. It sits beside `[region.*]` rather than folding into it: the
/// effect-region policy is path-shaped and keeps its own surface, while the
/// named rules the design note introduces are structure-shaped and gated here.
///
/// `enable` turns on the named rules by id; the effect-region policy always runs
/// whenever a `[region.*]` is present, exactly as before, so a `hinzu.toml` with
/// no `[rules]` section behaves identically to one written before the section
/// existed. `tables` holds each rule's own `[rules.<id>]` config verbatim, so a
/// rule owns the schema of its table and a new rule adds a section without
/// touching the ones already parsed.
#[derive(Clone, Debug, Default)]
pub struct RulesConfig {
    /// The named rules turned on, by id. The effect-region policy is not listed
    /// here — it runs on the presence of `[region.*]`.
    pub enable: Vec<String>,
    /// Per-rule config tables (`[rules.<id>]`), kept as raw TOML so each rule
    /// parses its own thresholds and toggles.
    pub tables: BTreeMap<String, toml::Value>,
}

/// The full policy — regions, the globs that exclude files entirely, what to do
/// about `Unknown`, and the rule-engine configuration.
#[derive(Clone, Debug, Default)]
pub struct Policy {
    pub regions: Vec<Region>,
    pub ignore: Vec<Pattern>,
    pub on_unknown: OnUnknown,
    pub rules: RulesConfig,
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

/// A flagged callable, with the evidence path that explains why. Either it
/// reaches a forbidden real effect, or — with [`Finding::Unknown`] — it reaches
/// an unseen external the analysis could not certify.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    pub symbol: SymbolId,
    pub display: String,
    pub file: String,
    pub region: String,
    /// The effect the region forbade, or [`Effect::Unknown`] for an unknown
    /// finding.
    pub effect: Effect,
    pub evidence: Vec<SymbolId>,
    /// Why it was flagged, and — for a forbidden effect vs. an unknown external —
    /// how the report should read.
    pub finding: Finding,
    /// Whether this fails the run or is only reported (see [`OnUnknown`]).
    pub severity: Severity,
}

impl Violation {
    /// Whether this finding fails the run (an [`Severity::Error`]). Warnings
    /// (from `on_unknown = "warn"`) are reported but do not fail.
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    /// One human-readable line explaining the violation, distinguishing a
    /// forbidden-effect violation from an unknown-external one so the two never
    /// read the same. This is the message the report prints and the string the
    /// rule engine carries as a [`crate::rules::Finding`]'s `message`, so both
    /// paths render a violation identically.
    pub fn describe(&self) -> String {
        match &self.finding {
            Finding::ForbiddenEffect => format!(
                "{} forbids {} in region '{}': {}",
                self.display,
                self.effect.as_str(),
                self.region,
                self.evidence.join(" -> "),
            ),
            Finding::Unknown { callee, flavor } => {
                let what = match flavor {
                    UnknownFlavor::Effect => format!("unknown external `{callee}`"),
                    UnknownFlavor::Target => "an unresolved call target".to_string(),
                };
                format!(
                    "{} cannot certify in region '{}': reaches {} — {}",
                    self.display,
                    self.region,
                    what,
                    self.evidence.join(" -> "),
                )
            }
        }
    }
}

/// Check every definition's effect summary against the policy.
///
/// Each definition is governed by the single most-specific region whose globs
/// match its file (a nested `adapters` carve-out overrides the broader `core`),
/// unless the policy's `ignore` globs exclude the file. The definition is
/// flagged when that region forbids a real effect its summary reaches, and —
/// unless `on_unknown = "ignore"` — when its summary reaches an `Unknown`
/// external from a region that forbids anything (an unseen external could be
/// hiding any effect, so it cannot be certified against a non-empty forbid set).
pub fn check(
    facts: &FactSet,
    summaries: &BTreeMap<SymbolId, EffectSummary>,
    policy: &Policy,
) -> Vec<Violation> {
    // The callees of unresolved (fn-pointer / dyn) edges: an `Unknown` whose
    // evidence path ends at one of these is a *target*-unknown, not an
    // effect-unknown. Everything else is an effect-unknown.
    let unresolved: BTreeSet<&str> = facts
        .edges
        .iter()
        .filter(|e| e.resolution == crate::facts::EdgeResolution::Unresolved)
        .map(|e| e.callee.as_str())
        .collect();

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
                        finding: Finding::ForbiddenEffect,
                        severity: Severity::Error,
                    });
                }
            }

            // An unseen external is uncertain: it could be doing anything the
            // region forbids. So a region that forbids *any* real effect cannot
            // certify a callable that reaches `Unknown`. A pure allow-everything
            // region (empty forbid) has nothing to hide, so it is exempt.
            if policy.on_unknown != OnUnknown::Ignore
                && !region.forbid.is_empty()
                && summary.effects.contains(&Effect::Unknown)
            {
                let evidence = summary
                    .evidence
                    .get(&Effect::Unknown)
                    .cloned()
                    .unwrap_or_default();
                let callee = evidence.last().cloned().unwrap_or_default();
                let flavor = if unresolved.contains(callee.as_str()) {
                    UnknownFlavor::Target
                } else {
                    UnknownFlavor::Effect
                };
                let severity = match policy.on_unknown {
                    OnUnknown::Fail => Severity::Error,
                    OnUnknown::Warn => Severity::Warning,
                    OnUnknown::Ignore => unreachable!("guarded above"),
                };
                violations.push(Violation {
                    symbol: def.id.clone(),
                    display: def.display.clone(),
                    file: def.file.clone(),
                    region: region.name.clone(),
                    effect: Effect::Unknown,
                    evidence,
                    finding: Finding::Unknown { callee, flavor },
                    severity,
                });
            }
        }
    }

    violations
}

// --- the `hinzu.toml` wire shape ---------------------------------------------

#[derive(Deserialize)]
struct PolicyDoc {
    #[serde(default)]
    analysis: AnalysisDoc,
    #[serde(default)]
    region: BTreeMap<String, RegionDoc>,
    /// The `[rules]` section: `enable = [...]` plus each rule's `[rules.<id>]`
    /// subtable, kept as a raw table so per-rule schemas stay with their rules.
    /// Absent in every `hinzu.toml` written before the rule engine — parsing it
    /// as an `Option` keeps those files behaving exactly as they did.
    #[serde(default)]
    rules: Option<toml::value::Table>,
}

#[derive(Default, Deserialize)]
struct AnalysisDoc {
    #[serde(default)]
    #[allow(dead_code)]
    confidence_threshold: Option<String>,
    #[serde(default)]
    ignore: Vec<String>,
    /// `fail` (default), `warn`, or `ignore` — what to do about `Unknown`.
    #[serde(default)]
    on_unknown: Option<String>,
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
        let on_unknown = match &self.analysis.on_unknown {
            Some(s) => OnUnknown::from_str(s).context("parsing [analysis] on_unknown")?,
            None => OnUnknown::default(),
        };

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

        let rules = parse_rules(self.rules)?;

        Ok(Policy {
            regions,
            ignore,
            on_unknown,
            rules,
        })
    }
}

/// Parse the `[rules]` section into a [`RulesConfig`]. `enable` is pulled out as
/// the list of named-rule ids; every remaining key is a `[rules.<id>]` config
/// subtable kept verbatim for the rule that owns it. A missing section yields
/// the default (no named rules, no tables) — the pre-rule-engine behavior.
fn parse_rules(table: Option<toml::value::Table>) -> Result<RulesConfig> {
    let Some(mut table) = table else {
        return Ok(RulesConfig::default());
    };
    let enable = match table.remove("enable") {
        None => Vec::new(),
        Some(v) => v
            .try_into::<Vec<String>>()
            .context("[rules] enable must be a list of rule ids")?,
    };
    let tables = table.into_iter().collect();
    Ok(RulesConfig { enable, tables })
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
                Ok(Effect::REAL
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

/// Parse a list of effect spellings into categories. Rejects `"unknown"`: it is
/// not a category a region can forbid or allow — uncertainty is governed by
/// `[analysis] on_unknown`, so naming it here is almost always a mistake.
fn parse_effects(names: &[String]) -> Result<Vec<Effect>> {
    names
        .iter()
        .map(|n| {
            let effect = Effect::from_str(n)?;
            if effect == Effect::Unknown {
                anyhow::bail!(
                    "'unknown' is not a region effect; control it with [analysis] on_unknown"
                );
            }
            Ok(effect)
        })
        .collect()
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
            is_component: false,
        }
    }

    /// A `hinzu.toml` with no `[rules]` section parses to an empty rules config:
    /// no named rules enabled, no per-rule tables. This is the backward-compat
    /// guarantee — files written before the rule engine behave unchanged.
    #[test]
    fn absent_rules_section_parses_to_empty_config() {
        let policy = Policy::from_toml(FIXTURE).unwrap();
        assert!(policy.rules.enable.is_empty());
        assert!(policy.rules.tables.is_empty());
    }

    /// The `[rules]` section is parsed: `enable` lists the named rules and each
    /// `[rules.<id>]` subtable is kept verbatim for the rule that owns it.
    #[test]
    fn rules_section_parses_enable_and_per_rule_tables() {
        let src = format!(
            "{FIXTURE}\n[rules]\nenable = [\"prop-drilling\"]\n\n[rules.prop-drilling]\nmax_depth = 3\n"
        );
        let policy = Policy::from_toml(&src).unwrap();
        assert_eq!(policy.rules.enable, vec!["prop-drilling".to_string()]);
        let table = policy.rules.tables.get("prop-drilling").unwrap();
        assert_eq!(table.get("max_depth").and_then(|v| v.as_integer()), Some(3));
        // The reserved `enable` key is not mistaken for a rule table.
        assert!(!policy.rules.tables.contains_key("enable"));
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
        assert_eq!(core_v.finding, Finding::ForbiddenEffect);
        assert!(core_v.is_error());
    }

    #[test]
    fn on_unknown_defaults_to_fail_and_rejects_a_named_region_effect() {
        // Default when the key is absent.
        assert_eq!(
            Policy::from_toml(FIXTURE).unwrap().on_unknown,
            OnUnknown::Fail
        );
        // `unknown` is not a category a region may name.
        let bad = "[region.core]\npaths=[\"x\"]\nforbid=[\"unknown\"]\n";
        assert!(Policy::from_toml(bad).is_err());
    }

    /// A core-forbidding policy with the given `on_unknown` setting.
    fn policy_with_on_unknown(mode: &str) -> Policy {
        let src = format!(
            "[analysis]\non_unknown = \"{mode}\"\n\
             [region.core]\npaths = [\"crates/*/src/**\"]\nforbid = [\"fs\", \"net\"]\n"
        );
        Policy::from_toml(&src).unwrap()
    }

    /// A core function reaching an `Unknown` external, for the on_unknown cases.
    fn unknown_facts() -> (FactSet, BTreeMap<SymbolId, EffectSummary>) {
        let mut facts = FactSet::default();
        facts.add_def(def("core_fn", "crates/hinzu-core/src/core.rs"));
        facts.add_edge(Edge::call(
            "core_fn",
            "serde_json::from_str",
            "crates/hinzu-core/src/core.rs",
            3,
        ));
        facts.add_root(EffectRoot {
            symbol: "serde_json::from_str".to_string(),
            effect: Effect::Unknown,
        });
        let summaries = NaiveEngine.propagate(&facts);
        (facts, summaries)
    }

    #[test]
    fn on_unknown_fail_flags_a_distinct_unknown_error() {
        let policy = policy_with_on_unknown("fail");
        let (facts, summaries) = unknown_facts();
        let violations = check(&facts, &summaries, &policy);
        assert_eq!(violations.len(), 1);
        let v = &violations[0];
        assert!(v.is_error());
        assert_eq!(v.effect, Effect::Unknown);
        assert_eq!(
            v.finding,
            Finding::Unknown {
                callee: "serde_json::from_str".to_string(),
                flavor: UnknownFlavor::Effect,
            }
        );
        // The evidence path ends at the offending external.
        assert_eq!(v.evidence.last().unwrap(), "serde_json::from_str");
    }

    #[test]
    fn on_unknown_warn_reports_but_does_not_fail() {
        let policy = policy_with_on_unknown("warn");
        let (facts, summaries) = unknown_facts();
        let violations = check(&facts, &summaries, &policy);
        assert_eq!(violations.len(), 1);
        assert!(!violations[0].is_error());
        assert_eq!(violations[0].severity, Severity::Warning);
    }

    #[test]
    fn on_unknown_ignore_treats_unknown_as_pure() {
        let policy = policy_with_on_unknown("ignore");
        let (facts, summaries) = unknown_facts();
        assert!(check(&facts, &summaries, &policy).is_empty());
    }

    #[test]
    fn an_unresolved_indirect_call_is_a_target_unknown() {
        let policy = Policy::from_toml(FIXTURE).unwrap();
        let mut facts = FactSet::default();
        facts.add_def(def("core_fn", "crates/hinzu-core/src/core.rs"));
        facts.add_edge(Edge {
            caller: "core_fn".to_string(),
            callee: "<indirect>".to_string(),
            kind: crate::facts::EdgeKind::Call,
            resolution: crate::facts::EdgeResolution::Unresolved,
            evidence_file: "crates/hinzu-core/src/core.rs".to_string(),
            evidence_line: 3,
            seam: false,
        });
        facts.add_root(EffectRoot {
            symbol: "<indirect>".to_string(),
            effect: Effect::Unknown,
        });
        let summaries = NaiveEngine.propagate(&facts);
        let violations = check(&facts, &summaries, &policy);
        assert_eq!(violations.len(), 1);
        assert_eq!(
            violations[0].finding,
            Finding::Unknown {
                callee: "<indirect>".to_string(),
                flavor: UnknownFlavor::Target,
            }
        );
    }
}
