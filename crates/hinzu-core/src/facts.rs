//! The fact schema v0: the language-independent vocabulary the propagation
//! engine reasons over. Adapters (Rust via a StableMIR driver, TypeScript via
//! the compiler API) normalize their language into these types; nothing below
//! this line knows what language produced the facts.

use std::collections::BTreeMap;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A stable, structured identity for a callable — the SCIP symbol style
/// (package/crate + version + descriptor path). Survives repeated analysis.
pub type SymbolId = String;

/// The languages hinzu adapters target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Rust,
    TypeScript,
    Python,
    Go,
}

impl Language {
    /// The lowercase spelling used in the fact store and JSON.
    pub fn as_str(&self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::TypeScript => "typescript",
            Language::Python => "python",
            Language::Go => "go",
        }
    }
}

impl FromStr for Language {
    type Err = anyhow::Error;

    /// Parse the store/JSON spelling back into a language.
    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "rust" => Ok(Language::Rust),
            "typescript" => Ok(Language::TypeScript),
            "python" => Ok(Language::Python),
            "go" => Ok(Language::Go),
            other => anyhow::bail!("unknown language: {other}"),
        }
    }
}

/// The closed set of effect categories, plus [`Effect::Unknown`] — the honest
/// marker for "we could not tell." An operation either belongs to one of the
/// seven real categories, is `Unknown`, or is pure.
///
/// `Unknown` is not a real-world effect: it is uncertainty made first-class, so
/// that an unseen external call (a foreign callee with no body, or an
/// unresolved indirect call) *propagates* up the call graph exactly like an
/// effect instead of being silently read as pure. It rides the same
/// root-seeding, propagation, evidence-path, and store machinery as the real
/// effects; the policy check treats it specially, governed by
/// `[analysis] on_unknown` rather than by a region's forbid/allow list. Because
/// `Unknown` is never a real effect, it is excluded from a region's effect
/// vocabulary — see [`Effect::REAL`] and the policy parser.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effect {
    Fs,
    Net,
    Db,
    Clock,
    Random,
    Process,
    Env,
    /// Heap allocation — the "may allocate" effect. `Vec::push`, `Box::new`,
    /// `String` growth, collection inserts, `format!`, `.collect()`, `Rc`/`Arc`
    /// construction. Tracked like any other effect so a performance-sensitive
    /// region can forbid it; over-approximate on purpose (an API that *may*
    /// allocate is marked, even if a given call does not).
    Alloc,
    /// "We could not tell." An unseen external call that no annotation, root, or
    /// trusted-pure baseline resolved — carried up the graph so a policy can
    /// refuse to certify anything that reaches it.
    Unknown,
}

impl Effect {
    /// The real effect categories — the vocabulary a policy region may forbid or
    /// allow. Deliberately excludes [`Effect::Unknown`], which is an uncertainty
    /// marker governed by `[analysis] on_unknown`, not a category a region can
    /// name.
    pub const REAL: [Effect; 8] = [
        Effect::Fs,
        Effect::Net,
        Effect::Db,
        Effect::Clock,
        Effect::Random,
        Effect::Process,
        Effect::Env,
        Effect::Alloc,
    ];

    /// The lowercase policy-file spelling of this effect.
    pub fn as_str(&self) -> &'static str {
        match self {
            Effect::Fs => "fs",
            Effect::Net => "net",
            Effect::Db => "db",
            Effect::Clock => "clock",
            Effect::Random => "random",
            Effect::Process => "process",
            Effect::Env => "env",
            Effect::Alloc => "alloc",
            Effect::Unknown => "unknown",
        }
    }
}

impl FromStr for Effect {
    type Err = anyhow::Error;

    /// Parse the policy-file / store spelling back into an effect category.
    /// Accepts `"unknown"` so derived summaries round-trip through the store;
    /// region and root parsing reject it separately (it is not a category a
    /// policy can name — see [`Effect::REAL`]).
    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "fs" => Ok(Effect::Fs),
            "net" => Ok(Effect::Net),
            "db" => Ok(Effect::Db),
            "clock" => Ok(Effect::Clock),
            "random" => Ok(Effect::Random),
            "process" => Ok(Effect::Process),
            "env" => Ok(Effect::Env),
            "alloc" => Ok(Effect::Alloc),
            "unknown" => Ok(Effect::Unknown),
            other => anyhow::bail!("unknown effect: {other}"),
        }
    }
}

/// A callable, with the source provenance a policy region matches on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Definition {
    pub id: SymbolId,
    pub display: String,
    pub language: Language,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
}

/// Build a [`Definition`] for tests. The `display` is the last `::`-segment of
/// `id`, matching the extractor's convention. Shared across the crate's test
/// modules so each one doesn't hand-roll the same struct literal.
#[cfg(test)]
pub(crate) fn make_def(id: &str, file: &str, line_start: u32, line_end: u32) -> Definition {
    Definition {
        id: id.to_string(),
        display: id.rsplit("::").next().unwrap_or(id).to_string(),
        language: Language::Rust,
        file: file.to_string(),
        line_start,
        line_end,
    }
}

/// What kind of dependency an edge records.
///
/// - `Call` — a resolved call: the caller invokes the callee.
/// - `Reference` — a bare reference to a symbol, for example passing a function
///   as a callback. Both `Call` and `Reference` carry effects.
/// - `Type` — a **signature type-dependency**: a function depends on a type
///   named in its parameter types, return type, or (for a class) its
///   `extends`/`implements` bounds. This is *not* a call — a function taking a
///   `File` parameter does not itself perform any filesystem effect — so a
///   `Type` edge deliberately does **not** propagate runtime effects (see
///   [`EdgeKind::carries_effects`]). It exists so the porting dependency graph
///   knows a function cannot be ported until the types in its signature are:
///   `--from` closures follow `Type` edges. Adapters that resolve types (the
///   TypeScript and Rust drivers today; an LSP/tree-sitter rung later) emit it;
///   a call-only adapter simply omits it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EdgeKind {
    Call,
    Reference,
    Type,
}

impl EdgeKind {
    /// The lowercase store/JSON spelling of this edge kind.
    pub fn as_str(&self) -> &'static str {
        match self {
            EdgeKind::Call => "call",
            EdgeKind::Reference => "reference",
            EdgeKind::Type => "type",
        }
    }

    /// Whether this edge participates in effect propagation and the reverse
    /// call graph the checker walks. `Call` and `Reference` carry effects; a
    /// `Type` edge is a signature-type dependency for porting closures and must
    /// **not** propagate runtime effects, so it is excluded from the effect
    /// engine, the root-seeding, and every reverse-call-graph the policy check
    /// consumes.
    pub fn carries_effects(&self) -> bool {
        matches!(self, EdgeKind::Call | EdgeKind::Reference)
    }
}

impl FromStr for EdgeKind {
    type Err = anyhow::Error;

    /// Parse the store/JSON spelling back into an edge kind.
    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "call" => Ok(EdgeKind::Call),
            "reference" => Ok(EdgeKind::Reference),
            "type" => Ok(EdgeKind::Type),
            other => anyhow::bail!("unknown edge kind: {other}"),
        }
    }
}

/// How the adapter resolved an edge — the provenance the precision ladder in
/// `notes/getting-started.md` depends on. `Call` and `Reference` are the two
/// kinds v0 emits; `ValueFlow` and `Unresolved` are reserved for later rungs
/// (points-to resolution and the conservative fallback).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EdgeResolution {
    Call,
    Reference,
    ValueFlow,
    Unresolved,
}

impl EdgeResolution {
    /// The store/JSON spelling of this resolution.
    pub fn as_str(&self) -> &'static str {
        match self {
            EdgeResolution::Call => "call",
            EdgeResolution::Reference => "reference",
            EdgeResolution::ValueFlow => "value-flow",
            EdgeResolution::Unresolved => "unresolved",
        }
    }

    /// The resolution that mirrors an edge kind when the adapter records no
    /// finer provenance: a call resolves as `Call`, a reference as `Reference`.
    /// A `Type` edge is a static resolution to a type declaration, so it mirrors
    /// as `Reference`.
    pub fn for_kind(kind: EdgeKind) -> Self {
        match kind {
            EdgeKind::Call => EdgeResolution::Call,
            EdgeKind::Reference => EdgeResolution::Reference,
            EdgeKind::Type => EdgeResolution::Reference,
        }
    }
}

impl FromStr for EdgeResolution {
    type Err = anyhow::Error;

    /// Parse the store/JSON spelling back into a resolution.
    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "call" => Ok(EdgeResolution::Call),
            "reference" => Ok(EdgeResolution::Reference),
            "value-flow" => Ok(EdgeResolution::ValueFlow),
            "unresolved" => Ok(EdgeResolution::Unresolved),
            other => anyhow::bail!("unknown edge resolution: {other}"),
        }
    }
}

/// "caller uses callee" — the unit of the call/use graph.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub caller: SymbolId,
    pub callee: SymbolId,
    pub kind: EdgeKind,
    /// How the adapter resolved this edge. Defaults from `kind` when a fact
    /// source records no finer provenance (see [`Edge::call`] / [`Edge::reference`]).
    #[serde(default = "default_resolution")]
    pub resolution: EdgeResolution,
    pub evidence_file: String,
    pub evidence_line: u32,
}

/// Serde default for an edge whose JSON omits `resolution`: a plain call.
fn default_resolution() -> EdgeResolution {
    EdgeResolution::Call
}

impl Edge {
    /// A `Call` edge whose resolution mirrors the kind.
    pub fn call(caller: &str, callee: &str, evidence_file: &str, evidence_line: u32) -> Self {
        Edge {
            caller: caller.to_string(),
            callee: callee.to_string(),
            kind: EdgeKind::Call,
            resolution: EdgeResolution::Call,
            evidence_file: evidence_file.to_string(),
            evidence_line,
        }
    }

    /// A `Reference` edge whose resolution mirrors the kind.
    pub fn reference(caller: &str, callee: &str, evidence_file: &str, evidence_line: u32) -> Self {
        Edge {
            caller: caller.to_string(),
            callee: callee.to_string(),
            kind: EdgeKind::Reference,
            resolution: EdgeResolution::Reference,
            evidence_file: evidence_file.to_string(),
            evidence_line,
        }
    }

    /// A `Type` edge: a signature type-dependency from a function (or class) to a
    /// type named in its parameters, return, or `extends`/`implements` bounds.
    /// Resolves statically to the type's declaration (mirrored as `Reference`)
    /// and does **not** carry effects — see [`EdgeKind::Type`].
    pub fn type_dep(caller: &str, callee: &str, evidence_file: &str, evidence_line: u32) -> Self {
        Edge {
            caller: caller.to_string(),
            callee: callee.to_string(),
            kind: EdgeKind::Type,
            resolution: EdgeResolution::Reference,
            evidence_file: evidence_file.to_string(),
            evidence_line,
        }
    }
}

/// A seed: an operation that *is* an effect, tagged with its category.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectRoot {
    pub symbol: SymbolId,
    pub effect: Effect,
}

/// The normalized fact tables an adapter produces and the engine consumes.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FactSet {
    #[serde(default, with = "def_map")]
    pub defs: BTreeMap<SymbolId, Definition>,
    #[serde(default)]
    pub edges: Vec<Edge>,
    #[serde(default)]
    pub roots: Vec<EffectRoot>,
}

impl FactSet {
    /// Register a callable, keyed by its symbol id.
    pub fn add_def(&mut self, def: Definition) {
        self.defs.insert(def.id.clone(), def);
    }

    /// Record a "caller uses callee" edge.
    pub fn add_edge(&mut self, edge: Edge) {
        self.edges.push(edge);
    }

    /// Seed an effectful root.
    pub fn add_root(&mut self, root: EffectRoot) {
        self.roots.push(root);
    }

    /// Parse a fact set from the JSON schema `hinzu check --facts` reads: a
    /// `definitions` array plus `edges` and `effect_roots`.
    pub fn from_json(json: &str) -> anyhow::Result<Self> {
        let wire: WireFacts = serde_json::from_str(json)?;
        Ok(wire.into())
    }
}

/// The on-the-wire JSON shape: `definitions` as a flat array (adapters emit a
/// list, not a keyed map), mirroring the store's tables.
#[derive(Serialize, Deserialize)]
struct WireFacts {
    #[serde(default)]
    definitions: Vec<Definition>,
    #[serde(default)]
    edges: Vec<Edge>,
    #[serde(default)]
    effect_roots: Vec<EffectRoot>,
}

impl From<WireFacts> for FactSet {
    fn from(wire: WireFacts) -> Self {
        let mut facts = FactSet::default();
        for def in wire.definitions {
            facts.add_def(def);
        }
        facts.edges = wire.edges;
        facts.roots = wire.effect_roots;
        facts
    }
}

/// Serialize `defs` as a flat array keyed by each definition's id, so the
/// derived `FactSet` serialization matches the JSON schema.
mod def_map {
    use super::{Definition, SymbolId};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<S: Serializer>(
        map: &BTreeMap<SymbolId, Definition>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        map.values().collect::<Vec<_>>().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<BTreeMap<SymbolId, Definition>, D::Error> {
        let defs = Vec::<Definition>::deserialize(d)?;
        Ok(defs.into_iter().map(|d| (d.id.clone(), d)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_kind_type_round_trips_through_str() {
        assert_eq!(EdgeKind::Type.as_str(), "type");
        assert_eq!(EdgeKind::from_str("type").unwrap(), EdgeKind::Type);
        // Every kind survives a str round trip.
        for kind in [EdgeKind::Call, EdgeKind::Reference, EdgeKind::Type] {
            assert_eq!(EdgeKind::from_str(kind.as_str()).unwrap(), kind);
        }
    }

    #[test]
    fn only_call_and_reference_carry_effects() {
        assert!(EdgeKind::Call.carries_effects());
        assert!(EdgeKind::Reference.carries_effects());
        // A signature-type dependency is not a call — it must not propagate
        // effects.
        assert!(!EdgeKind::Type.carries_effects());
    }

    #[test]
    fn type_dep_constructor_is_a_type_edge_with_reference_resolution() {
        let e = Edge::type_dep("app::foo", "app::Widget", "foo.rs", 3);
        assert_eq!(e.kind, EdgeKind::Type);
        // A type edge resolves statically to the type's declaration, mirrored as
        // Reference — never an invalid "type" resolution.
        assert_eq!(e.resolution, EdgeResolution::Reference);
        assert_eq!(
            EdgeResolution::for_kind(EdgeKind::Type),
            EdgeResolution::Reference
        );
    }

    #[test]
    fn type_edge_survives_json_round_trip() {
        let edge = Edge::type_dep("app::foo", "app::Widget", "foo.rs", 2);
        let json = serde_json::to_string(&edge).unwrap();
        // The kind serializes with the store/JSON spelling.
        assert!(json.contains("\"kind\":\"type\""), "json was {json}");
        let back: Edge = serde_json::from_str(&json).unwrap();
        assert_eq!(back, edge);
        assert_eq!(back.kind, EdgeKind::Type);
    }

    #[test]
    fn type_edge_ingests_through_factset_from_json() {
        // The wire schema `hinzu check --facts` reads carries the type edge.
        let json = r#"{
            "definitions": [],
            "edges": [{
                "caller": "app::foo", "callee": "app::Widget",
                "kind": "type", "resolution": "reference",
                "evidence_file": "foo.rs", "evidence_line": 2
            }],
            "effect_roots": []
        }"#;
        let facts = FactSet::from_json(json).unwrap();
        assert_eq!(facts.edges.len(), 1);
        assert_eq!(facts.edges[0].kind, EdgeKind::Type);
    }
}
