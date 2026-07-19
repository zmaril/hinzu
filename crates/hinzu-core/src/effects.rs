//! The propagation engine: reachability over the reverse call/use graph. This
//! is the language-independent core — it never touches syntax, only facts.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::facts::{Effect, FactSet, SymbolId};

/// A callable's transitively-reachable effects, each with one evidence path:
/// the caller chain from this callable down to the effectful root.
#[derive(Clone, Debug, Default)]
pub struct EffectSummary {
    pub effects: BTreeSet<Effect>,
    pub evidence: BTreeMap<Effect, Vec<SymbolId>>,
}

/// The seam every propagation engine plugs into: facts in, per-symbol effect
/// summaries out. `NaiveEngine` below is the breadth-first reference
/// implementation; phase 2 adds a DBSP (Feldera) engine behind this same trait
/// so the CLI and the fact store never learn which engine ran.
pub trait EffectEngine {
    /// Propagate effects to a fixed point and return each callable's summary.
    fn propagate(&self, facts: &FactSet) -> BTreeMap<SymbolId, EffectSummary>;
}

/// Forward adjacency: caller -> the symbols it uses. The transpose of the
/// reverse map [`propagate`] builds. The evidence walk in
/// [`shortest_path_to_roots`] follows it downward, from a function toward an
/// effect root.
pub fn forward_adjacency(facts: &FactSet) -> BTreeMap<SymbolId, Vec<SymbolId>> {
    let mut uses_of: BTreeMap<SymbolId, Vec<SymbolId>> = BTreeMap::new();
    // Type edges are signature-type dependencies (for porting closures), not
    // calls — they must not carry effects, so they are excluded here.
    for edge in facts.edges.iter().filter(|e| e.kind.carries_effects()) {
        uses_of
            .entry(edge.caller.clone())
            .or_default()
            .push(edge.callee.clone());
    }
    uses_of
}

/// A shortest path from `start` down the call/use graph to any symbol in
/// `roots`, both ends inclusive, or `None` when no root is reachable. The walk
/// is breadth-first, so the path is the fewest hops — the short explanation the
/// report prints. An engine that computes only the effect *set* (like the DBSP
/// circuit) reconstructs each evidence path with this, restricting `roots` to
/// the roots that carry the effect in question.
pub fn shortest_path_to_roots(
    forward: &BTreeMap<SymbolId, Vec<SymbolId>>,
    start: &SymbolId,
    roots: &BTreeSet<SymbolId>,
) -> Option<Vec<SymbolId>> {
    if roots.contains(start) {
        return Some(vec![start.clone()]);
    }
    // Breadth-first over forward edges, recording each node's predecessor so the
    // path can be walked back once a root is reached.
    let mut prev: BTreeMap<SymbolId, SymbolId> = BTreeMap::new();
    let mut seen: BTreeSet<SymbolId> = BTreeSet::new();
    let mut queue: VecDeque<SymbolId> = VecDeque::new();
    seen.insert(start.clone());
    queue.push_back(start.clone());

    while let Some(node) = queue.pop_front() {
        let Some(callees) = forward.get(&node) else {
            continue;
        };
        for callee in callees {
            if !seen.insert(callee.clone()) {
                continue;
            }
            prev.insert(callee.clone(), node.clone());
            if roots.contains(callee) {
                return Some(reconstruct_path(&prev, start, callee));
            }
            queue.push_back(callee.clone());
        }
    }
    None
}

/// Walk the predecessor chain from `end` back to `start`, then reverse it into a
/// forward path `[start, …, end]`.
fn reconstruct_path(
    prev: &BTreeMap<SymbolId, SymbolId>,
    start: &SymbolId,
    end: &SymbolId,
) -> Vec<SymbolId> {
    let mut path = vec![end.clone()];
    let mut cur = end;
    while cur != start {
        let p = &prev[cur];
        path.push(p.clone());
        cur = p;
    }
    path.reverse();
    path
}

/// The reference engine: a multi-source breadth-first search over the reverse
/// call/use graph. Batch-only and dependency-free — the honest baseline the
/// incremental DBSP engine is validated against.
#[derive(Clone, Copy, Debug, Default)]
pub struct NaiveEngine;

impl EffectEngine for NaiveEngine {
    fn propagate(&self, facts: &FactSet) -> BTreeMap<SymbolId, EffectSummary> {
        propagate(facts)
    }
}

/// Propagate effects backward from the roots to a fixed point.
///
/// Multi-source breadth-first search over the reverse graph (callee -> callers)
/// with a monotone set-union lattice per effect category. Because inserts only
/// ever add to a `BTreeSet`, the worklist drains even through cycles and
/// recursion; breadth-first order yields short evidence paths. This free
/// function is the body of [`NaiveEngine::propagate`]; callers that want the
/// engine seam should depend on the [`EffectEngine`] trait instead.
pub fn propagate(facts: &FactSet) -> BTreeMap<SymbolId, EffectSummary> {
    // Reverse adjacency: callee -> the symbols that use it. Type edges are
    // signature-type dependencies, not calls, so they are excluded — a `Type`
    // edge must never propagate a runtime effect.
    let mut callers_of: BTreeMap<SymbolId, Vec<SymbolId>> = BTreeMap::new();
    for edge in facts.edges.iter().filter(|e| e.kind.carries_effects()) {
        callers_of
            .entry(edge.callee.clone())
            .or_default()
            .push(edge.caller.clone());
    }

    let mut summaries: BTreeMap<SymbolId, EffectSummary> = BTreeMap::new();
    let mut work: VecDeque<SymbolId> = VecDeque::new();

    // Seed each root's own summary with its effect, evidence path = [root].
    for root in &facts.roots {
        let summary = summaries.entry(root.symbol.clone()).or_default();
        if summary.effects.insert(root.effect) {
            summary
                .evidence
                .entry(root.effect)
                .or_insert_with(|| vec![root.symbol.clone()]);
        }
        work.push_back(root.symbol.clone());
    }

    while let Some(sym) = work.pop_front() {
        // Snapshot the popped symbol's effects and their evidence paths so we
        // can push them up to each caller without aliasing the map.
        let popped: Vec<(Effect, Vec<SymbolId>)> = match summaries.get(&sym) {
            Some(s) => s
                .effects
                .iter()
                .map(|e| (*e, s.evidence.get(e).cloned().unwrap_or_default()))
                .collect(),
            None => continue,
        };

        let Some(callers) = callers_of.get(&sym) else {
            continue;
        };

        for caller in callers.clone() {
            let mut changed = false;
            let entry = summaries.entry(caller.clone()).or_default();
            for (effect, path) in &popped {
                if entry.effects.insert(*effect) {
                    // New effect for this caller: its evidence path is the
                    // caller prepended to the popped symbol's path.
                    let mut caller_path = Vec::with_capacity(path.len() + 1);
                    caller_path.push(caller.clone());
                    caller_path.extend(path.iter().cloned());
                    entry.evidence.insert(*effect, caller_path);
                    changed = true;
                }
            }
            if changed {
                work.push_back(caller);
            }
        }
    }

    summaries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{Edge, EffectRoot};
    use crate::test_support::edge;

    fn fs_root(sym: &str) -> EffectRoot {
        EffectRoot {
            symbol: sym.to_string(),
            effect: Effect::Fs,
        }
    }

    #[test]
    fn linear_chain_carries_effect_with_full_evidence_path() {
        // top -> mid -> root
        let mut facts = FactSet::default();
        facts.add_edge(edge("top", "mid"));
        facts.add_edge(edge("mid", "root"));
        facts.add_root(fs_root("root"));

        let summaries = propagate(&facts);

        let top = &summaries["top"];
        assert!(top.effects.contains(&Effect::Fs));
        let path = &top.evidence[&Effect::Fs];
        assert_eq!(
            path,
            &vec!["top".to_string(), "mid".to_string(), "root".to_string()]
        );
        assert_eq!(path.len(), 3);
        assert_eq!(path.last().unwrap(), "root");
    }

    #[test]
    fn cycle_terminates_and_both_nodes_carry_effect() {
        // a <-> b, and a -> root, so both can reach the effect.
        let mut facts = FactSet::default();
        facts.add_edge(edge("a", "b"));
        facts.add_edge(edge("b", "a"));
        facts.add_edge(edge("a", "root"));
        facts.add_root(fs_root("root"));

        let summaries = propagate(&facts);

        assert!(summaries["a"].effects.contains(&Effect::Fs));
        assert!(summaries["b"].effects.contains(&Effect::Fs));
    }

    #[test]
    fn type_edges_do_not_propagate_effects() {
        // `foo` depends on the TYPE `File` (a signature dependency), and `File`
        // is (contrived) an effect root. A type edge is not a call, so `foo`
        // must NOT inherit the effect through it.
        let mut facts = FactSet::default();
        facts.add_edge(Edge::type_dep("foo", "File", "foo.rs", 1));
        facts.add_root(fs_root("File"));
        // A genuine call edge into a real fs root, to prove propagation itself
        // still works and only the type edge is excluded.
        facts.add_edge(edge("bar", "open"));
        facts.add_root(fs_root("open"));

        let summaries = propagate(&facts);

        // `foo` reaches nothing through its type edge.
        let foo = summaries.get("foo").cloned().unwrap_or_default();
        assert!(
            foo.effects.is_empty(),
            "type edge leaked an effect into foo"
        );
        // `bar` still carries fs through its call edge.
        assert!(summaries["bar"].effects.contains(&Effect::Fs));
    }

    #[test]
    fn pure_function_with_no_path_has_empty_summary() {
        let mut facts = FactSet::default();
        facts.add_edge(edge("caller", "root"));
        facts.add_root(fs_root("root"));
        // "pure" reaches nothing effectful.
        facts.add_edge(edge("pure", "also_pure"));

        let summaries = propagate(&facts);

        let pure = summaries.get("pure").cloned().unwrap_or_default();
        assert!(pure.effects.is_empty());
        assert!(pure.evidence.is_empty());
    }
}
