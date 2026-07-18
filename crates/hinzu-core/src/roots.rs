// straitjacket-allow-file[:duplication] — `strip_generics` is a ~10-line
// generic-argument stripper this module shares in spirit with the StableMIR
// driver (crates/hinzu-rustc-driver). The driver is pinned to a nightly, uses
// `rustc_private`, and is excluded from the workspace, so it cannot depend on
// this crate; the small overlap is the honest cost of that isolation.
//! Effect-root seeding and uncertainty classification: what each call edge's
//! callee *means*. The StableMIR driver records every call edge with its
//! callee's monomorphized path, but it only sees the target crate's own bodies
//! — a call into a registry dependency like `rusqlite` is an edge whose callee
//! is a `rusqlite::…` path with no body to recurse into. This module decides,
//! for each such unseen callee, whether it is an effect root, trustworthy-pure,
//! or genuinely `Unknown`.
//!
//! ## The resolution order
//!
//! For a callee whose body the driver did not see, we resolve in this order —
//! the first rule that matches wins:
//!
//! 1. **Explicit pure annotation** (`[trust] "serde" = "pure"`): the maintainer
//!    vouches the crate is effect-free. Overrides everything below, including a
//!    built-in effect root.
//! 2. **Effect roots** — the built-in prefix table (`std::fs`, `std::net`, …
//!    plus `rusqlite`/`libsqlite3_sys` → db, `rand` → random) merged with
//!    `[roots]` rules and `[trust]` entries that name specific effects. A match
//!    seeds a root of that effect.
//! 3. **Trusted-pure baseline** — the standard library. A callee whose path is
//!    in `std`/`core`/`alloc`, or a call through a `std`/`core`/`alloc` *trait*
//!    (`<T as std::clone::Clone>::clone`), is trusted pure. Without this, every
//!    no-body `Vec::push` or `BTreeMap::insert` leaf would become `Unknown` and
//!    nothing would ever certify. The known effect roots at step 2 are the
//!    exceptions: `std::fs` stays an effect, not baseline-pure.
//! 4. **Otherwise `Unknown`** — a foreign, no-body callee nobody vouched for,
//!    or an indirect call (function pointer / `dyn`) whose target the driver
//!    could not resolve. `Unknown` propagates up the call graph like an effect,
//!    and `hinzu check` fails on it by default (see `[analysis] on_unknown`).
//!
//! `[roots]` and `[trust]` together are the design's "trusted external
//! summaries", stated in `hinzu.toml` rather than in the source, so the trust
//! list is explicit and auditable. Seeding is a pure transform over an existing
//! `FactSet`, so it is trivially testable without a live toolchain.

use std::collections::BTreeSet;
use std::str::FromStr;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::facts::{EdgeResolution, Effect, EffectRoot, FactSet, Language, SymbolId};

/// One prefix→effect rule: a callee whose path (generic arguments stripped)
/// contains `prefix` is a root of `effect`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Seed {
    prefix: String,
    effect: Effect,
}

/// The prefix table that decides which callees are effect roots, plus the
/// pure-annotation prefixes that clear a callee to trusted-pure. Built from a
/// built-in default merged with the `[roots]` and `[trust]` sections of the
/// policy file.
#[derive(Clone, Debug)]
pub struct RootSeeds {
    seeds: Vec<Seed>,
    /// Crate/path prefixes the maintainer vouched pure via `[trust] "x" =
    /// "pure"`. Matched against the whole callee name (not just the stripped
    /// path) so a trait call like `<serde_json::Value as Clone>::clone` clears
    /// too.
    pure_prefixes: Vec<String>,
}

/// The `std`/`core`/`alloc` trait-qualifier markers the trusted-pure baseline
/// recognizes in a UFCS callee name (`<T as std::clone::Clone>::clone`).
const TRUSTED_TRAIT_MARKERS: &[&str] = &[" as std::", " as core::", " as alloc::"];

/// The `std`/`core`/`alloc` path prefixes the trusted-pure baseline treats as
/// pure — the standard library is trusted.
const TRUSTED_PATH_PREFIXES: &[&str] = &["std::", "core::", "alloc::"];

/// hinzu's shipped Rust effect-annotation defaults: the standard library's I/O
/// and allocation surface plus a few common effectful crates, as a data file
/// merged in at construction. Editing the trust table means editing that file,
/// not this module.
const STD_ANNOTATIONS: &str = include_str!("../annotations/std.toml");

/// hinzu's shipped TypeScript / Node effect-annotation defaults: the Node
/// runtime's built-in effect surface, mapped to the same shared vocabulary. The
/// counterpart to `std.toml` for the TypeScript adapter's canonical external
/// symbols (`node:fs::readFileSync`, `global::fetch`). It carries no `alloc`
/// rules — there is no `alloc` effect for a garbage-collected runtime.
const NODE_ANNOTATIONS: &str = include_str!("../annotations/node.toml");

/// hinzu's shipped Python effect-annotation defaults: the CPython standard
/// library's I/O surface (plus a few well-known effectful third-party packages),
/// mapped to the same shared vocabulary. The counterpart to `std.toml` /
/// `node.toml` for the Python adapter's canonical external symbols
/// (`subprocess::run`, `builtins::open`, `pathlib::Path.mkdir`). It carries no
/// `alloc` rules — there is no `alloc` effect for a garbage-collected runtime.
const PYTHON_ANNOTATIONS: &str = include_str!("../annotations/python.toml");

impl Default for RootSeeds {
    /// The shipped Rust defaults (from `annotations/std.toml`) with no policy
    /// overrides. `pure_prefixes` carries only what that file's `[trust]`
    /// vouches pure — the genuinely-pure rest of the standard library is the
    /// trusted-pure baseline, applied after the effect table, not a prefix rule.
    fn default() -> Self {
        Self::with_base(STD_ANNOTATIONS)
    }
}

impl RootSeeds {
    /// A fresh table built from one built-in annotation file (`std.toml` or
    /// `node.toml`), before any policy overrides.
    fn with_base(base: &str) -> Self {
        let mut seeds = RootSeeds {
            seeds: Vec::new(),
            pure_prefixes: Vec::new(),
        };
        seeds
            .merge_toml(base)
            .expect("built-in annotation file is valid");
        seeds
    }

    /// The shipped defaults for a language: `std.toml` for Rust, `node.toml` for
    /// TypeScript. The two never mix — a Rust `alloc`/`Vec::push` rule must not
    /// fire on a TypeScript symbol, and a `node:fs` rule must not fire on a Rust
    /// one — so each language starts from its own base.
    pub fn for_language(language: Language) -> Self {
        match language {
            Language::Rust => Self::with_base(STD_ANNOTATIONS),
            Language::TypeScript => Self::with_base(NODE_ANNOTATIONS),
            Language::Python => Self::with_base(PYTHON_ANNOTATIONS),
        }
    }

    /// Parse the `[roots]` and `[trust]` sections of a `hinzu.toml` string and
    /// merge them onto the shipped defaults.
    ///
    /// `[roots]` maps a prefix to a single effect spelling (`"rusqlite::" =
    /// "db"`); a rule whose prefix already exists overrides the default effect
    /// for that prefix, a new prefix is appended.
    ///
    /// `[trust]` is the "trusted external summaries" list: it maps a
    /// crate/path prefix to either `"pure"` (vouch it is effect-free — clears
    /// `Unknown`) or an array of effect spellings (`["db"]` — declare specific
    /// effects, the same as a `[roots]` rule). An empty or absent section leaves
    /// the defaults untouched.
    pub fn from_toml(src: &str) -> Result<Self> {
        Self::from_toml_for(Language::Rust, src)
    }

    /// Like [`RootSeeds::from_toml`], but starting from the given language's
    /// built-in annotation base — so a TypeScript project resolves its Node
    /// built-ins and never sees a Rust `alloc` rule. The policy's `[roots]` /
    /// `[trust]` rules merge on top identically for both languages.
    pub fn from_toml_for(language: Language, src: &str) -> Result<Self> {
        let mut seeds = RootSeeds::for_language(language);
        seeds.merge_toml(src)?;
        Ok(seeds)
    }

    /// Merge the `[roots]` and `[trust]` rules of one `hinzu.toml` string into
    /// this table. Shared by [`RootSeeds::default`] (which merges the shipped
    /// `annotations/std.toml`) and [`RootSeeds::from_toml`] (which then merges
    /// the user's policy on top), so a later rule overrides an earlier one.
    fn merge_toml(&mut self, src: &str) -> Result<()> {
        let doc: RootsDoc =
            toml::from_str(src).context("parsing [roots]/[trust] from hinzu.toml")?;
        for (prefix, effect) in doc.roots {
            let effect = parse_effect(&effect)
                .with_context(|| format!("[roots] rule '{prefix}' has an unknown effect"))?;
            self.upsert_effect(prefix, effect);
        }
        for (prefix, decl) in doc.trust {
            match decl {
                TrustDecl::Pure(s) if s == "pure" => self.pure_prefixes.push(prefix),
                TrustDecl::Pure(other) => anyhow::bail!(
                    "[trust] rule '{prefix}' = \"{other}\": expected \"pure\" or a list of effects"
                ),
                TrustDecl::Effects(names) => {
                    for name in names {
                        let effect = parse_effect(&name).with_context(|| {
                            format!("[trust] rule '{prefix}' names an unknown effect")
                        })?;
                        self.upsert_effect(prefix.clone(), effect);
                    }
                }
            }
        }
        Ok(())
    }

    /// Add or override a prefix→effect rule, matching the `[roots]` override
    /// semantics: an existing prefix is retargeted, a new one appended.
    fn upsert_effect(&mut self, prefix: String, effect: Effect) {
        match self.seeds.iter_mut().find(|s| s.prefix == prefix) {
            Some(existing) => existing.effect = effect,
            None => self.seeds.push(Seed { prefix, effect }),
        }
    }

    /// Whether the whole callee name is vouched pure by a `[trust] … = "pure"`
    /// prefix. Matches on the full name (not the stripped path) so a call routed
    /// through a trait — `<anyhow::Error as std::clone::Clone>::clone` — is
    /// cleared by an `anyhow` annotation as well as an inherent `anyhow::…` call.
    fn is_annotated_pure(&self, callee: &str) -> bool {
        self.pure_prefixes.iter().any(|p| callee.contains(p))
    }

    /// The effect a callee path seeds, if any. Generic arguments are stripped
    /// first so a prefix inside a type argument (for example `rusqlite::Error`
    /// in `Result<_, rusqlite::Error>`) never seeds a root — only a genuine
    /// callee path does. A rule matches only on whole `::`-delimited segments,
    /// so `"collect"` matches `<_ as Iterator>::collect::<Vec<_>>` but not a
    /// user function named `collect_it`, and `"Vec::push"` matches
    /// `std::vec::Vec::push` but never a `MyVec::pushdown`. The rule with the
    /// most segments wins, so a specific rule overrides a broader one.
    fn effect_of(&self, callee: &str) -> Option<Effect> {
        let stripped = strip_generics(callee);
        let segments = path_segments(&stripped);
        self.seeds
            .iter()
            .filter(|s| segments_contain(&segments, &s.prefix))
            .max_by_key(|s| path_segments(&s.prefix).len())
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

    /// Seed both the effect roots ([`RootSeeds::seed`]) *and* an `Unknown` root
    /// for every unseen callee that no annotation, effect rule, or trusted-pure
    /// baseline resolved — so uncertainty propagates instead of being read as
    /// pure. Call this when `[analysis] on_unknown` is `fail` or `warn`; under
    /// `ignore`, use plain [`RootSeeds::seed`] for the old effects-only behavior.
    ///
    /// Two kinds of callee become `Unknown`: a foreign, no-body callee that fell
    /// through to step 4 of the resolution order, and an indirect call
    /// (function pointer / `dyn`) the driver marked `resolution: unresolved`.
    /// The `Unknown` root is seeded at the offending callee symbol, so the
    /// evidence path a policy reports ends exactly at what could not be resolved.
    pub fn seed_unknowns(&self, facts: &mut FactSet) {
        self.seed(facts);

        let seen = SeenCallees::from_facts(facts);
        // Callees already seeded as a real effect are accounted for and must not
        // also become `Unknown`. This covers an adapter that seeds effect roots
        // directly by declaration provenance (the TypeScript adapter's Node
        // built-ins) even if this table's own rules would not name them, so a
        // known effect is never double-reported as an uncertainty.
        let real_roots: BTreeSet<&str> = facts
            .roots
            .iter()
            .filter(|r| r.effect != Effect::Unknown)
            .map(|r| r.symbol.as_str())
            .collect();
        let mut present: BTreeSet<SymbolId> = facts
            .roots
            .iter()
            .filter(|r| r.effect == Effect::Unknown)
            .map(|r| r.symbol.clone())
            .collect();

        let mut new_roots = Vec::new();
        for edge in &facts.edges {
            // An indirect call the driver could not resolve: unknown *target*.
            let unknown = if edge.resolution == EdgeResolution::Unresolved {
                true
            } else if seen.contains(&edge.callee) || real_roots.contains(edge.callee.as_str()) {
                false
            } else {
                // A foreign, no-body callee: unknown only if nothing resolved it.
                self.classify_foreign(&edge.callee) == Resolution::Unknown
            };
            if unknown && present.insert(edge.callee.clone()) {
                new_roots.push(EffectRoot {
                    symbol: edge.callee.clone(),
                    effect: Effect::Unknown,
                });
            }
        }
        facts.roots.extend(new_roots);
    }

    /// Resolve a foreign (no-body) callee against the resolution order:
    /// explicit-pure annotation, then an effect rule, then the trusted-pure
    /// baseline, else `Unknown`. Only meaningful for callees the analyzer did
    /// not see a body for — a local callee is handled by [`SeenCallees`].
    fn classify_foreign(&self, callee: &str) -> Resolution {
        if self.is_annotated_pure(callee) {
            return Resolution::Pure;
        }
        if let Some(effect) = self.effect_of(callee) {
            return Resolution::Effect(effect);
        }
        if is_trusted_pure_baseline(callee) {
            return Resolution::Pure;
        }
        Resolution::Unknown
    }
}

/// How a foreign callee resolved. `Effect` and `Pure` mean "accounted for";
/// `Unknown` means it must propagate as uncertainty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Resolution {
    Pure,
    Effect(Effect),
    Unknown,
}

/// Whether a callee is covered by the trusted-pure baseline: the standard
/// library. True when the callee's own path is in `std`/`core`/`alloc`, or the
/// call goes through a `std`/`core`/`alloc` *trait* (a UFCS `<T as
/// std::…Trait>::method`). The known effect roots (`std::fs`, allocation, …) are
/// matched *before* this in the resolution order, so they stay effects; this
/// only clears the genuinely-pure remainder (arithmetic, slices, comparisons,
/// lazy iterator adapters) that would otherwise drown the run in `Unknown`.
fn is_trusted_pure_baseline(callee: &str) -> bool {
    let stripped = strip_generics(callee);
    let path = stripped.trim_start_matches(':');
    if TRUSTED_PATH_PREFIXES.iter().any(|p| path.starts_with(p)) {
        return true;
    }
    // A UFCS call through a std/core/alloc trait: `<Recv as std::…>::method`.
    if TRUSTED_TRAIT_MARKERS.iter().any(|m| callee.contains(m)) {
        return true;
    }
    // A UFCS call whose *receiver* type is std/core/alloc: `<std::vec::Vec<T> as
    // …>::method`. The leading `<` is followed by the receiver path (after any
    // `dyn`/`&`/`mut`/`*` qualifiers).
    if let Some(receiver) = callee.strip_prefix('<') {
        let receiver = receiver
            .trim_start_matches(|c: char| c == '&' || c == '*' || c.is_whitespace())
            .trim_start_matches("dyn ")
            .trim_start_matches("mut ")
            .trim_start();
        if TRUSTED_PATH_PREFIXES
            .iter()
            .any(|p| receiver.starts_with(p))
        {
            return true;
        }
    }
    false
}

/// The callees the analyzer already has a body for — a local definition, or a
/// call into a workspace crate whose bodies were walked. Such a callee never
/// becomes `Unknown`: its effects (if any) propagate through its own edges.
struct SeenCallees {
    /// Exact definition ids.
    ids: BTreeSet<String>,
    /// Definition ids with generic arguments stripped, so a monomorphized call
    /// (`Store::open::<&Path>`) matches its generic definition (`Store::open`).
    stripped: BTreeSet<String>,
    /// Crate names the definitions came from — the analyzed workspace crates.
    local_crates: BTreeSet<String>,
}

impl SeenCallees {
    /// Build the seen set from a fact set's definitions.
    fn from_facts(facts: &FactSet) -> Self {
        let mut ids = BTreeSet::new();
        let mut stripped = BTreeSet::new();
        let mut local_crates = BTreeSet::new();
        for id in facts.defs.keys() {
            ids.insert(id.clone());
            stripped.insert(strip_generics(id).trim_end_matches(':').to_string());
            if let Some(krate) = leading_crate(id) {
                local_crates.insert(krate);
            }
        }
        SeenCallees {
            ids,
            stripped,
            local_crates,
        }
    }

    /// Whether the analyzer already has this callee's body: an exact or
    /// generic-stripped definition match, or a callee in a local workspace crate
    /// (which covers local generics and `dyn`-dispatch to local impls).
    fn contains(&self, callee: &str) -> bool {
        if self.ids.contains(callee) {
            return true;
        }
        let stripped = strip_generics(callee);
        if self.stripped.contains(stripped.trim_end_matches(':')) {
            return true;
        }
        matches!(leading_crate(callee), Some(krate) if self.local_crates.contains(&krate))
    }
}

/// The crate a symbol path belongs to: the first `ident::` segment, skipping a
/// leading UFCS `<` and any `dyn`/`&`/`mut`/`*` qualifiers. `hinzu_core::x::y`
/// and `<dyn hinzu_core::T as …>::m` both yield `hinzu_core`; a foreign callee
/// carrying a local type only in its *arguments* (`serde_json::from::<Foo>`)
/// yields `serde_json`, so it is not mistaken for local.
fn leading_crate(symbol: &str) -> Option<String> {
    let mut ident = String::new();
    for c in symbol.chars() {
        if c == '_' || c.is_ascii_alphanumeric() {
            ident.push(c);
        } else if c == ':' && !ident.is_empty() {
            // `ident::` — the first crate-qualified segment.
            return Some(ident);
        } else {
            // A separator inside the receiver (`<`, space, `&`, …): the last
            // run of identifier chars was not crate-qualified, so reset and keep
            // scanning (this skips `dyn`, `mut`, and reference qualifiers).
            ident.clear();
        }
    }
    None
}

/// Strip balanced `<…>` generic-argument groups from a monomorphized path so
/// prefix matching runs on the callee's own path, not on its type arguments.
/// Without this, a `Result<_, rusqlite::Error>` in a callee's signature would
/// falsely seed a database root on the `?`-operator plumbing that carries it.
///
/// Runs of colons are collapsed back to `::`, because removing a turbofish
/// (`Vec::<usize>::push` → `Vec::` `::push`) leaves a doubled separator; without
/// this, a needle like `Vec::push` would miss the real callee.
fn strip_generics(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut depth = 0usize;
    let mut colons = 0usize;
    for c in name.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            ':' if depth == 0 => colons += 1,
            _ if depth == 0 => {
                if colons > 0 {
                    out.push_str(if colons >= 2 { "::" } else { ":" });
                    colons = 0;
                }
                out.push(c);
            }
            _ => {}
        }
    }
    if colons > 0 {
        out.push_str(if colons >= 2 { "::" } else { ":" });
    }
    out
}

/// Parse an effect spelling for a `[roots]`/`[trust]` rule. Rejects `"unknown"`:
/// uncertainty is what classification *produces*, never something a rule can
/// assign.
fn parse_effect(name: &str) -> Result<Effect> {
    let effect = Effect::from_str(name)?;
    if effect == Effect::Unknown {
        anyhow::bail!("'unknown' is not an assignable effect; it is what classification produces");
    }
    Ok(effect)
}

/// Split a (generics-stripped) path into its non-empty `::`-delimited segments.
/// A UFCS strip leaves a leading `::` (`::collect`), so empties are dropped:
/// `::collect` → `["collect"]`, `std::vec::Vec::push` → `["std","vec","Vec",
/// "push"]`.
fn path_segments(path: &str) -> Vec<&str> {
    path.split("::").filter(|s| !s.is_empty()).collect()
}

/// Whether `needle`'s segments appear as a consecutive run inside `haystack`'s
/// segments — segment-aligned matching, so a needle only ever matches whole path
/// components, never a substring of one.
fn segments_contain(haystack: &[&str], needle: &str) -> bool {
    let needle = path_segments(needle);
    if needle.is_empty() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_slice())
}

/// The `[roots]` and `[trust]` wire shape. `[roots]` maps a callee-path prefix
/// to a single effect spelling (`"rusqlite::" = "db"`); `[trust]` maps a prefix
/// to `"pure"` or a list of effects. Everything else in `hinzu.toml` is ignored
/// here — the policy parser reads the regions.
#[derive(Default, Deserialize)]
struct RootsDoc {
    #[serde(default)]
    roots: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    trust: std::collections::BTreeMap<String, TrustDecl>,
}

/// A `[trust]` value: `"pure"` (or any bare string, validated on merge) or a
/// list of effect spellings.
#[derive(Deserialize)]
#[serde(untagged)]
enum TrustDecl {
    Pure(String),
    Effects(Vec<String>),
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
        // A genuinely-pure std leaf: no effect rule matches; the trusted-pure
        // baseline (not the effect table) clears it.
        assert_eq!(seeds.effect_of("std::vec::Vec::<T>::len"), None);
        assert!(is_trusted_pure_baseline("std::vec::Vec::<T>::len"));
    }

    #[test]
    fn allocating_std_leaves_seed_the_alloc_effect() {
        let seeds = RootSeeds::default();
        // The turbofish form the driver actually emits.
        assert_eq!(
            seeds.effect_of("std::vec::Vec::<usize>::push"),
            Some(Effect::Alloc)
        );
        assert_eq!(
            seeds.effect_of("std::boxed::Box::<u64>::new"),
            Some(Effect::Alloc)
        );
        assert_eq!(seeds.effect_of("std::fmt::format"), Some(Effect::Alloc));
        assert_eq!(
            seeds.effect_of(
                "<std::ops::Range<usize> as std::iter::Iterator>::collect::<std::vec::Vec<usize>>"
            ),
            Some(Effect::Alloc)
        );
    }

    #[test]
    fn trusted_pure_baseline_covers_std_but_not_foreign() {
        // std path and std-trait UFCS calls are trusted pure.
        assert!(is_trusted_pure_baseline("std::cmp::max::<usize>"));
        assert!(is_trusted_pure_baseline(
            "<anyhow::Error as std::clone::Clone>::clone"
        ));
        assert!(is_trusted_pure_baseline(
            "<std::vec::Vec<u8> as std::ops::Deref>::deref"
        ));
        // A foreign inherent call is not covered — it needs an annotation.
        assert!(!is_trusted_pure_baseline("toml::from_str::<T>"));
        assert!(!is_trusted_pure_baseline("anyhow::__private::format_err"));
    }

    #[test]
    fn classify_resolves_annotation_then_root_then_baseline_then_unknown() {
        let seeds = RootSeeds::from_toml("[trust]\n\"anyhow\" = \"pure\"\n").unwrap();
        // Annotation wins: anyhow is vouched pure.
        assert_eq!(
            seeds.classify_foreign("anyhow::__private::format_err"),
            Resolution::Pure
        );
        // Effect rule: an allocating std leaf.
        assert_eq!(
            seeds.classify_foreign("std::vec::Vec::<usize>::push"),
            Resolution::Effect(Effect::Alloc)
        );
        // Baseline: a pure std leaf.
        assert_eq!(
            seeds.classify_foreign("std::cmp::max::<usize>"),
            Resolution::Pure
        );
        // Nothing resolves it: Unknown.
        assert_eq!(
            seeds.classify_foreign("toml::from_str::<T>"),
            Resolution::Unknown
        );
    }

    #[test]
    fn seed_unknowns_flags_only_unvouched_foreign_calls() {
        let mut facts = FactSet::default();
        facts.add_def(crate::facts::Definition {
            id: "app::run".to_string(),
            display: "run".to_string(),
            language: crate::facts::Language::Rust,
            file: "src/lib.rs".to_string(),
            line_start: 1,
            line_end: 3,
        });
        // A foreign no-body call nobody vouched for -> Unknown.
        facts.add_edge(Edge::call(
            "app::run",
            "toml::from_str::<Cfg>",
            "src/lib.rs",
            2,
        ));
        // An allocating std leaf -> alloc (not Unknown).
        facts.add_edge(Edge::call(
            "app::run",
            "std::vec::Vec::<u8>::push",
            "src/lib.rs",
            2,
        ));
        // A pure std leaf -> nothing.
        facts.add_edge(Edge::call(
            "app::run",
            "std::cmp::max::<u8>",
            "src/lib.rs",
            2,
        ));

        RootSeeds::default().seed_unknowns(&mut facts);

        let unknowns: BTreeSet<&str> = facts
            .roots
            .iter()
            .filter(|r| r.effect == Effect::Unknown)
            .map(|r| r.symbol.as_str())
            .collect();
        assert_eq!(unknowns, BTreeSet::from(["toml::from_str::<Cfg>"]));
        assert!(facts
            .roots
            .iter()
            .any(|r| r.effect == Effect::Alloc && r.symbol == "std::vec::Vec::<u8>::push"));
    }

    #[test]
    fn seed_unknowns_flags_unresolved_indirect_calls() {
        let mut facts = FactSet::default();
        facts.add_edge(crate::facts::Edge {
            caller: "app::dispatch".to_string(),
            callee: "<indirect>".to_string(),
            kind: crate::facts::EdgeKind::Call,
            resolution: EdgeResolution::Unresolved,
            evidence_file: "src/lib.rs".to_string(),
            evidence_line: 9,
        });
        RootSeeds::default().seed_unknowns(&mut facts);
        assert!(facts
            .roots
            .iter()
            .any(|r| r.effect == Effect::Unknown && r.symbol == "<indirect>"));
    }

    #[test]
    fn a_local_crate_callee_is_never_unknown() {
        let mut facts = FactSet::default();
        facts.add_def(crate::facts::Definition {
            id: "hinzu_core::store::Store::open".to_string(),
            display: "open".to_string(),
            language: crate::facts::Language::Rust,
            file: "src/store.rs".to_string(),
            line_start: 1,
            line_end: 3,
        });
        // The monomorphized call carries a turbofish the generic def lacks; it
        // must still count as seen (a local-crate callee), not Unknown.
        facts.add_edge(Edge::call(
            "hinzu_core::lib::check",
            "hinzu_core::store::Store::open::<&std::path::Path>",
            "src/lib.rs",
            2,
        ));
        RootSeeds::default().seed_unknowns(&mut facts);
        assert!(!facts.roots.iter().any(|r| r.effect == Effect::Unknown));
    }

    #[test]
    fn empty_roots_section_keeps_defaults() {
        let seeds = RootSeeds::from_toml("[region.core]\npaths = []\n").unwrap();
        assert_eq!(
            seeds.effect_of("rusqlite::Connection::prepare"),
            Some(Effect::Db)
        );
    }

    #[test]
    fn trust_pure_clears_an_unknown() {
        let src = "[trust]\n\"toml\" = \"pure\"\n\"serde_json\" = \"pure\"\n";
        let seeds = RootSeeds::from_toml(src).unwrap();
        assert_eq!(
            seeds.classify_foreign("toml::from_str::<T>"),
            Resolution::Pure
        );
        assert_eq!(
            seeds.classify_foreign("serde_json::from_str::<T>"),
            Resolution::Pure
        );
    }

    #[test]
    fn trust_effects_declare_specific_effects() {
        let src = "[trust]\n\"redis\" = [\"net\"]\n";
        let seeds = RootSeeds::from_toml(src).unwrap();
        assert_eq!(
            seeds.effect_of("redis::Client::get::<&str>"),
            Some(Effect::Net)
        );
    }

    #[test]
    fn typescript_base_resolves_node_builtins_and_omits_rust_alloc() {
        // The TypeScript base (`node.toml`) maps the adapter's canonical symbols
        // to the shared vocabulary.
        let seeds = RootSeeds::for_language(Language::TypeScript);
        assert_eq!(seeds.effect_of("node:fs::readFileSync"), Some(Effect::Fs));
        assert_eq!(seeds.effect_of("global::fetch"), Some(Effect::Net));
        assert_eq!(seeds.effect_of("global::Math.random"), Some(Effect::Random));
        assert_eq!(
            seeds.effect_of("node:child_process::spawn"),
            Some(Effect::Process)
        );
        // No `alloc` bleeds in from the Rust table: a TypeScript method named
        // `push` is not an allocation effect (there is no `alloc` for TS).
        assert_eq!(seeds.effect_of("src/list#List.push"), None);
        // And a project `[trust]` line still declares a db package's effect.
        let seeds =
            RootSeeds::from_toml_for(Language::TypeScript, "[trust]\n\"pg\" = [\"db\"]\n").unwrap();
        assert_eq!(seeds.effect_of("pg::Client::query"), Some(Effect::Db));
    }

    #[test]
    fn typescript_npm_call_is_unknown_but_node_builtin_root_is_not() {
        // A TypeScript fact set as the adapter emits it: a local function calls a
        // Node built-in (seeded as an fs root by the adapter) and an unvouched npm
        // package (no root).
        let mut facts = FactSet::default();
        facts.add_def(crate::facts::Definition {
            id: "src/io#readConfig".to_string(),
            display: "readConfig".to_string(),
            language: Language::TypeScript,
            file: "src/io.ts".to_string(),
            line_start: 1,
            line_end: 3,
        });
        facts.add_edge(Edge::call(
            "src/io#readConfig",
            "node:fs::readFileSync",
            "src/io.ts",
            2,
        ));
        facts.add_edge(Edge::call(
            "src/io#readConfig",
            "left-pad::leftPad",
            "src/io.ts",
            3,
        ));
        // The adapter seeds the Node built-in directly by declaration provenance.
        facts.add_root(EffectRoot {
            symbol: "node:fs::readFileSync".to_string(),
            effect: Effect::Fs,
        });

        RootSeeds::for_language(Language::TypeScript).seed_unknowns(&mut facts);

        let unknowns: BTreeSet<&str> = facts
            .roots
            .iter()
            .filter(|r| r.effect == Effect::Unknown)
            .map(|r| r.symbol.as_str())
            .collect();
        // The npm call is Unknown; the built-in fs root is accounted for, not
        // double-reported as an uncertainty.
        assert_eq!(unknowns, BTreeSet::from(["left-pad::leftPad"]));
        assert!(facts
            .roots
            .iter()
            .any(|r| r.effect == Effect::Fs && r.symbol == "node:fs::readFileSync"));
    }

    #[test]
    fn python_base_resolves_stdlib_and_omits_alloc() {
        // The Python base (`python.toml`) maps the adapter's canonical symbols to
        // the shared vocabulary, using the same segment-aligned matcher.
        let seeds = RootSeeds::for_language(Language::Python);
        assert_eq!(seeds.effect_of("builtins::open"), Some(Effect::Fs));
        assert_eq!(seeds.effect_of("subprocess::run"), Some(Effect::Process));
        assert_eq!(seeds.effect_of("os::system"), Some(Effect::Process));
        assert_eq!(
            seeds.effect_of("urllib::request.urlopen"),
            Some(Effect::Net)
        );
        assert_eq!(seeds.effect_of("os::environ"), Some(Effect::Env));
        assert_eq!(seeds.effect_of("time::monotonic"), Some(Effect::Clock));
        assert_eq!(seeds.effect_of("secrets::token_hex"), Some(Effect::Random));
        // A resolved pathlib I/O method is `fs`; the bare constructor is pure —
        // the adapter never emits an effect edge for `pathlib::Path`.
        assert_eq!(seeds.effect_of("pathlib::Path.mkdir"), Some(Effect::Fs));
        assert_eq!(seeds.effect_of("pathlib::Path"), None);
        assert_eq!(seeds.effect_of("pathlib::Path.with_suffix"), None);
        // No `alloc` bleeds in from the Rust table: a Python method named `push`
        // is not an allocation effect (there is no `alloc` for Python).
        assert_eq!(seeds.effect_of("src/list.py#List.push"), None);
        // And a project `[trust]` line still declares a db package's effect.
        let seeds = RootSeeds::from_toml_for(Language::Python, "[trust]\n\"psycopg\" = [\"db\"]\n")
            .unwrap();
        assert_eq!(seeds.effect_of("psycopg::connect"), Some(Effect::Db));
    }

    #[test]
    fn python_thirdparty_call_is_unknown_but_stdlib_root_is_not() {
        // A Python fact set as the adapter emits it: a local function calls a
        // stdlib effect (seeded as a process root by the adapter) and an unvouched
        // third-party package (an edge with no root).
        let mut facts = FactSet::default();
        facts.add_def(crate::facts::Definition {
            id: "src/ctx.py#run".to_string(),
            display: "run".to_string(),
            language: Language::Python,
            file: "src/ctx.py".to_string(),
            line_start: 1,
            line_end: 3,
        });
        facts.add_edge(Edge::call(
            "src/ctx.py#run",
            "subprocess::run",
            "src/ctx.py",
            2,
        ));
        facts.add_edge(Edge::call(
            "src/ctx.py#run",
            "yaml::safe_load",
            "src/ctx.py",
            3,
        ));
        // The adapter seeds the stdlib effect directly by declaration provenance.
        facts.add_root(EffectRoot {
            symbol: "subprocess::run".to_string(),
            effect: Effect::Process,
        });

        RootSeeds::for_language(Language::Python).seed_unknowns(&mut facts);

        let unknowns: BTreeSet<&str> = facts
            .roots
            .iter()
            .filter(|r| r.effect == Effect::Unknown)
            .map(|r| r.symbol.as_str())
            .collect();
        // The third-party call is Unknown; the stdlib process root is accounted
        // for, not double-reported as an uncertainty.
        assert_eq!(unknowns, BTreeSet::from(["yaml::safe_load"]));
        assert!(facts
            .roots
            .iter()
            .any(|r| r.effect == Effect::Process && r.symbol == "subprocess::run"));
    }
}
