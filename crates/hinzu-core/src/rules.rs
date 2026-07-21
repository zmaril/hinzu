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

use std::collections::{BTreeMap, BTreeSet};

use crate::effects::{forward_adjacency, shortest_path_to_roots, EffectSummary};
use crate::facts::{Definition, Effect, FactSet, SymbolId};
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

    /// Whether `symbol` names a definition the adapter tagged a React component.
    /// The component-shaped rules ask this rather than matching a naming
    /// convention — the flag is a type-checker decision the adapter recorded.
    pub fn is_component(&self, symbol: &SymbolId) -> bool {
        self.facts
            .defs
            .get(symbol)
            .map(|d| d.is_component)
            .unwrap_or(false)
    }

    /// Every definition the adapter tagged a component, in fact-set order. The
    /// component-shaped rules iterate this instead of filtering the whole
    /// definition table themselves.
    pub fn components(&self) -> impl Iterator<Item = &Definition> {
        self.facts.defs.values().filter(|d| d.is_component)
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

/// The id the component-aware effect rule registers under, listed in
/// `[rules].enable` to turn it on.
pub const EFFECT_IN_COMPONENT_RULE_ID: &str = "effect-in-component";

/// The effects a component's render path may not reach, unless the default is
/// overridden by the rule's `[rules.effect-in-component] forbid` list. `db` is
/// carried for completeness even though the TypeScript adapter does not yet seed
/// it — a rule states what it forbids, not what an adapter happens to emit.
const DEFAULT_FORBIDDEN: [Effect; 4] = [Effect::Fs, Effect::Net, Effect::Db, Effect::Process];

/// `effect-in-component`: flag a component whose *synchronous render path*
/// reaches a forbidden effect (`fs`, `net`, `db`, `process`, …). A component may
/// touch the world through an effect hook (`useEffect` / `useLayoutEffect`) or an
/// event handler (`onClick`, …), but not while React is computing the render —
/// synchronous I/O in a render body is the smell, the same I/O inside a hook is
/// correct React.
///
/// This is a component-aware *view* over the effect machinery, not a new
/// analysis. The rule walks the same call/use graph the effect engine does, but
/// over the **render subgraph**: it drops the seam edges the adapter marked (the
/// value handed to an effect hook or an event-handler prop), so an effect reached
/// only through a seam is sanctioned and produces no finding, while an effect on
/// the synchronous render path is flagged with the real evidence path down to the
/// root — `Dashboard -> loadUser -> global::fetch`.
///
/// Fidelity is high: it tracks the effect analysis (high on resolved calls,
/// honestly `Unknown` on unseen externals) and its precision rests on the
/// adapter's seam marking. Inline hook callbacks and inline event handlers never
/// join a component's render path in the first place (the graph does not connect
/// an inline closure to its enclosing component), so the seam marking's job is to
/// cut the remaining case: a *named* function handed to a hook or a handler.
pub struct EffectInComponentRule;

/// The rule's `[rules.effect-in-component]` config: which effects a component's
/// render path may not reach. The seams that sanction an effect — effect hooks
/// and event handlers — are structural (the adapter marks them), so they are not
/// a config knob here; a `forbid` list only narrows or widens the effect set.
struct EffectInComponentConfig {
    forbid: BTreeSet<Effect>,
}

impl EffectInComponentConfig {
    /// Read the config from the rule's `[rules.effect-in-component]` table,
    /// falling back to [`DEFAULT_FORBIDDEN`] when `forbid` is absent. An
    /// unparseable effect name is skipped rather than failing the run, so a typo
    /// narrows the rule instead of aborting the whole check.
    fn from_table(table: Option<&toml::Value>) -> Self {
        let forbid = table
            .and_then(|t| t.get("forbid"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .filter_map(|s| s.parse::<Effect>().ok())
                    .filter(|e| *e != Effect::Unknown)
                    .collect::<BTreeSet<_>>()
            })
            .filter(|set| !set.is_empty())
            .unwrap_or_else(|| DEFAULT_FORBIDDEN.into_iter().collect());
        Self { forbid }
    }
}

/// Forward adjacency restricted to the **render path**: the effect-carrying
/// edges, minus the seam edges the adapter marked. A seam edge enters an effect
/// hook or an event handler, so an effect reachable only across one runs outside
/// render and must not taint the component. This is the one structural
/// difference from [`forward_adjacency`]; everything downstream reuses the shared
/// evidence-path walk.
fn render_forward_adjacency(facts: &FactSet) -> BTreeMap<SymbolId, Vec<SymbolId>> {
    let mut uses_of: BTreeMap<SymbolId, Vec<SymbolId>> = BTreeMap::new();
    for edge in facts
        .edges
        .iter()
        .filter(|e| e.kind.carries_effects() && !e.seam)
    {
        uses_of
            .entry(edge.caller.clone())
            .or_default()
            .push(edge.callee.clone());
    }
    uses_of
}

impl Rule for EffectInComponentRule {
    fn id(&self) -> &'static str {
        EFFECT_IN_COMPONENT_RULE_ID
    }

    fn check(&self, cx: &RuleContext, config: &RuleConfig) -> Vec<Finding> {
        let cfg = EffectInComponentConfig::from_table(config.table);

        // The root symbols that carry a forbidden effect, and the effect each
        // one reports. A render path that reaches one of these — over the
        // seam-cut subgraph — is a finding.
        let mut forbidden_effect: BTreeMap<SymbolId, Effect> = BTreeMap::new();
        for root in &cx.facts.roots {
            if cfg.forbid.contains(&root.effect) {
                forbidden_effect
                    .entry(root.symbol.clone())
                    .or_insert(root.effect);
            }
        }
        if forbidden_effect.is_empty() {
            return Vec::new();
        }
        let forbidden_syms: BTreeSet<SymbolId> = forbidden_effect.keys().cloned().collect();

        let render = render_forward_adjacency(cx.facts);
        let mut findings = Vec::new();
        for def in cx.components() {
            if cx.policy.is_ignored(&def.file) {
                continue;
            }
            let Some(path) = shortest_path_to_roots(&render, &def.id, &forbidden_syms) else {
                continue;
            };
            // The path ends at the forbidden root; report that root's effect.
            let effect = path
                .last()
                .and_then(|tail| forbidden_effect.get(tail))
                .copied()
                .unwrap_or(Effect::Unknown);
            let message = format!(
                "{} performs {} on its render path: {}",
                def.display,
                effect.as_str(),
                path.join(" -> "),
            );
            findings.push(Finding {
                rule: self.id(),
                symbol: def.id.clone(),
                display: def.display.clone(),
                file: def.file.clone(),
                line_start: def.line_start,
                line_end: def.line_end,
                message,
                evidence: path,
                severity: Severity::Error,
            });
        }
        findings
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
    /// An engine with only the built-in effect-region rule registered. The demo
    /// entry point and the effect-only tests use it; the named rules register on
    /// top of it via [`RuleEngine::with_all_rules`].
    pub fn with_builtin() -> Self {
        Self {
            rules: vec![Box::new(EffectsRule)],
        }
    }

    /// The engine `hinzu check` runs: the built-in effect-region rule plus every
    /// named rule the design note specifies, each still gated by
    /// `[rules].enable`, so a config that enables none behaves exactly like
    /// [`RuleEngine::with_builtin`]. Registering a rule here is what makes its id
    /// available to `enable`; the gate in [`RuleEngine::run`] keeps it inert
    /// until a policy turns it on.
    pub fn with_all_rules() -> Self {
        let mut engine = Self::with_builtin();
        engine.register(Box::new(EffectInComponentRule));
        engine
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
            is_component: false,
        });
        facts.add_def(Definition {
            id: "adapter_fn".to_string(),
            display: "adapter_fn".to_string(),
            language: Language::Rust,
            file: "crates/hinzu-core/src/adapters/io.rs".to_string(),
            line_start: 1,
            line_end: 6,
            is_component: false,
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

    // --- effect-in-component ------------------------------------------------

    /// The policy that turns the component rule on, with nothing else — no
    /// `[region.*]`, so the effect-region rule stays silent and any finding is
    /// the component rule's.
    const ENABLE_EIC: &str = "[rules]\nenable = [\"effect-in-component\"]\n";

    /// A TypeScript definition; `component` sets the semantic tag the rule reads.
    /// Both component and helper defs go through this, so the struct literal is
    /// written once.
    fn ts_def(id: &str, file: &str, component: bool) -> Definition {
        Definition {
            id: id.to_string(),
            display: id.rsplit("::").next().unwrap_or(id).to_string(),
            language: Language::TypeScript,
            file: file.to_string(),
            line_start: 1,
            line_end: 10,
            is_component: component,
        }
    }

    /// `Dashboard` (a component) renders, calling `loadUser`, which fetches — so
    /// the render path reaches `net`. When `seam` is set, the hop into `loadUser`
    /// is a seam edge (as if `useEffect(loadUser)` or `onClick={loadUser}`), so
    /// the fetch runs outside render.
    fn component_reaching_net(seam: bool) -> FactSet {
        let file = "src/web/Dashboard.tsx";
        let mut facts = FactSet::default();
        facts.add_def(ts_def("web::Dashboard", file, true));
        facts.add_def(ts_def("web::loadUser", file, false));
        let hop = Edge::call("web::Dashboard", "web::loadUser", file, 12);
        facts.add_edge(if seam { hop.seam() } else { hop });
        facts.add_edge(Edge::call("web::loadUser", "global::fetch", file, 34));
        facts.add_root(EffectRoot {
            symbol: "global::fetch".to_string(),
            effect: Effect::Net,
        });
        facts
    }

    /// Run every rule over `facts` under `policy_src` and keep only the
    /// component rule's findings.
    fn eic_findings(facts: &FactSet, policy_src: &str) -> Vec<Finding> {
        let summaries = NaiveEngine.propagate(facts);
        let policy = Policy::from_toml(policy_src).unwrap();
        let cx = RuleContext::new(facts, &summaries, &policy);
        RuleEngine::with_all_rules()
            .run(&cx)
            .into_iter()
            .filter(|f| f.rule == EFFECT_IN_COMPONENT_RULE_ID)
            .collect()
    }

    /// A component that reaches a forbidden effect on its synchronous render path
    /// is flagged, with the evidence path down to the effect root and an error
    /// severity that fails the run.
    #[test]
    fn flags_a_component_with_an_effect_on_its_render_path() {
        let facts = component_reaching_net(false);
        let findings = eic_findings(&facts, ENABLE_EIC);

        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.symbol, "web::Dashboard");
        assert!(f.is_error());
        assert_eq!(
            f.evidence,
            vec![
                "web::Dashboard".to_string(),
                "web::loadUser".to_string(),
                "global::fetch".to_string(),
            ]
        );
        assert!(
            f.message.contains("performs net"),
            "message was: {}",
            f.message
        );
    }

    /// The same effect reached only through a seam edge — an effect hook or an
    /// event handler — is sanctioned: it runs outside render, so the component is
    /// not flagged.
    #[test]
    fn a_seam_edge_sanctions_the_effect() {
        let facts = component_reaching_net(true);
        assert!(eic_findings(&facts, ENABLE_EIC).is_empty());
    }

    /// The rule is gated by `[rules].enable`: without it listed, a component with
    /// a render-path effect produces no component finding.
    #[test]
    fn does_not_run_unless_enabled() {
        let facts = component_reaching_net(false);
        assert!(eic_findings(&facts, "").is_empty());
    }

    /// A definition the adapter did not tag a component is never flagged, even
    /// when it reaches the same effect by the same path — the rule keys on the
    /// component flag, not the call graph alone.
    #[test]
    fn a_non_component_with_the_same_path_is_not_flagged() {
        let mut facts = component_reaching_net(false);
        // Demote the component to a plain callable; the graph is unchanged.
        facts.defs.get_mut("web::Dashboard").unwrap().is_component = false;
        assert!(eic_findings(&facts, ENABLE_EIC).is_empty());
    }

    /// The `forbid` list narrows the rule: a policy that forbids only `fs` does
    /// not flag a component that reaches `net`.
    #[test]
    fn forbid_list_narrows_the_effect_set() {
        let facts = component_reaching_net(false);
        let src =
            "[rules]\nenable = [\"effect-in-component\"]\n[rules.effect-in-component]\nforbid = [\"fs\"]\n";
        assert!(eic_findings(&facts, src).is_empty());
    }
}
