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

/// Propagate effects backward from the roots to a fixed point.
///
/// Multi-source breadth-first search over the reverse graph (callee -> callers)
/// with a monotone set-union lattice per effect category. Because inserts only
/// ever add to a `BTreeSet`, the worklist drains even through cycles and
/// recursion; breadth-first order yields short evidence paths.
pub fn propagate(facts: &FactSet) -> BTreeMap<SymbolId, EffectSummary> {
    // Reverse adjacency: callee -> the symbols that use it.
    let mut callers_of: BTreeMap<SymbolId, Vec<SymbolId>> = BTreeMap::new();
    for edge in &facts.edges {
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
    use crate::facts::{Edge, EdgeKind, EffectRoot};

    fn edge(caller: &str, callee: &str) -> Edge {
        Edge {
            caller: caller.to_string(),
            callee: callee.to_string(),
            kind: EdgeKind::Call,
            evidence_file: "x.rs".to_string(),
            evidence_line: 1,
        }
    }

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
