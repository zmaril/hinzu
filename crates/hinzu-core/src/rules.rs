//! The rule engine: run registered rules over a shared [`RuleContext`] and
//! collect [`Finding`]s.
//!
//! A rule is a query over the fact database and the facts the engine derives
//! from it, producing findings — the shape the effect-region policy already has,
//! lifted so other analyses land in the same report and gate the same way. The
//! engine builds the shared derived facts once and hands every enabled rule the
//! same [`RuleContext`]; a rule reads what it needs and emits findings.
//!
//! Effect propagation is the first and best-proven rule rather than a privileged
//! mechanism: [`EffectsRule`] runs the existing effect-region check behind the
//! [`Rule`] trait and produces the same findings, in the same order, with the
//! same evidence paths as before. The seam is otherwise design-agnostic — the
//! language-understanding rules the design note specifies (a component-aware
//! effect view, prop-drilling, one-component-per-file) attach behind this trait
//! once their derived facts land, without disturbing what runs today.

use std::collections::BTreeMap;

use crate::effects::{forward_adjacency, EffectSummary};
use crate::facts::{FactSet, SymbolId};
use crate::policy::{self, Policy, Severity};

/// The id the ported effect-region check registers under. It always runs (gated
/// by the presence of `[region.*]`, as before), so it is never listed in
/// `[rules].enable`.
pub const EFFECTS_RULE_ID: &str = "effects";

/// A flagged definition with the evidence path that justifies it — the unit a
/// rule emits and the reporter and exit-code gate consume uniformly.
///
/// This generalizes [`policy::Violation`]: the effect-region check specializes
/// it with an `effect` and a `region`, while a `Finding` carries only what every
/// rule shares — the id of the rule that produced it, the flagged definition and
/// its location, a human-facing `message`, the `evidence` chain down the graph
/// (what separates hinzu's findings from a scanner's: it reports *why*, hop by
/// hop), and a `severity` that decides whether the run fails. A future
/// structure-shaped rule fills the same fields without an `effect` or `region`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// The rule that produced this finding (its [`Rule::id`]).
    pub rule: &'static str,
    /// The flagged callable or component.
    pub symbol: SymbolId,
    /// Its display name, as the report prints it.
    pub display: String,
    /// The file it lives in.
    pub file: String,
    /// The first line of its definition span.
    pub line_start: u32,
    /// The last line of its definition span.
    pub line_end: u32,
    /// The human-facing explanation the report prints.
    pub message: String,
    /// The path that justifies the finding: a chain of symbols down the graph.
    pub evidence: Vec<SymbolId>,
    /// Whether this fails the run ([`Severity::Error`]) or is only reported
    /// ([`Severity::Warning`]).
    pub severity: Severity,
}

impl Finding {
    /// Whether this finding fails the run. Warnings are reported but do not fail.
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// The shared context every rule reads. The engine computes each derived
/// structure once and hands the same context to every enabled rule, so ten
/// rules that all want the same derived fact pay for it once.
///
/// Today it carries the [`FactSet`], the per-symbol [`EffectSummary`] map, the
/// [`Policy`], and the forward adjacency used to reconstruct evidence paths. The
/// language-aware derived facts the design note specifies — the component index,
/// the render tree, the prop-flow relation — attach here as further fields when
/// the deferred rules and their adapter extensions land; the seam is shaped for
/// them now so adding one does not change this type's role.
pub struct RuleContext<'a> {
    /// The normalized facts a rule queries.
    pub facts: &'a FactSet,
    /// Each definition's transitively-reachable effects and evidence paths.
    pub summaries: &'a BTreeMap<SymbolId, EffectSummary>,
    /// The policy: regions, `on_unknown`, and the `[rules]` configuration.
    pub policy: &'a Policy,
    /// Forward adjacency (caller -> the symbols it uses), for the evidence-path
    /// walk a rule reconstructs a chain with. Derived once, shared.
    pub forward_adjacency: BTreeMap<SymbolId, Vec<SymbolId>>,
}

impl<'a> RuleContext<'a> {
    /// Build the context, computing the shared derived facts once.
    pub fn new(
        facts: &'a FactSet,
        summaries: &'a BTreeMap<SymbolId, EffectSummary>,
        policy: &'a Policy,
    ) -> Self {
        Self {
            facts,
            summaries,
            policy,
            forward_adjacency: forward_adjacency(facts),
        }
    }

    /// The config view for one rule: whether `[rules].enable` lists it and its
    /// own `[rules.<id>]` table, if any.
    fn config_for(&self, id: &str) -> RuleConfig<'_> {
        RuleConfig {
            enabled: self.policy.rules.enable.iter().any(|e| e == id),
            table: self.policy.rules.tables.get(id),
        }
    }
}

/// A rule's slice of the `[rules]` configuration: whether it is enabled and its
/// own `[rules.<id>]` table. A rule parses its thresholds and toggles out of
/// `table`; the effect-region check ignores it and reads `[region.*]` instead.
pub struct RuleConfig<'a> {
    /// Whether `[rules].enable` lists this rule's id.
    pub enabled: bool,
    /// The rule's `[rules.<id>]` config table, if present.
    pub table: Option<&'a toml::Value>,
}

/// One analysis over a fact set. The engine builds the shared derived facts once
/// and hands every enabled rule the same context; a rule reads what it needs and
/// emits findings. New rules implement this and register an id.
pub trait Rule {
    /// The stable id used in `[rules]` config and in every finding.
    fn id(&self) -> &'static str;
    /// Run the query and emit findings. `cx` carries the fact set plus the
    /// derived facts, so a rule never recomputes them.
    fn check(&self, cx: &RuleContext, config: &RuleConfig) -> Vec<Finding>;
}

/// The ported effect-region check, behind the [`Rule`] trait. It runs the exact
/// [`policy::check`] the CLI ran before and maps each [`policy::Violation`] to a
/// [`Finding`], preserving order, evidence, severity, and the rendered message —
/// a refactor, not a behavior change.
pub struct EffectsRule;

impl Rule for EffectsRule {
    fn id(&self) -> &'static str {
        EFFECTS_RULE_ID
    }

    fn check(&self, cx: &RuleContext, _config: &RuleConfig) -> Vec<Finding> {
        policy::check(cx.facts, cx.summaries, cx.policy)
            .into_iter()
            .map(|v| finding_from_violation(self.id(), v, cx.facts))
            .collect()
    }
}

/// Lift one effect-region [`policy::Violation`] into a [`Finding`]. The message
/// is the violation's own rendering, so a finding reads identically to how the
/// violation read before the rule engine existed.
fn finding_from_violation(rule: &'static str, v: policy::Violation, facts: &FactSet) -> Finding {
    let message = v.describe();
    let (line_start, line_end) = facts
        .defs
        .get(&v.symbol)
        .map(|d| (d.line_start, d.line_end))
        .unwrap_or((0, 0));
    Finding {
        rule,
        symbol: v.symbol,
        display: v.display,
        file: v.file,
        line_start,
        line_end,
        message,
        evidence: v.evidence,
        severity: v.severity,
    }
}

/// The registry: the rules to run, in registration order. Effects is registered
/// first, so its findings lead the report exactly as they did before.
pub struct RuleEngine {
    rules: Vec<Box<dyn Rule>>,
}

impl Default for RuleEngine {
    fn default() -> Self {
        Self::with_builtin()
    }
}

impl RuleEngine {
    /// An engine with only the built-in effect-region rule registered — the set
    /// that runs today. The design note's named rules register on top of this
    /// once they land.
    pub fn with_builtin() -> Self {
        Self {
            rules: vec![Box::new(EffectsRule)],
        }
    }

    /// Register another rule to run after the ones already registered.
    pub fn register(&mut self, rule: Box<dyn Rule>) {
        self.rules.push(rule);
    }

    /// Fold every applicable rule over the shared context and concatenate their
    /// findings. The effect-region rule always runs; a named rule runs only when
    /// `[rules].enable` lists it, so an engine with no named rules enabled
    /// produces exactly the effect-region findings.
    pub fn run(&self, cx: &RuleContext) -> Vec<Finding> {
        let mut findings = Vec::new();
        for rule in &self.rules {
            let config = cx.config_for(rule.id());
            if rule.id() != EFFECTS_RULE_ID && !config.enabled {
                continue;
            }
            findings.extend(rule.check(cx, &config));
        }
        findings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{EffectEngine, NaiveEngine};
    use crate::facts::{Definition, Edge, Effect, EffectRoot, Language};

    /// Facts where a core function reaches the filesystem through an adapter —
    /// the canonical functional-core leak the effect-region check flags.
    fn leak_facts() -> FactSet {
        let mut facts = FactSet::default();
        facts.add_def(Definition {
            id: "core_fn".to_string(),
            display: "core_fn".to_string(),
            language: Language::Rust,
            file: "crates/hinzu-core/src/core.rs".to_string(),
            line_start: 10,
            line_end: 20,
        });
        facts.add_def(Definition {
            id: "adapter_fn".to_string(),
            display: "adapter_fn".to_string(),
            language: Language::Rust,
            file: "crates/hinzu-core/src/adapters/io.rs".to_string(),
            line_start: 1,
            line_end: 6,
        });
        facts.add_edge(Edge::call(
            "core_fn",
            "adapter_fn",
            "crates/hinzu-core/src/core.rs",
            14,
        ));
        facts.add_edge(Edge::call(
            "adapter_fn",
            "std::fs::read",
            "crates/hinzu-core/src/adapters/io.rs",
            3,
        ));
        facts.add_root(EffectRoot {
            symbol: "std::fs::read".to_string(),
            effect: Effect::Fs,
        });
        facts
    }

    const POLICY: &str = r#"
[region.core]
paths  = ["crates/*/src/**"]
forbid = ["fs", "net", "process"]

[region.adapters]
paths = ["crates/*/src/adapters/**"]
allow = ["fs", "net", "process", "env"]
"#;

    /// The engine's effects finding matches the underlying violation field for
    /// field — the port carries the message, evidence, severity, and location
    /// through unchanged, so the report reads identically.
    #[test]
    fn effects_rule_mirrors_the_underlying_violation() {
        let facts = leak_facts();
        let summaries = NaiveEngine.propagate(&facts);
        let policy = Policy::from_toml(POLICY).unwrap();

        let violations = policy::check(&facts, &summaries, &policy);
        assert_eq!(violations.len(), 1);

        let cx = RuleContext::new(&facts, &summaries, &policy);
        let findings = RuleEngine::with_builtin().run(&cx);

        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.rule, EFFECTS_RULE_ID);
        assert_eq!(f.symbol, "core_fn");
        assert_eq!(f.message, violations[0].describe());
        assert_eq!(f.evidence, violations[0].evidence);
        assert_eq!(f.severity, violations[0].severity);
        assert!(f.is_error());
        // The location is threaded from the flagged definition.
        assert_eq!((f.line_start, f.line_end), (10, 20));
    }

    /// A clean policy produces no findings — the run is green, exactly as the
    /// effect-region check alone would report.
    #[test]
    fn a_clean_policy_yields_no_findings() {
        let facts = leak_facts();
        let summaries = NaiveEngine.propagate(&facts);
        // A permissive policy: the core carve-out allows everything.
        let policy = Policy::from_toml(
            "[region.core]\npaths = [\"crates/*/src/**\"]\nallow = [\"fs\", \"net\", \"db\", \"process\", \"clock\", \"random\", \"env\", \"alloc\"]\n",
        )
        .unwrap();
        let cx = RuleContext::new(&facts, &summaries, &policy);
        assert!(RuleEngine::with_builtin().run(&cx).is_empty());
    }

    /// A named rule listed in `[rules].enable` runs; the same rule left out does
    /// not. Effects is unaffected by `enable` — it always runs.
    #[test]
    fn enable_gates_named_rules_but_not_effects() {
        struct Marker;
        impl Rule for Marker {
            fn id(&self) -> &'static str {
                "marker"
            }
            fn check(&self, cx: &RuleContext, config: &RuleConfig) -> Vec<Finding> {
                assert!(config.enabled, "only runs when enabled");
                vec![Finding {
                    rule: "marker",
                    symbol: "x".to_string(),
                    display: "x".to_string(),
                    file: cx.facts.defs.keys().next().cloned().unwrap_or_default(),
                    line_start: 0,
                    line_end: 0,
                    message: "marker fired".to_string(),
                    evidence: Vec::new(),
                    severity: Severity::Warning,
                }]
            }
        }

        let facts = leak_facts();
        let summaries = NaiveEngine.propagate(&facts);

        // Run the builtin engine plus a freshly-registered Marker under `policy_src`.
        let run = |policy_src: &str| {
            let policy = Policy::from_toml(policy_src).unwrap();
            let cx = RuleContext::new(&facts, &summaries, &policy);
            let mut engine = RuleEngine::with_builtin();
            engine.register(Box::new(Marker));
            engine.run(&cx)
        };

        // Not enabled: the marker rule is skipped, only effects runs.
        let findings = run(POLICY);
        assert!(findings.iter().all(|f| f.rule == EFFECTS_RULE_ID));

        // Enabled: the marker rule fires alongside effects.
        let enabled_src = format!("{POLICY}\n[rules]\nenable = [\"marker\"]\n");
        let findings = run(&enabled_src);
        assert!(findings.iter().any(|f| f.rule == "marker"));
        assert!(findings.iter().any(|f| f.rule == EFFECTS_RULE_ID));
    }
}
