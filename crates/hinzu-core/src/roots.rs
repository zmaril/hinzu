// straitjacket-allow-file[:duplication] — `strip_generics` is a ~10-line
// generic-argument stripper this module shares in spirit with the StableMIR
// driver (crates/hinzu-rustc-driver). The driver is pinned to a nightly, uses
// `rustc_private`, and is excluded from the workspace, so it cannot depend on
// this crate; the small overlap is the honest cost of that isolation.
//! Effect-root seeding: which operations *are* an effect. The StableMIR driver
//! records every call edge with its callee's monomorphized path, but it only
//! sees the target crate's own bodies — a call into a registry dependency like
//! `rusqlite` is an edge whose callee is a `rusqlite::…` path with no body to
//! recurse into. This module turns those callee paths into effect roots by
//! matching each against a configurable prefix table: the design's "trusted
//! external summaries", stated in `hinzu.toml` rather than in the source.
//!
//! A small built-in default covers the standard library (`std::fs`, `std::net`,
//! …) plus a few common effectful crates (`rusqlite`/`libsqlite3_sys` for the
//! database, `rand` for randomness); a `[roots]` section in the policy file
//! extends or overrides it. Seeding is a pure transform over an existing
//! `FactSet`, so it is trivially testable without a live toolchain.

use std::collections::BTreeSet;
use std::str::FromStr;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::facts::{Effect, EffectRoot, FactSet};

/// One prefix→effect rule: a callee whose path (generic arguments stripped)
/// contains `prefix` is a root of `effect`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Seed {
    prefix: String,
    effect: Effect,
}

/// The prefix table that decides which callees are effect roots. Built from a
/// built-in default merged with any `[roots]` rules in the policy file.
#[derive(Clone, Debug)]
pub struct RootSeeds {
    seeds: Vec<Seed>,
}

/// The built-in prefix table: the standard-library effect operations the driver
/// already knew, plus a few common effectful crates whose calls only show up as
/// edges into a registry dependency. Prefixes use the `contains`-after-strip
/// match below, so `std::fs::` catches `std::fs::read_to_string::<…>` and
/// `rusqlite::` catches `rusqlite::Connection::execute::<…>`.
const BUILTIN: &[(&str, Effect)] = &[
    ("std::fs::", Effect::Fs),
    ("std::net::", Effect::Net),
    ("std::process::", Effect::Process),
    ("std::time::", Effect::Clock),
    ("std::env::", Effect::Env),
    ("rand::", Effect::Random),
    ("rand_core::", Effect::Random),
    ("tokio::net", Effect::Net),
    ("mio::", Effect::Net),
    ("socket2::", Effect::Net),
    // The database adapter's real I/O flows through these: hinzu's own fact
    // store calls `rusqlite`, which links the bundled `libsqlite3_sys` C
    // library. Without these two, a std-only seed finds no effects in a program
    // whose I/O is all SQLite — the self-check would pass trivially.
    ("rusqlite::", Effect::Db),
    ("libsqlite3_sys::", Effect::Db),
];

impl Default for RootSeeds {
    /// The built-in table with no policy overrides.
    fn default() -> Self {
        RootSeeds {
            seeds: BUILTIN
                .iter()
                .map(|(prefix, effect)| Seed {
                    prefix: (*prefix).to_string(),
                    effect: *effect,
                })
                .collect(),
        }
    }
}

impl RootSeeds {
    /// Parse the `[roots]` section of a `hinzu.toml` string and merge it onto
    /// the built-in defaults. A rule whose prefix already exists overrides the
    /// default effect for that prefix; a new prefix is appended. An empty or
    /// absent section leaves the defaults untouched.
    pub fn from_toml(src: &str) -> Result<Self> {
        let doc: RootsDoc = toml::from_str(src).context("parsing [roots] from hinzu.toml")?;
        let mut seeds = RootSeeds::default();
        for (prefix, effect) in doc.roots {
            let effect = Effect::from_str(&effect)
                .with_context(|| format!("[roots] rule '{prefix}' has an unknown effect"))?;
            match seeds.seeds.iter_mut().find(|s| s.prefix == prefix) {
                Some(existing) => existing.effect = effect,
                None => seeds.seeds.push(Seed { prefix, effect }),
            }
        }
        Ok(seeds)
    }

    /// The effect a callee path seeds, if any. Generic arguments are stripped
    /// first so a prefix inside a type argument (for example `rusqlite::Error`
    /// in `Result<_, rusqlite::Error>`) never seeds a root — only a genuine
    /// callee path does. The longest matching prefix wins, so a specific rule
    /// overrides a broader one.
    fn effect_of(&self, callee: &str) -> Option<Effect> {
        let path = strip_generics(callee);
        self.seeds
            .iter()
            .filter(|s| path.contains(&s.prefix))
            .max_by_key(|s| s.prefix.len())
            .map(|s| s.effect)
    }

    /// Seed roots for every edge callee that matches a rule, appending to the
    /// fact set. Idempotent: a `(symbol, effect)` already present as a root — a
    /// standard-library root the driver seeded, or a prior run — is not added
    /// again, so re-seeding merged driver facts never double-counts.
    pub fn seed(&self, facts: &mut FactSet) {
        let mut present: BTreeSet<(String, Effect)> = facts
            .roots
            .iter()
            .map(|r| (r.symbol.clone(), r.effect))
            .collect();

        let mut new_roots = Vec::new();
        for edge in &facts.edges {
            if let Some(effect) = self.effect_of(&edge.callee) {
                if present.insert((edge.callee.clone(), effect)) {
                    new_roots.push(EffectRoot {
                        symbol: edge.callee.clone(),
                        effect,
                    });
                }
            }
        }
        facts.roots.extend(new_roots);
    }
}

/// Strip balanced `<…>` generic-argument groups from a monomorphized path so
/// prefix matching runs on the callee's own path, not on its type arguments.
/// Without this, a `Result<_, rusqlite::Error>` in a callee's signature would
/// falsely seed a database root on the `?`-operator plumbing that carries it.
fn strip_generics(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut depth = 0usize;
    for c in name.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// The `[roots]` wire shape: a table mapping a callee-path prefix to an effect
/// spelling (`"rusqlite::" = "db"`). Everything else in `hinzu.toml` is ignored
/// here — the policy parser reads the regions.
#[derive(Default, Deserialize)]
struct RootsDoc {
    #[serde(default)]
    roots: std::collections::BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::Edge;

    /// A fact set whose only outgoing edges are a `rusqlite` database call and a
    /// `?`-operator branch that merely mentions `rusqlite::Error` in a type
    /// argument — the exact false-positive the generic strip must avoid.
    fn facts_with_db_call() -> FactSet {
        let mut facts = FactSet::default();
        facts.add_edge(Edge::call(
            "store::open",
            "rusqlite::Connection::open_in_memory",
            "src/store.rs",
            80,
        ));
        facts.add_edge(Edge::call(
            "store::insert",
            "rusqlite::Statement::<'_>::execute::<(&str,)>",
            "src/store.rs",
            110,
        ));
        facts.add_edge(Edge::call(
            "store::insert",
            "<std::result::Result<usize, rusqlite::Error> as std::ops::Try>::branch",
            "src/store.rs",
            111,
        ));
        facts
    }

    #[test]
    fn seeds_db_roots_for_rusqlite_calls_only() {
        let mut facts = facts_with_db_call();
        RootSeeds::default().seed(&mut facts);

        let symbols: BTreeSet<&str> = facts.roots.iter().map(|r| r.symbol.as_str()).collect();
        assert!(symbols.contains("rusqlite::Connection::open_in_memory"));
        assert!(symbols.contains("rusqlite::Statement::<'_>::execute::<(&str,)>"));
        // The `?` branch only carries `rusqlite::Error` as a type argument; the
        // generic strip keeps it from seeding a spurious database root.
        assert!(!symbols
            .iter()
            .any(|s| s.contains("Try") || s.contains("branch")));
        assert!(facts.roots.iter().all(|r| r.effect == Effect::Db));
        assert_eq!(facts.roots.len(), 2);
    }

    #[test]
    fn seeding_is_idempotent_and_preserves_existing_roots() {
        let mut facts = facts_with_db_call();
        // A pre-existing std root, as the driver would have emitted.
        facts.add_edge(Edge::call(
            "adapter::read",
            "std::fs::read_to_string::<&std::path::Path>",
            "src/adapter.rs",
            5,
        ));
        RootSeeds::default().seed(&mut facts);
        let after_first = facts.roots.len();
        // Re-seeding the same facts adds nothing.
        RootSeeds::default().seed(&mut facts);
        assert_eq!(facts.roots.len(), after_first);

        let fs = facts
            .roots
            .iter()
            .find(|r| r.effect == Effect::Fs)
            .expect("std::fs seeded an fs root");
        assert_eq!(fs.symbol, "std::fs::read_to_string::<&std::path::Path>");
    }

    #[test]
    fn config_extends_and_overrides_the_defaults() {
        let src = r#"
[roots]
"redis::" = "net"
"rusqlite::" = "net"
"#;
        let seeds = RootSeeds::from_toml(src).unwrap();
        // A new prefix from config.
        assert_eq!(
            seeds.effect_of("redis::Client::get::<&str>"),
            Some(Effect::Net)
        );
        // The config rule overrides the built-in `rusqlite::` = db.
        assert_eq!(
            seeds.effect_of("rusqlite::Connection::open_in_memory"),
            Some(Effect::Net)
        );
        // Defaults the config did not touch still apply.
        assert_eq!(
            seeds.effect_of("std::fs::read_to_string::<&std::path::Path>"),
            Some(Effect::Fs)
        );
    }

    #[test]
    fn a_pure_callee_seeds_nothing() {
        let seeds = RootSeeds::default();
        assert_eq!(seeds.effect_of("hinzu_core::effects::propagate"), None);
        assert_eq!(seeds.effect_of("std::vec::Vec::<T>::push"), None);
    }

    #[test]
    fn empty_roots_section_keeps_defaults() {
        let seeds = RootSeeds::from_toml("[region.core]\npaths = []\n").unwrap();
        assert_eq!(
            seeds.effect_of("rusqlite::Connection::prepare"),
            Some(Effect::Db)
        );
    }
}
