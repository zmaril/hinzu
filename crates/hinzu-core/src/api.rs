//! The public-API surface report: a language-agnostic, serializable description
//! of a package's **public interface** — exported functions/methods with real
//! signatures, exported types/enums/traits/aliases/consts with their shapes,
//! visibility, module path, and doc comments — grouped by module.
//!
//! Where [`crate::graph`] answers "in what order should a port move code", this
//! module answers "what is the contract the port must match": the declared
//! public surface, with signatures. Two consumers drive the shape: porting (the
//! source package's public API as the contract a port must satisfy) and
//! binding/agent tooling (deciding what ops and DTOs a generated binding should
//! expose).
//!
//! ## The pure boundary
//!
//! Everything here is a **pure** transform over already-extracted in-memory
//! data: [`build_api`] takes a package descriptor, a [`Fidelity`] block, and the
//! per-module items a language extractor produced, and returns a normalized,
//! deterministically-sorted [`ApiReport`]. It reads no files and spawns no
//! processes — the language-specific extraction (running `rustdoc`, a type
//! checker, or an LSP) lives in the CLI, which hands the parsed result here. So
//! this module stays inside hinzu-core's functional-core region.
//!
//! ## Determinism
//!
//! Diffs and CI gates need a stable byte layout, so [`build_api`] sorts
//! [`modules`](ApiReport::modules) by path and the top-level
//! [`items`](Module::items) within each module by `(kind, name)`, while
//! **preserving source order** of a struct's `fields`, an enum's `variants`, and
//! a signature's `params` — where position carries meaning. No timestamps and no
//! absolute paths leak in; the extractor is responsible for handing over
//! relative file paths.
//!
//! ## Fidelity, stated honestly
//!
//! Types are **rendered strings** (`Vec<String>`, `Option<Bar>`) — honest and
//! portable for v1; structured, cross-referenced type refs are a documented
//! follow-up. Whatever a given language extractor cannot model is recorded in
//! [`Fidelity::notes`] rather than faked (for Rust, for instance, `throws` is
//! not modeled; a `Result` return's error type is captured in
//! [`Signature::error_type`] instead).

use serde::{Deserialize, Serialize};

/// The schema version embedded in every emitted API report, so a consumer can
/// branch on shape changes. Bumped only on a breaking change to the shape.
pub const HINZU_API_VERSION: u32 = 1;

/// The complete API report, ready to serialize as JSON.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiReport {
    /// The schema version ([`HINZU_API_VERSION`]).
    pub hinzu_api_version: u32,
    /// The analyzed package.
    pub package: PackageInfo,
    /// How the surface was extracted and what it does/doesn't capture.
    pub fidelity: Fidelity,
    /// The public modules, sorted by path.
    pub modules: Vec<Module>,
}

/// The analyzed package: its name, source language, a label for the analyzed
/// root (usually the project path), and a version when one is known.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackageInfo {
    /// The package/crate name.
    pub name: String,
    /// The source language: `"rust"`, `"typescript"`, `"python"`, or `"go"`.
    pub language: String,
    /// A free-form label for the analyzed target (usually the project path).
    pub root: String,
    /// The package version, when the extractor could determine it.
    pub version: Option<String>,
}

/// The fidelity of an API report: which extractor produced it, the extractor's
/// own format version when relevant, whether the surface is believed complete,
/// and honest human-readable notes about what is and isn't modeled.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Fidelity {
    /// The extraction source: `"rustdoc-json"`, `"tsc"`, `"lsp-ty"`,
    /// `"lsp-gopls"`.
    pub source: String,
    /// The extractor's own format version, when it exposes one (e.g. rustdoc
    /// JSON's `format_version`), recorded so a consumer can reason about drift.
    pub format_version: Option<String>,
    /// Whether the report is believed to capture the whole public surface.
    pub complete: bool,
    /// Honest caveats about what the surface does and does not capture.
    pub notes: Vec<String>,
}

/// One module's slice of the public surface: its path, defining file, doc
/// comment, and the public items declared in it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Module {
    /// The module path (`hinzu_core::facts`), the grouping key.
    pub path: String,
    /// The module's defining file, when known.
    pub file: Option<String>,
    /// The module-level doc comment, when present.
    pub doc: Option<String>,
    /// The public items in this module, sorted by `(kind, name)`.
    pub items: Vec<ApiItem>,
}

/// One public item — a function, method, type, alias, or const — with the
/// common metadata every kind carries plus the kind-specific payload (a
/// [`Signature`] for callables, [`Field`]s for aggregates, [`Variant`]s for
/// enums, and so on). Unused payloads are empty/`null` for a given kind.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiItem {
    /// The item kind: `"function"`, `"method"`, `"struct"`, `"enum"`,
    /// `"trait"`, `"typeAlias"`, `"const"`, `"interface"`, `"class"`,
    /// `"record"`, …
    pub kind: String,
    /// A stable id — for Rust the rustdoc item path (`hinzu_core::facts::
    /// Definition`) so it can later cross-reference facts symbol ids.
    pub id: String,
    /// The short item name.
    pub name: String,
    /// The item's visibility (`"public"`, `"crate"`, `"private"`, …).
    pub visibility: String,
    /// The module path the item is declared in.
    #[serde(rename = "modulePath")]
    pub module_path: String,
    /// The defining file, when known.
    pub file: Option<String>,
    /// The first source line, when known.
    pub line: Option<u32>,
    /// The item's doc comment, when present.
    pub doc: Option<String>,
    /// The item's own generic parameters, rendered (`T`, `T: Clone`, `'a`).
    pub generics: Vec<String>,
    /// Whether the item is marked deprecated.
    pub deprecated: bool,
    /// The signature, for `function`/`method` items.
    pub signature: Option<Signature>,
    /// The fields, for `struct`/`record`/`interface`/`class` items (else empty).
    pub fields: Vec<Field>,
    /// The variants, for `enum` items (else empty).
    pub variants: Vec<Variant>,
    /// Implemented/extended traits or supertypes (`extends`/`implements`).
    pub implements: Vec<String>,
    /// The aliased type, for a `typeAlias` item.
    #[serde(rename = "aliasTarget")]
    pub alias_target: Option<String>,
    /// The declared type, for a `const` item.
    #[serde(rename = "constType")]
    pub const_type: Option<String>,
    /// The value, for a `const` item, when known.
    #[serde(rename = "constValue")]
    pub const_value: Option<String>,
}

impl ApiItem {
    /// A bare item with the given identity and an empty payload: `public`
    /// visibility, no file/line/doc, not deprecated, and every kind-specific
    /// field (`signature`, `fields`, `variants`, `implements`, alias/const) left
    /// empty. Each language extractor calls this and then fills only the fields
    /// its item kind carries — the one place the empty-item shape is written.
    pub fn new(kind: &str, id: &str, name: &str, module_path: &str) -> Self {
        ApiItem {
            kind: kind.to_string(),
            id: id.to_string(),
            name: name.to_string(),
            visibility: "public".to_string(),
            module_path: module_path.to_string(),
            file: None,
            line: None,
            doc: None,
            generics: Vec::new(),
            deprecated: false,
            signature: None,
            fields: Vec::new(),
            variants: Vec::new(),
            implements: Vec::new(),
            alias_target: None,
            const_type: None,
            const_value: None,
        }
    }
}

/// A callable's signature: its parameters (in order), rendered return type,
/// async-ness, receiver, a knowable error type, and its own generics.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Signature {
    /// The parameters, in declaration order (position is meaningful).
    pub params: Vec<Param>,
    /// The rendered return type, when there is one.
    #[serde(rename = "returnType")]
    pub return_type: Option<String>,
    /// Whether the callable is `async`.
    #[serde(rename = "isAsync")]
    pub is_async: bool,
    /// The receiver, for a method (`"&self"`, `"&mut self"`, `"self"`, or the
    /// owning type for an associated function); `null` for a free function.
    pub receiver: Option<String>,
    /// The error type, when knowable: a Rust `Result<_, E>` → `E`; later a
    /// JSDoc `@throws`. `null` when the callable is infallible or the error
    /// type is not statically knowable.
    #[serde(rename = "errorType")]
    pub error_type: Option<String>,
    /// The callable's own generic parameters, rendered.
    pub generics: Vec<String>,
}

/// One parameter of a callable.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Param {
    /// The parameter name (empty for a positional/tuple field).
    pub name: String,
    /// The rendered parameter type.
    pub ty: String,
    /// Whether the parameter is optional (e.g. an `Option<_>` or a defaulted
    /// argument in a language that has them).
    pub optional: bool,
    /// The default value, when the language models one.
    pub default: Option<String>,
}

/// One field of a struct/record/interface/class.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Field {
    /// The field name (empty for a tuple field).
    pub name: String,
    /// The rendered field type.
    pub ty: String,
    /// The field's visibility.
    pub visibility: String,
    /// The field's doc comment, when present.
    pub doc: Option<String>,
    /// Whether the field is optional (e.g. an `Option<_>`).
    pub optional: bool,
}

/// One variant of an enum.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Variant {
    /// The variant name.
    pub name: String,
    /// The variant's fields (tuple or struct payload), in order; empty for a
    /// unit variant.
    pub fields: Vec<Field>,
    /// The explicit discriminant, when one is set.
    pub discriminant: Option<String>,
    /// The variant's doc comment, when present.
    pub doc: Option<String>,
}

/// Normalize extracted per-module items into a deterministic [`ApiReport`].
///
/// This is the pure seam every language extractor lands in: it stamps the schema
/// version and sorts for a stable byte layout — [`modules`](ApiReport::modules)
/// by path and each module's top-level [`items`](Module::items) by
/// `(kind, name)` — while preserving the source order of fields, variants, and
/// params, whose position is meaningful. It transforms only in-memory data, so
/// it stays inside the functional-core region.
pub fn build_api(package: PackageInfo, fidelity: Fidelity, mut modules: Vec<Module>) -> ApiReport {
    for module in &mut modules {
        module
            .items
            .sort_by(|a, b| (&a.kind, &a.name).cmp(&(&b.kind, &b.name)));
    }
    modules.sort_by(|a, b| a.path.cmp(&b.path));
    ApiReport {
        hinzu_api_version: HINZU_API_VERSION,
        package,
        fidelity,
        modules,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal item with only the fields a test cares about; the rest take the
    /// neutral empty/`None` defaults from [`ApiItem::new`].
    fn item(kind: &str, name: &str) -> ApiItem {
        ApiItem::new(kind, &format!("m::{name}"), name, "m")
    }

    fn module(path: &str, items: Vec<ApiItem>) -> Module {
        Module {
            path: path.to_string(),
            file: None,
            doc: None,
            items,
        }
    }

    fn package() -> PackageInfo {
        PackageInfo {
            name: "demo".to_string(),
            language: "rust".to_string(),
            root: "demo".to_string(),
            version: None,
        }
    }

    fn fidelity() -> Fidelity {
        Fidelity {
            source: "rustdoc-json".to_string(),
            format_version: Some("60".to_string()),
            complete: false,
            notes: Vec::new(),
        }
    }

    #[test]
    fn build_api_stamps_version_and_sorts_modules_by_path() {
        let modules = vec![
            module("m::z", vec![item("struct", "Z")]),
            module("m::a", vec![item("struct", "A")]),
        ];
        let report = build_api(package(), fidelity(), modules);

        assert_eq!(report.hinzu_api_version, HINZU_API_VERSION);
        let paths: Vec<&str> = report.modules.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, vec!["m::a", "m::z"]);
    }

    #[test]
    fn build_api_sorts_items_by_kind_then_name() {
        // Deliberately out of order across both kind and name.
        let items = vec![
            item("struct", "Beta"),
            item("function", "zed"),
            item("struct", "Alpha"),
            item("function", "abc"),
        ];
        let report = build_api(package(), fidelity(), vec![module("m", items)]);

        let order: Vec<(&str, &str)> = report.modules[0]
            .items
            .iter()
            .map(|i| (i.kind.as_str(), i.name.as_str()))
            .collect();
        assert_eq!(
            order,
            vec![
                ("function", "abc"),
                ("function", "zed"),
                ("struct", "Alpha"),
                ("struct", "Beta"),
            ]
        );
    }

    #[test]
    fn build_api_preserves_source_order_of_fields_variants_and_params() {
        // Positional payloads must NOT be reordered, even though items are.
        let mut s = item("struct", "S");
        s.fields = vec![
            Field {
                name: "second".to_string(),
                ty: "u32".to_string(),
                visibility: "public".to_string(),
                doc: None,
                optional: false,
            },
            Field {
                name: "first".to_string(),
                ty: "u32".to_string(),
                visibility: "public".to_string(),
                doc: None,
                optional: false,
            },
        ];
        let mut f = item("function", "f");
        f.signature = Some(Signature {
            params: vec![
                Param {
                    name: "b".to_string(),
                    ty: "u8".to_string(),
                    optional: false,
                    default: None,
                },
                Param {
                    name: "a".to_string(),
                    ty: "u8".to_string(),
                    optional: false,
                    default: None,
                },
            ],
            return_type: None,
            is_async: false,
            receiver: None,
            error_type: None,
            generics: Vec::new(),
        });
        let report = build_api(package(), fidelity(), vec![module("m", vec![s, f])]);

        // f sorts before S (function < struct), but neither payload is reordered.
        let f_out = &report.modules[0].items[0];
        assert_eq!(f_out.name, "f");
        let pnames: Vec<&str> = f_out
            .signature
            .as_ref()
            .unwrap()
            .params
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(pnames, vec!["b", "a"]);

        let s_out = &report.modules[0].items[1];
        let fnames: Vec<&str> = s_out.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(fnames, vec!["second", "first"]);
    }

    #[test]
    fn report_round_trips_through_json() {
        let report = build_api(
            package(),
            fidelity(),
            vec![module("m", vec![item("enum", "E")])],
        );
        let json = serde_json::to_string(&report).unwrap();
        let back: ApiReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back.modules[0].items[0].name, "E");
        // The camelCase contract keys are on the wire.
        assert!(json.contains("\"modulePath\""));
        assert!(json.contains("\"hinzu_api_version\""));
    }
}
