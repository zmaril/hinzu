//! A pure transform from a hinzu [`ApiReport`](crate::api::ApiReport) into the
//! two JSON documents the `fluessig` binding generator consumes: an op-layer
//! `api.json` (interfaces + ops + DTO models + synthesized unions) and a
//! `catalog.json` (whose only load-bearing content here is the `enums` lifted
//! from string-literal union type aliases).
//!
//! ## Why this lives in hinzu-core
//!
//! Like [`crate::api::build_api`], this is a **pure** in-memory transform: it
//! reads no files and spawns no processes, so it stays inside hinzu-core's
//! functional-core region. The CLI (`hinzu api-fluessig`) does the reading and
//! writing.
//!
//! ## The hard part, and how it degrades honestly
//!
//! hinzu's API report renders every type as a **string** produced by the
//! TypeScript type checker (`"Promise<SpawnResponse | ErrorResponse>"`,
//! `"boolean | undefined"`, `"FileDiff[]"`, `"\"error\""`). fluessig instead
//! wants a small structured [`FlType`] (a closed scalar set, model/enum refs,
//! lists, nullables, named unions). [`Converter::parse_type`] bridges the two
//! for the common shapes and **falls back to `Json`** on anything it cannot
//! model — every fallback is counted in [`Stats`] so the feasibility numbers are
//! honest rather than silently lossy.
//!
//! ## Mapping decisions (documented, so the coverage numbers are legible)
//!
//! * `interface` / `record` items → `models[]`; a `class` → an `interfaces[]`
//!   entry whose ops are that class's `method` items (matched by receiver); free
//!   `function` items → one flat interface named for the package.
//! * A `typeAlias` whose target is an all-string-literal union
//!   (`"a" | "b" | "c"`) → a **named** catalog enum. An *inline* string-literal
//!   union collapses to `string` (only named ones can be referenced by name).
//! * A non-null multi-member union with at least one non-literal member →
//!   a **synthesized** `unions[]` entry named by joining its members with `Or`
//!   (`RpcCommand | RpcExtensionUIResponse` → `RpcCommandOrRpcExtensionUIResponse`).
//! * `number` → `float64` (TS does not distinguish int/float — an ambiguity we
//!   record but do not treat as a degradation).
//! * An exported `const` whose declared type is representable (a scalar, or a
//!   model/union/foreign ref via [`Converter::parse_type`]) → a `consts[]` entry
//!   (see [`FlConst`]); an intrinsically-untyped one (`any`) is honestly skipped.
//! * `namespace`, `trait`, and non-union `typeAlias` items are dropped (counted).

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::api::{ApiItem, ApiReport};

// ─────────────────────────── output: api.json ───────────────────────────────

/// The version stamp fluessig's loader gates on (`format` must equal fluessig's
/// `FORMAT_VERSION`, currently 1).
#[derive(Debug, Clone, Serialize)]
pub struct FlVersions {
    pub format: u32,
    pub emitter: String,
    pub compiler: String,
}

impl Default for FlVersions {
    fn default() -> Self {
        FlVersions {
            format: 1,
            emitter: "hinzu-api-fluessig".to_string(),
            compiler: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/// The op-layer document — the serde shape of fluessig's `api.json`.
#[derive(Debug, Clone, Serialize)]
pub struct FlApiDoc {
    pub fluessig: FlVersions,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub models: Vec<FlModel>,
    pub unions: Vec<FlUnion>,
    pub interfaces: Vec<FlInterface>,
    /// Top-level EXPORTED CONSTANTS the surface declares — an
    /// `export const VERSION: string = "…"` has no op/DTO home, so it rides here
    /// (see [`FlConst`]). `skip_serializing_if` empty keeps a no-const output
    /// byte-identical to the pre-const shape (fluessig's loader defaults the key).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub consts: Vec<FlConst>,
}

/// A top-level exported constant, in the exact wire shape of fluessig's
/// `ApiConst`. Its `type` reuses the shared [`FlType`] lowering ([`FlType::Scalar`]
/// for a scalar const, a model/union/foreign ref otherwise), and `value` carries
/// the compile-time literal **only** when the const's source is a simple literal
/// (a fully-quoted string, a plain number, `true`/`false`) — a runtime expression
/// (`pkg.version || "0.0.0"`) leaves `value` absent so fluessig emits a documented
/// non-representable note rather than a broken `pub const`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FlConst {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    #[serde(rename = "type")]
    pub ty: FlType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<FlConstValue>,
}

/// The literal a [`FlConst`] carries — the untagged wire shape of fluessig's
/// `ConstValue`. Serializes as the bare JSON scalar the value naturally is
/// (`"0.80.10"`, `42`, `3.14`, `true`); the const's declared `type` remains the
/// authority for lowering, this is only the value carrier.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(untagged)]
pub enum FlConstValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

/// A DTO model (from a TS `interface` / `record`).
#[derive(Debug, Clone, Serialize)]
pub struct FlModel {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    pub fields: Vec<FlField>,
}

/// One field of a [`FlModel`].
#[derive(Debug, Clone, Serialize)]
pub struct FlField {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: FlType,
    pub nullable: bool,
}

/// A named tagged union synthesized from a multi-member TS union.
#[derive(Debug, Clone, Serialize)]
pub struct FlUnion {
    pub name: String,
    pub variants: Vec<FlUnionVariant>,
}

/// One alternative of a [`FlUnion`].
#[derive(Debug, Clone, Serialize)]
pub struct FlUnionVariant {
    pub tag: String,
    #[serde(rename = "type")]
    pub ty: FlType,
}

/// An op-bearing interface (a TS `class`, or the flat free-function group).
#[derive(Debug, Clone, Serialize)]
pub struct FlInterface {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    pub ops: Vec<FlOp>,
}

/// One operation on an interface.
#[derive(Debug, Clone, Serialize)]
pub struct FlOp {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    pub shape: String,
    #[serde(rename = "async", skip_serializing_if = "std::ops::Not::not")]
    pub is_async: bool,
    pub params: Vec<FlParam>,
    pub returns: FlType,
}

/// One parameter of a [`FlOp`].
#[derive(Debug, Clone, Serialize)]
pub struct FlParam {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: FlType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub optional: Option<bool>,
}

/// The structured type fluessig understands. Serializes with the exact
/// `#[serde(untagged)]` shape of fluessig's `ApiType`: a bare scalar string, or
/// a single-key object for model/enum/list/nullable/union/foreign.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum FlType {
    Scalar(String),
    Model {
        model: String,
    },
    Enum {
        r#enum: String,
    },
    List {
        list: Box<FlType>,
    },
    Nullable {
        nullable: Box<FlType>,
    },
    Union {
        union: String,
    },
    /// A truly-foreign type — an external/host value the surface references but
    /// fluessig has no model for (Node's `net.Server`, a `ChildProcess`, an
    /// `AbortSignal`). Rather than silently collapsing it to `Json`, it carries a
    /// [`FlForeign`] so fluessig lowers it to a typed **opaque handle**. Serializes
    /// as `{"foreign":{"name":"…","rustPath":"…"}}`, the exact wire shape of
    /// fluessig's `ApiType::Foreign` (see `ForeignType`).
    Foreign {
        foreign: FlForeign,
    },
}

/// The payload of an [`FlType::Foreign`]: the source type `name` (e.g.
/// `net.Server`, `ChildProcess`) and a best-effort `rust_path` label for the
/// generated opaque handle. Mirrors fluessig's `ForeignType`: `rust_path`
/// serializes as `rustPath`, so the object reads as a single `foreign` key.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FlForeign {
    pub name: String,
    #[serde(rename = "rustPath")]
    pub rust_path: String,
}

impl FlType {
    fn json() -> Self {
        FlType::Scalar("Json".to_string())
    }
}

// ─────────────────────────── output: catalog.json ───────────────────────────

/// The catalog document. Only `enums` carries meaning for this spike; the other
/// arrays are present-but-empty to satisfy fluessig's `deny_unknown_fields`
/// loader (models live in `api.json`, not here).
#[derive(Debug, Clone, Serialize)]
pub struct FlCatalog {
    pub fluessig: FlVersions,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub scalars: Vec<serde_json::Value>,
    pub unions: Vec<serde_json::Value>,
    pub enums: Vec<FlEnum>,
    pub entities: Vec<serde_json::Value>,
    #[serde(rename = "relationProperties")]
    pub relation_properties: Vec<serde_json::Value>,
    #[serde(rename = "valueStructs")]
    pub value_structs: Vec<serde_json::Value>,
}

/// A catalog enum (lifted from a string-literal union type alias, or a real TS
/// `enum` item).
#[derive(Debug, Clone, Serialize)]
pub struct FlEnum {
    pub name: String,
    pub variants: Vec<FlEnumVariant>,
}

/// One enum member. `value` carries the wire string when it differs from the
/// name (unused for lifted literal unions, where name == wire value).
#[derive(Debug, Clone, Serialize)]
pub struct FlEnumVariant {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

// ─────────────────────────────── stats ──────────────────────────────────────

/// The feasibility evidence: how much of the source surface round-tripped to a
/// cleanly-typed fluessig shape, and — for whatever did not — why.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Stats {
    pub items_in: usize,
    /// How many sibling-package (context) reports fed the resolution namespace.
    /// `0` in the single-report path.
    pub context_reports: usize,
    /// Of the emitted types, how many were *pulled in* from a context report by
    /// the scoped transitive closure (sibling models/enums/unions the primary
    /// references). Counted toward `models_emitted`/`enums_emitted`/
    /// `unions_synthesized` as well — this is the cross-package slice of them.
    pub context_types_pulled: usize,
    pub by_kind: BTreeMap<String, usize>,
    pub models_emitted: usize,
    /// Of `models_emitted`, how many were *minted* from an inline/anonymous
    /// object literal (rather than a named `interface`/`record` item).
    pub models_minted: usize,
    pub enums_emitted: usize,
    pub interfaces_emitted: usize,
    pub unions_synthesized: usize,
    /// Top-level exported consts emitted into `api.json`'s `consts[]` — a const
    /// whose declared type is representable (a scalar, or a model/union/foreign ref
    /// via [`Converter::parse_type`]). No longer counted in [`Self::dropped`]. An
    /// intrinsically-untyped const (`any`) stays honestly counted under
    /// `dropped["const dropped (untyped)"]` instead.
    pub consts_emitted: usize,
    /// Of `unions_synthesized`, how many were *lifted* from a top-level union /
    /// indexed-access `typeAlias` (rather than synthesized from an anonymous
    /// inline union). These are named for their alias so field/param/return refs
    /// resolve to them instead of degrading to `Json`.
    pub unions_lifted: usize,
    pub ops_total: usize,
    pub ops_clean: usize,
    pub ops_degraded: usize,
    pub fields_total: usize,
    pub fields_degraded: usize,
    pub params_total: usize,
    pub params_degraded: usize,
    pub returns_degraded: usize,
    /// How many type references were emitted as `Foreign` opaque handles instead
    /// of degrading to `Json` — the truly-external refs recovered by the
    /// opaque-handle policy.
    pub foreign_emitted: usize,
    /// The distinct truly-external types emitted as `Foreign`, keyed by source
    /// name, valued by reference count. These no longer count as `unresolved type
    /// reference`.
    pub foreign_types: BTreeMap<String, usize>,
    /// Unresolved refs kept as honest `Json` because they are **pi-internal**
    /// types that are simply not in the current `--context` set — keyed by ref
    /// name, valued by reference count. Each is resolvable by adding the defining
    /// package to `--context`; not misrepresented as external.
    pub context_expandable: BTreeMap<String, usize>,
    /// Unresolved refs naming an **in-scope item with no DTO form** — a `class`
    /// handle, a dropped non-union alias (keyed by ref name, valued by reference
    /// count). Kept as honest `Json`; adding a `--context` package cannot help, so
    /// these are reported apart from [`context_expandable`](Self::context_expandable).
    pub unmodeled_refs: BTreeMap<String, usize>,
    /// Unresolved refs left as `Json` because they are **generic type parameters**
    /// (`T`, a declared generic of the owning item) — no external handle applies.
    pub generic_params: BTreeMap<String, usize>,
    /// Items dropped with no fluessig home, keyed by a human reason.
    pub dropped: BTreeMap<String, usize>,
    /// Every type-string the parser could not model, keyed by cause. The sum is
    /// the count of `Json` fallbacks across all fields/params/returns.
    pub degradation_reasons: BTreeMap<String, usize>,
    /// Non-fatal ambiguities that still produced a real typed mapping
    /// (`number`→`float64`, a named string-literal union → enum, etc.).
    pub notes: BTreeMap<String, usize>,
}

impl Stats {
    fn bump(map: &mut BTreeMap<String, usize>, key: &str) {
        *map.entry(key.to_string()).or_insert(0) += 1;
    }
}

/// The full transform result: the two documents plus the coverage stats.
#[derive(Debug, Clone, Serialize)]
pub struct FluessigOutput {
    pub api: FlApiDoc,
    pub catalog: FlCatalog,
    pub stats: Stats,
}

// ─────────────────────────── the converter ──────────────────────────────────

/// Carries the resolution context (which bare identifiers name a known enum vs.
/// a known model) and accumulates synthesized unions + degradation reasons as
/// types are parsed.
struct Converter {
    known_enums: BTreeSet<String>,
    known_models: BTreeSet<String>,
    /// Names of top-level `typeAlias` targets lifted into named unions (a union
    /// of named types, or an `X[keyof X]` indexed-access expansion). A bare ref
    /// to one of these resolves to a `Union` rather than degrading to `Json`.
    known_unions: BTreeSet<String>,
    /// The value types of each known interface/record, keyed by model name —
    /// the raw field type-strings, in declaration order. Feeds `X[keyof X]`
    /// indexed-access expansion (the union over a map's value types).
    indexable: BTreeMap<String, Vec<String>>,
    unions: BTreeMap<String, FlUnion>,
    reasons: BTreeMap<String, usize>,
    notes: BTreeMap<String, usize>,
    /// Models synthesized from inline/anonymous object literals, keyed by name.
    minted: BTreeMap<String, FlModel>,
    /// Field-set signature → minted model name, so identical inline objects
    /// collapse to a single model instead of emitting N copies.
    minted_by_sig: BTreeMap<String, String>,
    /// The naming context (owning op/field path) used to name the next minted
    /// model — e.g. `["SpawnInstance", "Options"]` → `SpawnInstanceOptions`.
    name_hint: Vec<String>,
    /// The names that resolve to a **context** (sibling-package) type rather than
    /// a primary-package one. A resolved ref to any of these is recorded in
    /// [`needed`](Self::needed) so the type is pulled into the scoped emission.
    /// Empty in the single-report path, so that path is untouched.
    context_names: BTreeSet<String>,
    /// Context type names actually referenced (transitively) by the emitted
    /// surface — the worklist driving scoped cross-package emission.
    needed: BTreeSet<String>,
    /// Truly-external refs emitted as `Foreign` opaque handles (name → refcount).
    foreign_types: BTreeMap<String, usize>,
    /// Pi-internal refs kept as `Json` because they are not in the current
    /// `--context` set (name → refcount) — resolvable by adding their package.
    context_expandable: BTreeMap<String, usize>,
    /// Refs naming an in-scope item with no DTO form — a `class` handle, a dropped
    /// alias (name → refcount). Kept as `Json`; NOT a context gap.
    unmodeled_refs: BTreeMap<String, usize>,
    /// Names declared in some report but resolving to no model/enum/union (class,
    /// dropped alias, const) — the lookup behind [`unmodeled_refs`].
    known_nonmodel: BTreeSet<String>,
    /// Generic-type-parameter refs kept as `Json` (name → refcount).
    generic_params: BTreeMap<String, usize>,
    /// The declared generic parameters of the item currently being parsed (`T`,
    /// `TSchema`), so a bare ref to one is recognized as a generic rather than an
    /// unresolved external. Set around each item's fields/signature, then cleared.
    current_generics: BTreeSet<String>,
}

/// The outcome of parsing one rendered type string.
struct Parsed {
    ty: FlType,
    /// A `Json` fallback happened somewhere inside — the type is not faithfully
    /// modeled.
    degraded: bool,
}

impl Parsed {
    fn clean(ty: FlType) -> Self {
        Parsed {
            ty,
            degraded: false,
        }
    }
}

impl Converter {
    /// A fresh converter over a resolution context (the known enum/model names),
    /// with empty accumulators. The one place the [`Converter`] shape is built.
    fn new(known_enums: BTreeSet<String>, known_models: BTreeSet<String>) -> Self {
        Converter {
            known_enums,
            known_models,
            known_unions: BTreeSet::new(),
            indexable: BTreeMap::new(),
            unions: BTreeMap::new(),
            reasons: BTreeMap::new(),
            notes: BTreeMap::new(),
            minted: BTreeMap::new(),
            minted_by_sig: BTreeMap::new(),
            name_hint: Vec::new(),
            context_names: BTreeSet::new(),
            needed: BTreeSet::new(),
            foreign_types: BTreeMap::new(),
            context_expandable: BTreeMap::new(),
            unmodeled_refs: BTreeMap::new(),
            known_nonmodel: BTreeSet::new(),
            generic_params: BTreeMap::new(),
            current_generics: BTreeSet::new(),
        }
    }

    /// Record a resolved ref: if `name` names a context (sibling-package) type,
    /// add it to the worklist so scoped emission pulls it (and, transitively,
    /// whatever it references) into the output. A no-op for primary-package
    /// names, so the single-report path never populates the worklist.
    fn note_ref(&mut self, name: &str) {
        if self.context_names.contains(name) {
            self.needed.insert(name.to_string());
        }
    }

    fn degrade(&mut self, reason: &str) -> Parsed {
        Stats::bump(&mut self.reasons, reason);
        Parsed {
            ty: FlType::json(),
            degraded: true,
        }
    }

    /// Parse one rendered TS type string into an [`FlType`]. Never panics;
    /// anything unmodelable becomes `Json` (counted).
    fn parse_type(&mut self, raw: &str) -> Parsed {
        let s = normalize(raw);
        let s = s.trim();
        if s.is_empty() {
            return self.degrade("empty type string");
        }

        // Wrappers first (they bind looser than a top-level `|`).
        if let Some(inner) = strip_generic(s, "Promise") {
            // A `Promise<T>` in a field/return position: async-ness is handled at
            // the op level; here we just unwrap to the payload.
            return self.parse_type(&inner);
        }
        if let Some(inner) = strip_generic(s, "Array")
            .or_else(|| strip_generic(s, "ReadonlyArray"))
            .or_else(|| strip_generic(s, "ReadonlySet"))
            .or_else(|| strip_generic(s, "Set"))
        {
            let p = self.parse_type(&inner);
            return Parsed {
                ty: FlType::List {
                    list: Box::new(p.ty),
                },
                degraded: p.degraded,
            };
        }
        // Trailing `[]` array suffix (balanced).
        if let Some(inner) = strip_array_suffix(s) {
            let p = self.parse_type(inner);
            return Parsed {
                ty: FlType::List {
                    list: Box::new(p.ty),
                },
                degraded: p.degraded,
            };
        }

        // Top-level union.
        let members = split_top(s, '|');
        if members.len() > 1 {
            return self.parse_union(&members);
        }

        // A top-level `=>` is a function type — no fluessig home.
        if has_top_level_arrow(s) {
            return self.degrade("function type");
        }
        // An object literal `{ ... }`: mint a named model from its fields.
        if s.starts_with('{') {
            return self.parse_inline_object(s);
        }
        // Indexed / conditional / mapped types (`X[keyof X]`, `T extends ...`).
        if s.contains(" extends ") || s.contains("keyof ") || s.contains("infer ") {
            return self.degrade("conditional/mapped type");
        }

        self.parse_atom(s)
    }

    /// Parse a single (non-union, non-array, non-wrapper) atom.
    fn parse_atom(&mut self, s: &str) -> Parsed {
        // String / numeric / boolean literals collapse to their scalar.
        if is_string_literal(s) {
            return Parsed::clean(FlType::Scalar("string".to_string()));
        }
        if s == "true" || s == "false" {
            return Parsed::clean(FlType::Scalar("boolean".to_string()));
        }
        if is_numeric_literal(s) {
            Stats::bump(&mut self.notes, "numeric literal → float64");
            return Parsed::clean(FlType::Scalar("float64".to_string()));
        }

        match s {
            "string" => Parsed::clean(FlType::Scalar("string".to_string())),
            "boolean" => Parsed::clean(FlType::Scalar("boolean".to_string())),
            "number" => {
                Stats::bump(&mut self.notes, "number → float64 (int/float ambiguity)");
                Parsed::clean(FlType::Scalar("float64".to_string()))
            }
            "bigint" => Parsed::clean(FlType::Scalar("int64".to_string())),
            "void" | "undefined" | "null" | "never" => {
                Parsed::clean(FlType::Scalar("void".to_string()))
            }
            "any" | "unknown" | "object" => Parsed::clean(FlType::json()),
            "Uint8Array" | "Buffer" | "ArrayBuffer" | "Uint8ArrayConstructor" => {
                Parsed::clean(FlType::Scalar("bytes".to_string()))
            }
            "Date" => {
                Stats::bump(&mut self.notes, "Date → string");
                Parsed::clean(FlType::Scalar("string".to_string()))
            }
            _ => self.parse_named(s),
        }
    }

    /// A bare (possibly generic) identifier: a known enum/model ref, an
    /// unwrappable generic wrapper, or a degradation.
    fn parse_named(&mut self, s: &str) -> Parsed {
        // Generic types: unwrap the transparent ones, degrade the rest.
        if let Some((head, inner)) = split_generic_head(s) {
            match head {
                "Readonly" | "Partial" | "Required" | "NonNullable" | "Awaited" => {
                    return self.parse_type(&inner);
                }
                "Record" | "Map" => return self.degrade("Record/Map type"),
                _ => return self.degrade("unmodeled generic type"),
            }
        }
        // A dotted, module-qualified name (`net.Server`, `NodeJS.ProcessEnv`) is
        // never a fluessig model — it is a truly-external host type. Emit a
        // `Foreign` opaque handle rather than an `unparsable type expression`.
        if let Some(foreign) = dotted_foreign(s) {
            return self.emit_foreign(s, foreign);
        }
        if !is_ident(s) {
            return self.degrade("unparsable type expression");
        }
        if self.known_enums.contains(s) {
            self.note_ref(s);
            return Parsed::clean(FlType::Enum {
                r#enum: s.to_string(),
            });
        }
        if self.known_models.contains(s) {
            self.note_ref(s);
            return Parsed::clean(FlType::Model {
                model: s.to_string(),
            });
        }
        if self.known_unions.contains(s) {
            self.note_ref(s);
            return Parsed::clean(FlType::Union {
                union: s.to_string(),
            });
        }
        // An unresolved bare name. The opaque-handle policy sorts it into three
        // honest buckets (see [`Converter::resolve_unresolved`]): a generic type
        // parameter, a truly-external type (→ `Foreign`), or a pi-internal type
        // merely absent from the current `--context` set (→ honest `Json`).
        self.resolve_unresolved(s)
    }

    /// Classify an unresolved bare PascalCase-ish name under the opaque-handle
    /// policy. Order matters: generics first (never opaqued), then the
    /// external/builtin allowlist (→ `Foreign`), else it is presumed pi-internal
    /// and kept as honest `Json` — the conservative default, so a pi type is never
    /// misrepresented as external.
    fn resolve_unresolved(&mut self, s: &str) -> Parsed {
        if self.current_generics.contains(s) || is_generic_param(s) {
            // A generic type parameter has no external handle — keep the current
            // `Json` fallback, recorded so the residual is legible.
            Stats::bump(&mut self.generic_params, s);
            return self.degrade("unresolved type reference");
        }
        if let Some(foreign) = builtin_foreign(s) {
            return self.emit_foreign(s, foreign);
        }
        if self.known_nonmodel.contains(s) {
            // Declared in scope but with no DTO form (a `class` handle, a dropped
            // alias) — honest `Json`, but NOT a context gap; adding a package would
            // not help. Recorded separately so the residual is not misreported.
            Stats::bump(&mut self.unmodeled_refs, s);
            return self.degrade("unresolved type reference");
        }
        // Presumed pi-internal, just not in the current `--context` set. Honest
        // `Json` (not opaqued), recorded so the summary can point at the package
        // to add — never misrepresented as external.
        Stats::bump(&mut self.context_expandable, s);
        self.degrade("unresolved type reference")
    }

    /// Emit a `Foreign` opaque handle for a truly-external ref, counting it under
    /// `foreign_types` (and, at fold time, `foreign_emitted`). Not a degradation:
    /// the type is faithfully carried as a typed handle rather than a `Json` blob.
    fn emit_foreign(&mut self, seen_as: &str, foreign: FlForeign) -> Parsed {
        Stats::bump(&mut self.foreign_types, seen_as);
        Parsed::clean(FlType::Foreign { foreign })
    }

    /// A multi-member top-level union. Null/undefined members make it nullable;
    /// an all-string-literal union collapses to `string`; otherwise a named
    /// union is synthesized.
    fn parse_union(&mut self, members: &[String]) -> Parsed {
        let mut nullable = false;
        let mut rest: Vec<String> = Vec::new();
        for m in members {
            let t = m.trim();
            if t == "null" || t == "undefined" {
                nullable = true;
            } else {
                rest.push(t.to_string());
            }
        }
        if rest.is_empty() {
            return Parsed::clean(FlType::Scalar("void".to_string()));
        }
        let inner = if rest.len() == 1 {
            self.parse_type(&rest[0])
        } else if rest.iter().all(|m| is_string_literal(m)) {
            // An inline (anonymous) string-literal union: only *named* ones become
            // enums, so this collapses to `string`.
            Stats::bump(&mut self.notes, "inline string-literal union → string");
            Parsed::clean(FlType::Scalar("string".to_string()))
        } else if rest.iter().all(|m| m == "true" || m == "false") {
            Parsed::clean(FlType::Scalar("boolean".to_string()))
        } else {
            self.synthesize_union(&rest)
        };
        if nullable {
            Parsed {
                ty: FlType::Nullable {
                    nullable: Box::new(inner.ty),
                },
                degraded: inner.degraded,
            }
        } else {
            inner
        }
    }

    /// Build (and register, deduplicated by name) a named union from its
    /// non-null members. The union rides as a `String` envelope in the Rust core
    /// surface, so member resolution is metadata only — a member that is neither
    /// a known model nor a known enum keeps its PascalCase name as a `model` ref
    /// rather than degrading.
    fn synthesize_union(&mut self, members: &[String]) -> Parsed {
        let mut variants: Vec<FlUnionVariant> = Vec::new();
        let mut name_parts: Vec<String> = Vec::new();
        let mut seen_tags: BTreeSet<String> = BTreeSet::new();
        let mut degraded = false;
        for m in members {
            let (ty, label) = self.union_member(m, &mut degraded);
            name_parts.push(pascal(&label));
            push_unique_variant(&mut variants, &mut seen_tags, &label, ty);
        }
        let name = format!("{}Union", name_parts.join("Or"));
        self.unions.entry(name.clone()).or_insert(FlUnion {
            name: name.clone(),
            variants,
        });
        Parsed {
            ty: FlType::Union { union: name },
            degraded,
        }
    }

    /// One union member → (its fluessig type, a label used to name the union &
    /// tag the variant).
    fn union_member(&mut self, m: &str, degraded: &mut bool) -> (FlType, String) {
        let t = m.trim();
        if is_ident(t) {
            if self.known_enums.contains(t) {
                self.note_ref(t);
                return (
                    FlType::Enum {
                        r#enum: t.to_string(),
                    },
                    t.to_string(),
                );
            }
            // Keep the name as metadata even when the model is not (yet) defined.
            self.note_ref(t);
            return (
                FlType::Model {
                    model: t.to_string(),
                },
                t.to_string(),
            );
        }
        // A structural member (literal, object, function). Fall back to Json and
        // label it generically.
        let p = self.parse_type(t);
        if p.degraded {
            *degraded = true;
        }
        let label = structural_label(&p.ty);
        (p.ty, label)
    }

    /// Lift a top-level union / indexed-access `typeAlias` into a **named** union
    /// registered under the alias's own name (so `field: OrchestratorRequest`
    /// resolves to `Union { union: "OrchestratorRequest" }`). `members` is the
    /// flattened member list from [`expand_alias_union_members`] — each recurses
    /// through [`parse_type`], so an in-package model/enum/lifted-union member
    /// resolves and a sibling-package member degrades to `Json` (counted),
    /// while its source name is kept as the variant tag. Returns whether a new
    /// union was registered (a duplicate name is a no-op).
    fn lift_alias_union(&mut self, name: &str, members: &[String]) -> bool {
        if self.unions.contains_key(name) {
            return false;
        }
        let mut variants: Vec<FlUnionVariant> = Vec::new();
        let mut seen_tags: BTreeSet<String> = BTreeSet::new();
        for m in members {
            let t = m.trim();
            let parsed = self.parse_type(t);
            // Keep the member's source ident as the tag label even when the type
            // degrades to `Json`, so tags stay readable (`rpcCommand`) rather than
            // collapsing to `json`.
            let label = if is_ident(t) {
                t.to_string()
            } else {
                structural_label(&parsed.ty)
            };
            push_unique_variant(&mut variants, &mut seen_tags, &label, parsed.ty);
        }
        self.unions.insert(
            name.to_string(),
            FlUnion {
                name: name.to_string(),
                variants,
            },
        );
        true
    }

    /// An inline/anonymous object literal `{ a: T; b?: U }` → a **minted**,
    /// named model (rather than a `Json` blob). Members that are call/method or
    /// index signatures — or an empty/unparsable body — still degrade honestly,
    /// since those are not plain data records (that's the callback lane). Each
    /// field type recurses through [`parse_type`], so a nested inline object
    /// mints a nested model and a known name resolves; identical field-sets
    /// dedupe to one model.
    fn parse_inline_object(&mut self, s: &str) -> Parsed {
        let inner = match s.strip_prefix('{').and_then(|t| t.strip_suffix('}')) {
            Some(i) => i.trim(),
            None => return self.degrade("inline object literal"),
        };
        if inner.is_empty() {
            return self.degrade("empty inline object literal");
        }
        let mut fields: Vec<FlField> = Vec::new();
        for member in split_object_members(inner) {
            if member.is_empty() {
                continue;
            }
            let Some((raw_name, raw_ty)) = split_object_member(&member) else {
                // A call/method/index signature — not a plain data record.
                return self.degrade("inline object with call/index signature");
            };
            let (field_name, optional) = match raw_name.strip_suffix('?') {
                Some(n) => (n.trim().to_string(), true),
                None => (raw_name, false),
            };
            if !is_ident(&field_name) {
                return self.degrade("inline object with call/index signature");
            }
            // Recurse, extending the naming path so a nested inline object mints
            // a nested, readably-named model.
            self.name_hint.push(pascal(&field_name));
            let parsed = self.parse_type(&raw_ty);
            self.name_hint.pop();
            let (ty, was_nullable) = unwrap_nullable(parsed.ty);
            fields.push(FlField {
                name: field_name,
                ty,
                nullable: optional || was_nullable,
            });
        }
        if fields.is_empty() {
            return self.degrade("empty inline object literal");
        }
        Parsed::clean(self.mint_object_model(fields))
    }

    /// Register (deduplicated by field-set) a minted model for `fields` and
    /// return a `Model` ref to it. Deterministic: the name comes from the active
    /// [`name_hint`] path, disambiguated against every known/minted name.
    fn mint_object_model(&mut self, fields: Vec<FlField>) -> FlType {
        let sig = object_signature(&fields);
        if let Some(existing) = self.minted_by_sig.get(&sig) {
            return FlType::Model {
                model: existing.clone(),
            };
        }
        let name = self.unique_minted_name();
        self.known_models.insert(name.clone());
        self.minted_by_sig.insert(sig, name.clone());
        self.minted.insert(
            name.clone(),
            FlModel {
                name: name.clone(),
                doc: None,
                fields,
            },
        );
        Stats::bump(&mut self.notes, "inline object literal → minted model");
        FlType::Model { model: name }
    }

    /// A readable, collision-free name for the model about to be minted: the
    /// joined naming path (falling back to `InlineObject` at the top level),
    /// suffixed with a counter only if that name is already taken.
    fn unique_minted_name(&self) -> String {
        let base = self.name_hint.concat();
        let base = if base.is_empty() {
            "InlineObject".to_string()
        } else {
            base
        };
        let taken = |n: &str| self.known_models.contains(n) || self.known_enums.contains(n);
        if !taken(&base) {
            return base;
        }
        let mut i = 2;
        loop {
            let cand = format!("{base}{i}");
            if !taken(&cand) {
                return cand;
            }
            i += 1;
        }
    }
}

// ─────────────────────────── orchestration ──────────────────────────────────

/// Convert a hinzu [`ApiReport`] into fluessig's `api.json` + `catalog.json`
/// (plus coverage [`Stats`]). Pure: transforms only in-memory data.
///
/// `context` is a slice of **sibling-package** reports the primary imports types
/// from. Their models/enums/liftable-union aliases populate the resolution
/// namespace so the primary's refs to sibling types (`RpcCommand`,
/// `RpcResponse`, …) resolve instead of degrading to `Json`. Emission stays
/// **scoped**: only the sibling types the primary surface *transitively
/// references* are pulled into the output as real `models[]`/`unions[]`/`enums`;
/// the context packages' own op surface is never emitted. Pass `&[]` for the
/// single-report behavior (byte-identical to the pre-context path).
pub fn build_fluessig(report: &ApiReport, context: &[ApiReport]) -> FluessigOutput {
    let items: Vec<&ApiItem> = report.modules.iter().flat_map(|m| m.items.iter()).collect();

    let mut stats = Stats {
        items_in: items.len(),
        context_reports: context.len(),
        ..Default::default()
    };
    for it in &items {
        Stats::bump(&mut stats.by_kind, &it.kind);
    }

    // Pass 1 — resolution context. Interfaces/records are models; a
    // string-literal-union type alias (or a real enum) is an enum.
    let mut catalog_enums: Vec<FlEnum> = Vec::new();
    let mut known_enums = BTreeSet::new();
    let mut known_models = BTreeSet::new();
    // The value types of each interface/record, for `X[keyof X]` expansion.
    let mut indexable: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // Every primary-package item name — a context type of the same name is
    // skipped (the primary's own definition is authoritative and already
    // emitted), so cross-package resolution never shadows or duplicates it.
    let primary_names: BTreeSet<String> = items.iter().map(|it| it.name.clone()).collect();
    for it in &items {
        match it.kind.as_str() {
            "interface" | "record" => {
                known_models.insert(it.name.clone());
                indexable.insert(
                    it.name.clone(),
                    it.fields.iter().map(|f| f.ty.clone()).collect(),
                );
            }
            "enum" => {
                known_enums.insert(it.name.clone());
                catalog_enums.push(enum_from_item(it));
            }
            "typeAlias" => {
                if let Some(variants) = string_literal_union(it.alias_target.as_deref()) {
                    known_enums.insert(it.name.clone());
                    catalog_enums.push(FlEnum {
                        name: it.name.clone(),
                        variants,
                    });
                }
            }
            _ => {}
        }
    }

    // Pass 1c — sibling-package (context) namespace. Registers context type NAMES
    // into the resolution namespace so the primary's refs resolve, and records how
    // to emit each on demand (`context_models`/`context_enum_defs`/
    // `context_unions`). Nothing is emitted here — the scoped closure below pulls
    // in only the transitively-referenced ones. A name the primary already owns is
    // skipped. `indexable` is merged (primary wins) so cross-package `X[keyof X]`
    // still expands.
    let ctx_items: Vec<&ApiItem> = context
        .iter()
        .flat_map(|r| r.modules.iter().flat_map(|m| m.items.iter()))
        .collect();
    let mut context_names: BTreeSet<String> = BTreeSet::new();
    let mut context_models: BTreeMap<String, &ApiItem> = BTreeMap::new();
    let mut context_enum_defs: BTreeMap<String, FlEnum> = BTreeMap::new();
    // First register models/enums + `indexable`, so alias liftability (which reads
    // `indexable`) sees every context map type before it is tested.
    for it in &ctx_items {
        if primary_names.contains(&it.name) {
            continue;
        }
        match it.kind.as_str() {
            "interface" | "record" => {
                known_models.insert(it.name.clone());
                indexable
                    .entry(it.name.clone())
                    .or_insert_with(|| it.fields.iter().map(|f| f.ty.clone()).collect());
                context_names.insert(it.name.clone());
                context_models.entry(it.name.clone()).or_insert(it);
            }
            "enum" => {
                known_enums.insert(it.name.clone());
                context_names.insert(it.name.clone());
                context_enum_defs
                    .entry(it.name.clone())
                    .or_insert_with(|| enum_from_item(it));
            }
            "typeAlias" => {
                if let Some(variants) = string_literal_union(it.alias_target.as_deref()) {
                    known_enums.insert(it.name.clone());
                    context_names.insert(it.name.clone());
                    context_enum_defs.entry(it.name.clone()).or_insert(FlEnum {
                        name: it.name.clone(),
                        variants,
                    });
                }
            }
            _ => {}
        }
    }

    // Resolution sub-pass: recognize which top-level union / indexed-access
    // aliases will lift, so refs to them (from any item, in any order) resolve to
    // a `Union` in pass 2. String-literal-union aliases are already enums above.
    let mut known_unions = BTreeSet::new();
    for it in &items {
        if it.kind == "typeAlias"
            && !known_enums.contains(&it.name)
            && expand_alias_union_members(it.alias_target.as_deref(), &indexable).is_some()
        {
            known_unions.insert(it.name.clone());
        }
    }
    // The same for context liftable-union aliases (deferred emission).
    let mut context_unions: BTreeMap<String, &ApiItem> = BTreeMap::new();
    for it in &ctx_items {
        if it.kind == "typeAlias"
            && !primary_names.contains(&it.name)
            && !known_enums.contains(&it.name)
            && expand_alias_union_members(it.alias_target.as_deref(), &indexable).is_some()
        {
            known_unions.insert(it.name.clone());
            context_names.insert(it.name.clone());
            context_unions.entry(it.name.clone()).or_insert(it);
        }
    }

    // Names that ARE declared (in the primary or a context report) but resolve to
    // no DTO/enum/union — a `class` (its op surface, not a data model), a dropped
    // non-union alias, a const. A ref to one is honestly unresolved but is NOT a
    // context gap (adding a package cannot help — it is in scope, just not a
    // model), so it is reported separately from the pi-internal-not-in-context set.
    let mut known_nonmodel: BTreeSet<String> = items
        .iter()
        .chain(ctx_items.iter())
        .map(|it| it.name.clone())
        .collect();
    known_nonmodel.retain(|n| {
        !known_models.contains(n) && !known_enums.contains(n) && !known_unions.contains(n)
    });

    let mut conv = Converter::new(known_enums, known_models);
    conv.known_unions = known_unions;
    conv.indexable = indexable;
    conv.context_names = context_names;
    conv.known_nonmodel = known_nonmodel;

    // Pass 2 — models, interfaces, and free-function ops.
    let mut models: Vec<FlModel> = Vec::new();
    let mut interfaces: Vec<FlInterface> = Vec::new();
    let mut consts: Vec<FlConst> = Vec::new();
    let mut free_ops: Vec<FlOp> = Vec::new();

    for it in &items {
        match it.kind.as_str() {
            "interface" | "record" => {
                models.push(build_model(&mut conv, &mut stats, it));
            }
            "class" => {
                interfaces.push(build_class_interface(&mut conv, &mut stats, &items, it));
            }
            "function" => {
                if let Some(op) = build_op(&mut conv, &mut stats, it) {
                    free_ops.push(op);
                }
            }
            "const" => {
                // Emit a representable const into `consts[]` (was dropped). An
                // intrinsically-untyped one (`any`) is skipped inside `build_const`.
                if let Some(c) = build_const(&mut conv, &mut stats, it) {
                    consts.push(c);
                }
            }
            "method" => { /* handled with its owning class */ }
            "enum" | "typeAlias" | "namespace" | "trait" | "struct" => {
                // enum/typeAlias were consumed in pass 1 (or dropped as non-union);
                // the rest have no op/model home in this spike.
                if it.kind == "typeAlias" && conv.known_unions.contains(&it.name) {
                    // A top-level union / indexed-access alias: lift it into a
                    // named union rather than dropping it.
                    if let Some(members) =
                        expand_alias_union_members(it.alias_target.as_deref(), &conv.indexable)
                    {
                        if conv.lift_alias_union(&it.name, &members) {
                            stats.unions_lifted += 1;
                        }
                    }
                    continue;
                }
                let reason = match it.kind.as_str() {
                    "typeAlias" if conv.known_enums.contains(&it.name) => continue,
                    "enum" => continue,
                    "typeAlias" => "typeAlias (non-string-union) dropped",
                    "namespace" => "namespace dropped",
                    "trait" => "trait dropped",
                    _ => "unsupported item dropped",
                };
                Stats::bump(&mut stats.dropped, reason);
            }
            other => {
                Stats::bump(
                    &mut stats.dropped,
                    &format!("unknown kind `{other}` dropped"),
                );
            }
        }
    }

    if !free_ops.is_empty() {
        free_ops.sort_by(|a, b| a.name.cmp(&b.name));
        interfaces.push(FlInterface {
            name: package_interface_name(&report.package.name),
            doc: Some(format!(
                "Free functions of `{}`, grouped as one interface.",
                report.package.name
            )),
            ops: free_ops,
        });
    }

    // Scoped cross-package emission — a transitive closure over the sibling types
    // the primary surface referenced. Each round emits the newly-`needed` context
    // types as real fluessig types; emitting a context model/union parses its
    // fields/members, which may reference further context types (`RpcCommand` →
    // its inline objects; `AgentSessionEvent` → `AgentMessage`), growing the
    // worklist. Deterministic: `needed` is a `BTreeSet`, so each batch is
    // name-sorted, and only referenced types are pulled (never a context package's
    // whole op surface). Truly-foreign refs (`ChildProcess`, `http.Server`) were
    // never registered as context types, so they still degrade to `Json` here.
    let mut emitted_ctx: BTreeSet<String> = BTreeSet::new();
    loop {
        let batch: Vec<String> = conv
            .needed
            .iter()
            .filter(|n| !emitted_ctx.contains(n.as_str()))
            .cloned()
            .collect();
        if batch.is_empty() {
            break;
        }
        for name in batch {
            emitted_ctx.insert(name.clone());
            if let Some(it) = context_models.get(name.as_str()) {
                models.push(build_model(&mut conv, &mut stats, it));
            } else if let Some(en) = context_enum_defs.get(name.as_str()) {
                catalog_enums.push(en.clone());
            } else if let Some(it) = context_unions.get(name.as_str()) {
                if let Some(members) =
                    expand_alias_union_members(it.alias_target.as_deref(), &conv.indexable)
                {
                    if conv.lift_alias_union(&name, &members) {
                        stats.unions_lifted += 1;
                    }
                }
            }
        }
    }
    stats.context_types_pulled = emitted_ctx.len();

    // Fold in models minted from inline object literals (deduped by field-set).
    stats.models_minted = conv.minted.len();
    models.extend(std::mem::take(&mut conv.minted).into_values());

    models.sort_by(|a, b| a.name.cmp(&b.name));
    interfaces.sort_by(|a, b| a.name.cmp(&b.name));
    consts.sort_by(|a, b| a.name.cmp(&b.name));
    catalog_enums.sort_by(|a, b| a.name.cmp(&b.name));
    let mut unions: Vec<FlUnion> = conv.unions.into_values().collect();
    unions.sort_by(|a, b| a.name.cmp(&b.name));

    stats.models_emitted = models.len();
    stats.interfaces_emitted = interfaces.len();
    stats.enums_emitted = catalog_enums.len();
    stats.unions_synthesized = unions.len();
    stats.degradation_reasons = conv.reasons;
    stats.notes = conv.notes;
    stats.foreign_emitted = conv.foreign_types.values().sum();
    stats.foreign_types = conv.foreign_types;
    stats.context_expandable = conv.context_expandable;
    stats.unmodeled_refs = conv.unmodeled_refs;
    stats.generic_params = conv.generic_params;

    let source = Some(format!("{} (via hinzu api-fluessig)", report.package.name));
    let api = FlApiDoc {
        fluessig: FlVersions::default(),
        source: source.clone(),
        models,
        unions,
        interfaces,
        consts,
    };
    let catalog = FlCatalog {
        fluessig: FlVersions::default(),
        source,
        scalars: Vec::new(),
        unions: Vec::new(),
        enums: catalog_enums,
        entities: Vec::new(),
        relation_properties: Vec::new(),
        value_structs: Vec::new(),
    };
    FluessigOutput {
        api,
        catalog,
        stats,
    }
}

/// Build a catalog [`FlEnum`] from a real `enum` item's variants (name + wire
/// discriminant). Shared by the primary and context namespace passes.
fn enum_from_item(it: &ApiItem) -> FlEnum {
    FlEnum {
        name: it.name.clone(),
        variants: it
            .variants
            .iter()
            .map(|v| FlEnumVariant {
                name: v.name.clone(),
                value: v.discriminant.clone(),
            })
            .collect(),
    }
}

/// Build a DTO model from an `interface`/`record` item, tallying degraded fields.
fn build_model(conv: &mut Converter, stats: &mut Stats, it: &ApiItem) -> FlModel {
    conv.current_generics = generic_names(&it.generics);
    let mut fields = Vec::new();
    for f in &it.fields {
        // A method-shaped field on an interface (a rendered function type) has no
        // DTO meaning — skip it rather than emitting a `Json` data field.
        if has_top_level_arrow(&normalize(&f.ty)) {
            Stats::bump(&mut stats.notes, "interface method-field skipped");
            continue;
        }
        stats.fields_total += 1;
        conv.name_hint
            .push(format!("{}{}", pascal(&it.name), pascal(&f.name)));
        let parsed = conv.parse_type(&f.ty);
        conv.name_hint.pop();
        let (ty, was_nullable) = unwrap_nullable(parsed.ty);
        if parsed.degraded {
            stats.fields_degraded += 1;
        }
        fields.push(FlField {
            name: f.name.clone(),
            ty,
            nullable: f.optional || was_nullable,
        });
    }
    conv.current_generics.clear();
    FlModel {
        name: it.name.clone(),
        doc: it.doc.clone(),
        fields,
    }
}

/// Build an op-bearing interface from a `class` item and its `method` items
/// (matched by receiver == class name).
fn build_class_interface(
    conv: &mut Converter,
    stats: &mut Stats,
    items: &[&ApiItem],
    class: &ApiItem,
) -> FlInterface {
    let mut ops = Vec::new();
    for it in items {
        if it.kind != "method" {
            continue;
        }
        let is_ours = it
            .signature
            .as_ref()
            .and_then(|s| s.receiver.as_deref())
            .map(|r| r == class.name)
            .unwrap_or(false)
            || it.id.starts_with(&format!("{}.", class.id));
        if is_ours {
            if let Some(op) = build_op(conv, stats, it) {
                ops.push(op);
            }
        }
    }
    ops.sort_by(|a, b| a.name.cmp(&b.name));
    FlInterface {
        name: class.name.clone(),
        doc: class.doc.clone(),
        ops,
    }
}

/// Build one op from a `function`/`method` item. Returns `None` only when the
/// item has no signature.
fn build_op(conv: &mut Converter, stats: &mut Stats, it: &ApiItem) -> Option<FlOp> {
    let sig = it.signature.as_ref()?;
    stats.ops_total += 1;
    // The op's own generics plus the owning item's — either can spell a bare
    // type-param ref (`T`, `TParams`) in a param/return position.
    conv.current_generics = generic_names(&it.generics);
    conv.current_generics.extend(generic_names(&sig.generics));
    let mut degraded = false;

    // Return type: unwrap Promise (→ async) and Async{Iterable,Generator} (→ stream).
    let mut is_async = sig.is_async;
    let mut shape = "unary";
    conv.name_hint.push(format!("{}Result", pascal(&it.name)));
    let (returns, ret_degraded) = match sig.return_type.as_deref() {
        None => (FlType::Scalar("void".to_string()), false),
        Some(rt) => {
            let n = normalize(rt);
            let n = n.trim().to_string();
            if let Some(inner) = strip_generic(&n, "Promise") {
                is_async = true;
                let p = conv.parse_type(&inner);
                (p.ty, p.degraded)
            } else if let Some(inner) = strip_generic(&n, "AsyncIterable")
                .or_else(|| strip_generic(&n, "AsyncGenerator"))
                .or_else(|| strip_generic(&n, "AsyncIterableIterator"))
            {
                shape = "stream";
                let p = conv.parse_type(&inner);
                (p.ty, p.degraded)
            } else {
                let p = conv.parse_type(&n);
                (p.ty, p.degraded)
            }
        }
    };
    conv.name_hint.pop();
    if ret_degraded {
        stats.returns_degraded += 1;
        degraded = true;
    }

    let mut params = Vec::new();
    for p in &sig.params {
        stats.params_total += 1;
        let role = if p.name.is_empty() {
            "Arg".to_string()
        } else {
            pascal(&p.name)
        };
        conv.name_hint.push(format!("{}{}", pascal(&it.name), role));
        let parsed = conv.parse_type(&p.ty);
        conv.name_hint.pop();
        if parsed.degraded {
            stats.params_degraded += 1;
            degraded = true;
        }
        // The param's own `optional` flag already conveys nullability; unwrap a
        // `| undefined` the parser turned into `Nullable` so we don't double it.
        let (ty, was_nullable) = unwrap_nullable(parsed.ty);
        let optional = if p.optional || was_nullable {
            Some(true)
        } else {
            None
        };
        params.push(FlParam {
            name: sanitize_param(&p.name),
            ty,
            optional,
        });
    }

    conv.current_generics.clear();
    if degraded {
        stats.ops_degraded += 1;
    } else {
        stats.ops_clean += 1;
    }

    Some(FlOp {
        name: it.name.clone(),
        doc: it.doc.clone(),
        shape: shape.to_string(),
        is_async,
        params,
        returns,
    })
}

/// Build an [`FlConst`] from a `const` item, or `None` when the const has no
/// representable type. The declared `type` is lowered through the shared
/// [`Converter::parse_type`], so a scalar becomes a bare scalar (`number` →
/// `float64`, `boolean` → `boolean`, …) and a model/union/foreign ref resolves
/// exactly as it does for a field/param — no parallel scalar-mapping path.
///
/// An intrinsically-untyped const (`any`/`unknown`/`object`, or no declared type)
/// has NO fluessig type form: emitting it would be a zero-information
/// `{"type":"Json"}`. It is honestly skipped and counted under
/// `dropped["const dropped (untyped)"]` rather than dropped silently or forged
/// into a bogus typed const. A named-but-unmodeled ref (a `class` handle like
/// `OrchestratorSupervisor`) is NOT untyped — it lowers to whatever `parse_type`
/// yields (here honest `Json`) and is still emitted, so the const's existence and
/// its referenced type stay visible in the surface.
///
/// `const_value` is RAW EXPRESSION TEXT, not a literal, so a `value` is attached
/// only when it is a SIMPLE literal AND the type is a const-representable scalar
/// (see [`const_value_for`]); any runtime expression leaves `value` absent.
fn build_const(conv: &mut Converter, stats: &mut Stats, it: &ApiItem) -> Option<FlConst> {
    let raw = it.const_type.as_deref().map(normalize).unwrap_or_default();
    let raw = raw.trim();
    if raw.is_empty() || is_untyped_ts_type(raw) {
        Stats::bump(&mut stats.dropped, "const dropped (untyped)");
        return None;
    }
    let parsed = conv.parse_type(raw);
    let value = const_value_for(&parsed.ty, it.const_value.as_deref());
    stats.consts_emitted += 1;
    Some(FlConst {
        name: it.name.clone(),
        doc: it.doc.clone(),
        ty: parsed.ty,
        value,
    })
}

/// Extract a [`FlConstValue`] from a const's raw `const_value` text — but ONLY
/// when the value is a SIMPLE literal and the declared `ty` is a
/// const-representable scalar the literal matches. Anything else (a runtime
/// expression like `pkg.version || "0.0.0"`, a non-scalar type, a form/type
/// mismatch) → `None`, so a runtime-valued const round-trips as a typed const
/// with no value rather than a fabricated one. The value form follows the literal
/// (untagged), matching fluessig's `ConstValue`.
fn const_value_for(ty: &FlType, raw: Option<&str>) -> Option<FlConstValue> {
    let raw = raw?.trim();
    let FlType::Scalar(scalar) = ty else {
        return None;
    };
    match scalar.as_str() {
        "string" => simple_string_literal(raw).map(FlConstValue::Str),
        "boolean" => match raw {
            "true" => Some(FlConstValue::Bool(true)),
            "false" => Some(FlConstValue::Bool(false)),
            _ => None,
        },
        "int32" | "int64" => raw.parse::<i64>().ok().map(FlConstValue::Int),
        "float64" => {
            // A bare integer literal for a float const rides as `Int` (fluessig's
            // untagged carrier and rust-core both accept it); a fractional literal
            // rides as `Float`.
            if let Ok(i) = raw.parse::<i64>() {
                Some(FlConstValue::Int(i))
            } else {
                raw.parse::<f64>()
                    .ok()
                    .filter(|f| f.is_finite())
                    .map(FlConstValue::Float)
            }
        }
        _ => None,
    }
}

mod helpers;
use helpers::*;

#[cfg(test)]
mod tests;
