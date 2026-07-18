//! The durable fact store: SQLite, embedded (via the bundled `rusqlite`), no
//! system library needed. It holds the source-of-truth facts an adapter
//! extracts and the effect summaries the engine derives, so repeated runs and
//! cross-revision comparison have somewhere to land. The schema mirrors the
//! fact schema v0 in [`crate::facts`].

use std::collections::BTreeMap;
use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::effects::EffectSummary;
use crate::facts::{
    Definition, Edge, EdgeKind, EdgeResolution, Effect, EffectRoot, FactSet, Language, SymbolId,
};

/// The SQLite schema, created if absent. Columns map one-to-one onto the fact
/// schema; enums are stored as their lowercase string forms.
const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS definitions (
    id         TEXT PRIMARY KEY,
    display    TEXT NOT NULL,
    language   TEXT NOT NULL,
    file       TEXT NOT NULL,
    line_start INTEGER NOT NULL,
    line_end   INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS edges (
    caller        TEXT NOT NULL,
    callee        TEXT NOT NULL,
    kind          TEXT NOT NULL,
    resolution    TEXT NOT NULL,
    evidence_file TEXT NOT NULL,
    evidence_line INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS effect_roots (
    symbol TEXT NOT NULL,
    effect TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS effect_summaries (
    symbol        TEXT NOT NULL,
    effect        TEXT NOT NULL,
    evidence_path TEXT NOT NULL
);
";

/// A handle on the SQLite fact store.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (creating if needed) a fact store at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path.as_ref())
            .with_context(|| format!("opening fact store at {}", path.as_ref().display()))?;
        Self::from_conn(conn)
    }

    /// Open an in-memory fact store — used by tests and by `hinzu check` when
    /// no `--db` path is given.
    pub fn open_in_memory() -> Result<Self> {
        Self::from_conn(Connection::open_in_memory()?)
    }

    fn from_conn(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA)
            .context("creating the fact-store schema")?;
        Ok(Store { conn })
    }

    /// Insert a whole fact set. Idempotent on definitions (upsert by id);
    /// edges and roots append.
    pub fn insert_facts(&mut self, facts: &FactSet) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut def = tx.prepare(
                "INSERT OR REPLACE INTO definitions \
                 (id, display, language, file, line_start, line_end) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for d in facts.defs.values() {
                def.execute((
                    &d.id,
                    &d.display,
                    d.language.as_str(),
                    &d.file,
                    d.line_start,
                    d.line_end,
                ))?;
            }

            let mut edge = tx.prepare(
                "INSERT INTO edges \
                 (caller, callee, kind, resolution, evidence_file, evidence_line) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for e in &facts.edges {
                edge.execute((
                    &e.caller,
                    &e.callee,
                    e.kind.as_str(),
                    e.resolution.as_str(),
                    &e.evidence_file,
                    e.evidence_line,
                ))?;
            }

            let mut root =
                tx.prepare("INSERT INTO effect_roots (symbol, effect) VALUES (?1, ?2)")?;
            for r in &facts.roots {
                root.execute((&r.symbol, r.effect.as_str()))?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Load every fact back out of the store.
    pub fn load_facts(&self) -> Result<FactSet> {
        let mut facts = FactSet::default();

        let mut defs = self
            .conn
            .prepare("SELECT id, display, language, file, line_start, line_end FROM definitions")?;
        let rows = defs.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, u32>(4)?,
                r.get::<_, u32>(5)?,
            ))
        })?;
        for row in rows {
            let (id, display, language, file, line_start, line_end) = row?;
            facts.add_def(Definition {
                id,
                display,
                language: Language::from_str(&language)?,
                file,
                line_start,
                line_end,
            });
        }

        let mut edges = self.conn.prepare(
            "SELECT caller, callee, kind, resolution, evidence_file, evidence_line FROM edges",
        )?;
        let rows = edges.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, u32>(5)?,
            ))
        })?;
        for row in rows {
            let (caller, callee, kind, resolution, evidence_file, evidence_line) = row?;
            facts.add_edge(Edge {
                caller,
                callee,
                kind: EdgeKind::from_str(&kind)?,
                resolution: EdgeResolution::from_str(&resolution)?,
                evidence_file,
                evidence_line,
            });
        }

        let mut roots = self
            .conn
            .prepare("SELECT symbol, effect FROM effect_roots")?;
        let rows = roots.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (symbol, effect) = row?;
            facts.add_root(EffectRoot {
                symbol,
                effect: Effect::from_str(&effect)?,
            });
        }

        Ok(facts)
    }

    /// Persist the derived effect summaries. Replaces any prior summaries so a
    /// re-run reflects the current facts.
    pub fn write_summaries(&mut self, summaries: &BTreeMap<SymbolId, EffectSummary>) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM effect_summaries", [])?;
        {
            let mut ins = tx.prepare(
                "INSERT INTO effect_summaries (symbol, effect, evidence_path) VALUES (?1, ?2, ?3)",
            )?;
            for (symbol, summary) in summaries {
                for effect in &summary.effects {
                    let path = summary
                        .evidence
                        .get(effect)
                        .cloned()
                        .unwrap_or_default()
                        .join("->");
                    ins.execute((symbol, effect.as_str(), &path))?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Load the persisted effect summaries back, reconstructing each effect's
    /// evidence path from its `->`-joined form.
    pub fn load_summaries(&self) -> Result<BTreeMap<SymbolId, EffectSummary>> {
        let mut summaries: BTreeMap<SymbolId, EffectSummary> = BTreeMap::new();
        let mut stmt = self
            .conn
            .prepare("SELECT symbol, effect, evidence_path FROM effect_summaries")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (symbol, effect, path) = row?;
            let effect = Effect::from_str(&effect)?;
            let entry = summaries.entry(symbol).or_default();
            entry.effects.insert(effect);
            let steps: Vec<SymbolId> = if path.is_empty() {
                Vec::new()
            } else {
                path.split("->").map(|s| s.to_string()).collect()
            };
            entry.evidence.insert(effect, steps);
        }
        Ok(summaries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{EffectEngine, NaiveEngine};

    fn one_def(id: &str, file: &str, line_start: u32) -> Definition {
        Definition {
            id: id.to_string(),
            display: id.rsplit("::").next().unwrap_or(id).to_string(),
            language: Language::Rust,
            file: file.to_string(),
            line_start,
            line_end: line_start + 4,
        }
    }

    /// A small graph: `outer` calls `inner`, which references a net root. The
    /// reference edge exercises resolution provenance across the round trip.
    fn sample_facts() -> FactSet {
        let mut facts = FactSet::default();
        facts.add_def(one_def("pkg::app::outer", "src/app.rs", 3));
        facts.add_def(one_def("pkg::wire::inner", "src/wire.rs", 42));
        facts.add_edge(Edge::call(
            "pkg::app::outer",
            "pkg::wire::inner",
            "src/app.rs",
            5,
        ));
        facts.add_edge(Edge::reference(
            "pkg::wire::inner",
            "tokio::net::TcpStream::connect",
            "src/wire.rs",
            44,
        ));
        facts.add_root(EffectRoot {
            symbol: "tokio::net::TcpStream::connect".to_string(),
            effect: Effect::Net,
        });
        facts
    }

    #[test]
    fn facts_round_trip_through_the_store() {
        let facts = sample_facts();
        let mut store = Store::open_in_memory().unwrap();
        store.insert_facts(&facts).unwrap();
        let loaded = store.load_facts().unwrap();

        assert_eq!(loaded.defs, facts.defs);
        assert_eq!(loaded.edges, facts.edges);
        assert_eq!(loaded.roots, facts.roots);
        // The reference edge kept its resolution provenance across the trip.
        let ref_edge = loaded
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Reference)
            .unwrap();
        assert_eq!(ref_edge.resolution, EdgeResolution::Reference);
    }

    #[test]
    fn summaries_round_trip_through_the_store() {
        let facts = sample_facts();
        let summaries = NaiveEngine.propagate(&facts);
        let mut store = Store::open_in_memory().unwrap();
        store.write_summaries(&summaries).unwrap();
        let loaded = store.load_summaries().unwrap();

        let original = &summaries["pkg::app::outer"];
        let reloaded = &loaded["pkg::app::outer"];
        assert_eq!(reloaded.effects, original.effects);
        assert_eq!(
            reloaded.evidence[&Effect::Net],
            original.evidence[&Effect::Net]
        );
    }
}
