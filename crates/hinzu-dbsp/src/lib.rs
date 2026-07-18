//! The DBSP (Feldera) effect-propagation engine.
//!
//! It plugs into the same [`EffectEngine`](hinzu_core::effects::EffectEngine)
//! seam as the reference [`NaiveEngine`](hinzu_core::effects::NaiveEngine), so
//! the CLI and the fact store never learn which engine ran. The propagation is
//! a recursive fixed point over the edge relation, expressed as a DBSP circuit:
//!
//! ```text
//! effect(f, e)      :- effect_root(f, e)
//! effect(caller, e) :- edge(caller, callee), effect(callee, e)
//! ```
//!
//! `.distinct()` collapses the z-set weights to set semantics, so the rule is
//! monotone and the fixed point terminates even through call-graph cycles. The
//! circuit yields the effect *set* per function; each summary's evidence path
//! is then reconstructed with a breadth-first walk over the fact graph
//! ([`hinzu_core::effects::shortest_path_to_roots`]), shared with the engine
//! core so the path logic lives in exactly one place.
//!
//! The batch trait rebuilds the circuit per call. Keeping the circuit resident
//! for incremental delta steps (the property that motivates DBSP) is a later
//! phase; the seam here is deliberately the same batch shape as the reference
//! engine, so the two can be cross-checked pair for pair.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use anyhow::Result;
use dbsp::typed_batch::IndexedZSetReader;
use dbsp::utils::Tup2;
use dbsp::{OrdZSet, OutputHandle, RootCircuit, Runtime, Stream, ZSetHandle};

use hinzu_core::effects::{forward_adjacency, shortest_path_to_roots, EffectEngine, EffectSummary};
use hinzu_core::facts::{Effect, FactSet, SymbolId};

/// A pair carried through the circuit. Edges are `Tup2(caller, callee)`; effect
/// facts and roots are `Tup2(func, effect_spelling)`.
type Pair = Tup2<String, String>;

/// The DBSP effect-propagation engine. Batch today; the resident-circuit
/// incremental path is a later phase behind the same trait.
#[derive(Clone, Copy, Debug, Default)]
pub struct DbspEngine;

impl EffectEngine for DbspEngine {
    /// Propagate effects to a fixed point with the DBSP circuit, then attach an
    /// evidence path to each `(function, effect)` pair. Panics only if DBSP
    /// itself fails to build or run the circuit — an honest engine failure, not
    /// a faked-empty analysis; [`DbspEngine::try_propagate`] returns the error
    /// for callers that want to handle it.
    fn propagate(&self, facts: &FactSet) -> BTreeMap<SymbolId, EffectSummary> {
        self.try_propagate(facts)
            .expect("DBSP effect propagation failed")
    }
}

impl DbspEngine {
    /// The fallible core of [`EffectEngine::propagate`]: run the circuit and
    /// return each callable's summary, surfacing any DBSP error.
    pub fn try_propagate(&self, facts: &FactSet) -> Result<BTreeMap<SymbolId, EffectSummary>> {
        let pairs = self.effect_pairs(facts)?;
        Ok(summaries_from_pairs(facts, pairs))
    }

    /// Run the recursive circuit and return the set of `(function, effect)`
    /// pairs — the effect *set* per function, before evidence is attached.
    fn effect_pairs(&self, facts: &FactSet) -> Result<BTreeSet<(SymbolId, Effect)>> {
        // Union call and reference edges, deduped to distinct (caller, callee):
        // both edge kinds carry effects, and z-set weights would otherwise
        // double-count (moot after `.distinct()`, but cheaper deduped here).
        let mut edge_set: BTreeSet<(String, String)> = BTreeSet::new();
        for edge in &facts.edges {
            edge_set.insert((edge.caller.clone(), edge.callee.clone()));
        }
        let roots: Vec<(String, String)> = facts
            .roots
            .iter()
            .map(|r| (r.symbol.clone(), r.effect.as_str().to_string()))
            .collect();

        // Single worker: repo-sized graphs are small enough that thread
        // coordination costs more than it saves (see the spike findings).
        let (mut circuit, (edges_h, roots_h, out_h)) = Runtime::init_circuit(1, build_circuit)?;

        edges_h.append(&mut to_zset(edge_set.into_iter()));
        roots_h.append(&mut to_zset(roots.into_iter()));
        circuit.transaction()?;

        let mut out = BTreeSet::new();
        let z = out_h.consolidate();
        for (Tup2(func, eff), (), weight) in z.iter() {
            if weight > 0 {
                out.insert((func.clone(), Effect::from_str(&eff)?));
            }
        }
        Ok(out)
    }
}

/// Build the recursive effect-closure circuit. Ported from the DBSP spike: the
/// `join` inside `recursive` requires a live runtime, so the circuit must be
/// built via [`Runtime::init_circuit`], not `RootCircuit::build`.
#[allow(clippy::type_complexity)]
fn build_circuit(
    circuit: &mut RootCircuit,
) -> Result<(
    ZSetHandle<Pair>,            // edges input  (caller, callee)
    ZSetHandle<Pair>,            // roots input  (func, effect)
    OutputHandle<OrdZSet<Pair>>, // effect summaries output (func, effect)
)> {
    let (edges, edges_handle) = circuit.add_input_zset::<Pair>();
    let (roots, roots_handle) = circuit.add_input_zset::<Pair>();

    let effects = circuit.recursive(|child: &_, effect: Stream<_, OrdZSet<Pair>>| {
        // Import the parent inputs into the nested (recursive) circuit.
        let edges = edges.delta0(child);
        let roots = roots.delta0(child);

        // Index effect facts by func (the callee side) and edges by callee, so
        // the join fires on `callee == func`.
        let effect_by_func = effect.map_index(|Tup2(func, eff)| (func.clone(), eff.clone()));
        let edges_by_callee =
            edges.map_index(|Tup2(caller, callee)| (callee.clone(), caller.clone()));

        // join on callee == func  =>  effect(caller, eff)
        let derived = edges_by_callee.join(&effect_by_func, |_callee_func, caller, eff| {
            Tup2(caller.clone(), eff.clone())
        });

        // Union with the base (roots) and collapse to set semantics so the
        // fixed point terminates through cycles.
        Ok(derived.plus(&roots).distinct())
    })?;

    Ok((edges_handle, roots_handle, effects.output()))
}

/// Turn `(a, b)` string pairs into weight-1 z-set entries for `append`.
fn to_zset(pairs: impl Iterator<Item = (String, String)>) -> Vec<Tup2<Pair, dbsp::ZWeight>> {
    pairs.map(|(a, b)| Tup2(Tup2(a, b), 1)).collect()
}

/// Attach an evidence path to each `(function, effect)` pair the circuit found.
/// The set membership is DBSP's; the path is a shortest breadth-first walk over
/// the fact graph to a root carrying that effect, reusing the engine core's
/// [`shortest_path_to_roots`] so the path logic is not duplicated.
fn summaries_from_pairs(
    facts: &FactSet,
    pairs: BTreeSet<(SymbolId, Effect)>,
) -> BTreeMap<SymbolId, EffectSummary> {
    let forward = forward_adjacency(facts);

    // The roots that carry each effect — the BFS targets for that effect.
    let mut roots_by_effect: BTreeMap<Effect, BTreeSet<SymbolId>> = BTreeMap::new();
    for root in &facts.roots {
        roots_by_effect
            .entry(root.effect)
            .or_default()
            .insert(root.symbol.clone());
    }

    let mut summaries: BTreeMap<SymbolId, EffectSummary> = BTreeMap::new();
    for (func, effect) in pairs {
        let entry = summaries.entry(func.clone()).or_default();
        entry.effects.insert(effect);
        if let Some(targets) = roots_by_effect.get(&effect) {
            if let Some(path) = shortest_path_to_roots(&forward, &func, targets) {
                entry.evidence.insert(effect, path);
            }
        }
    }
    summaries
}

#[cfg(test)]
mod tests {
    use super::*;
    use hinzu_core::effects::NaiveEngine;
    use hinzu_core::facts::{Edge, EffectRoot};

    fn fs_root(sym: &str) -> EffectRoot {
        EffectRoot {
            symbol: sym.to_string(),
            effect: Effect::Fs,
        }
    }

    /// The effect *set* per function must match the reference engine exactly.
    fn assert_same_effect_sets(facts: &FactSet) {
        let dbsp = DbspEngine.propagate(facts);
        let naive = NaiveEngine.propagate(facts);

        let dbsp_sets: BTreeMap<&SymbolId, &BTreeSet<Effect>> =
            dbsp.iter().map(|(k, v)| (k, &v.effects)).collect();
        let naive_sets: BTreeMap<&SymbolId, &BTreeSet<Effect>> =
            naive.iter().map(|(k, v)| (k, &v.effects)).collect();
        assert_eq!(
            dbsp_sets, naive_sets,
            "DBSP effect sets diverge from NaiveEngine"
        );
    }

    #[test]
    fn linear_chain_matches_naive_and_carries_the_effect() {
        // top -> mid -> root
        let mut facts = FactSet::default();
        facts.add_edge(Edge::call("top", "mid", "x.rs", 1));
        facts.add_edge(Edge::call("mid", "root", "x.rs", 2));
        facts.add_root(fs_root("root"));

        assert_same_effect_sets(&facts);

        let dbsp = DbspEngine.propagate(&facts);
        assert!(dbsp["top"].effects.contains(&Effect::Fs));
        assert_eq!(
            dbsp["top"].evidence[&Effect::Fs],
            vec!["top".to_string(), "mid".to_string(), "root".to_string()]
        );
    }

    #[test]
    fn unknown_propagates_like_an_effect() {
        // An `Unknown` root must flow up the graph and match the reference
        // engine, so `on_unknown` fires the same way under either engine.
        let mut facts = FactSet::default();
        facts.add_edge(Edge::call("caller", "serde_json::from_str", "x.rs", 1));
        facts.add_root(EffectRoot {
            symbol: "serde_json::from_str".to_string(),
            effect: Effect::Unknown,
        });
        assert_same_effect_sets(&facts);
        let dbsp = DbspEngine.propagate(&facts);
        assert!(dbsp["caller"].effects.contains(&Effect::Unknown));
    }

    #[test]
    fn cycle_terminates_and_matches_naive() {
        // a <-> b, a -> root
        let mut facts = FactSet::default();
        facts.add_edge(Edge::call("a", "b", "x.rs", 1));
        facts.add_edge(Edge::call("b", "a", "x.rs", 2));
        facts.add_edge(Edge::call("a", "root", "x.rs", 3));
        facts.add_root(fs_root("root"));

        assert_same_effect_sets(&facts);
        let dbsp = DbspEngine.propagate(&facts);
        assert!(dbsp["a"].effects.contains(&Effect::Fs));
        assert!(dbsp["b"].effects.contains(&Effect::Fs));
    }
}
