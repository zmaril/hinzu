//! The fact schema v0: the language-independent vocabulary the propagation
//! engine reasons over. Adapters (Rust via SCIP, TypeScript via the compiler
//! API) normalize their language into these types; nothing below this line
//! knows what language produced the facts.

use std::collections::BTreeMap;

/// A stable, structured identity for a callable — the SCIP symbol style
/// (package/crate + version + descriptor path). Survives repeated analysis.
pub type SymbolId = String;

/// The languages hinzu adapters target first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Rust,
    TypeScript,
}

/// The closed set of effect categories. An operation either belongs to one of
/// these or is pure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Effect {
    Fs,
    Net,
    Db,
    Clock,
    Random,
    Process,
    Env,
}

impl Effect {
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
        }
    }
}

/// A callable, with the source provenance a policy region matches on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Definition {
    pub id: SymbolId,
    pub display: String,
    pub language: Language,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
}

/// Whether an edge is a resolved call or a bare reference to a symbol (for
/// example, passing a function as a callback). Both carry effects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeKind {
    Call,
    Reference,
}

/// "caller uses callee" — the unit of the call/use graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    pub caller: SymbolId,
    pub callee: SymbolId,
    pub kind: EdgeKind,
    pub evidence_file: String,
    pub evidence_line: u32,
}

/// A seed: an operation that *is* an effect, tagged with its category.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectRoot {
    pub symbol: SymbolId,
    pub effect: Effect,
}

/// The normalized fact tables an adapter produces and the engine consumes.
#[derive(Clone, Debug, Default)]
pub struct FactSet {
    pub defs: BTreeMap<SymbolId, Definition>,
    pub edges: Vec<Edge>,
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
}
