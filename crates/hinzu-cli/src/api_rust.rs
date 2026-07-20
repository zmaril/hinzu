//! The Rust public-API extraction path: drive `rustdoc --output-format=json`
//! over a target cargo crate, parse the emitted JSON, and lower it into
//! hinzu's language-agnostic [`ApiReport`].
//!
//! ## Why rustdoc JSON, not the StableMIR driver
//!
//! The effect pipeline's Rust source is the StableMIR driver, but StableMIR is
//! **monomorphized and reachability-scoped**: it sees what a program *reaches*,
//! not what a crate *declares public*. An API command needs the opposite — the
//! declared public surface with signatures, visibility, generics, doc comments,
//! and type shapes. `rustdoc --output-format=json` gives exactly that, for the
//! whole `pub` surface, without needing MIR or a reachability root. So this path
//! shells out to `cargo rustdoc` on the pinned nightly (rustdoc JSON is
//! nightly-only) rather than reusing the driver.
//!
//! ## Format-version resilience
//!
//! rustdoc's JSON `FORMAT_VERSION` changes across nightlies, so this module does
//! **not** depend on the `rustdoc-types` crate pinned to one version. It
//! deserializes only the fields it needs, navigating the item payloads as
//! [`serde_json::Value`], and records the observed `format_version` in the
//! report's [`Fidelity`] block. A field this code doesn't recognize is skipped,
//! not a hard error.
//!
//! All filesystem and process effects live here in the CLI; the core
//! [`hinzu_core::api::build_api`] only normalizes the in-memory result.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use hinzu_core::api::{
    build_api, ApiItem, ApiReport, Fidelity, Field, Module, PackageInfo, Param, Signature, Variant,
};
use serde::Deserialize;
use serde_json::Value;

/// The nightly rustdoc JSON extraction is pinned to, matching the StableMIR
/// driver's toolchain. rustdoc JSON is a nightly-only unstable option, so a
/// pinned nightly keeps the emitted `format_version` predictable. Overridable
/// with `HINZU_RUSTDOC_TOOLCHAIN` for a different nightly.
const RUSTDOC_NIGHTLY: &str = "nightly-2026-07-18";

/// Whether `path` looks like a cargo project (has a `Cargo.toml`).
pub fn is_cargo_project(path: &Path) -> bool {
    path.join("Cargo.toml").is_file()
}

/// Extract the public API of a cargo crate: run rustdoc JSON over it, parse the
/// output, and lower it into a normalized [`ApiReport`]. `root_label` is the
/// free-form target label carried into [`PackageInfo::root`] (usually the
/// project path as the operator typed it).
pub fn extract(project: &Path, root_label: &str) -> Result<ApiReport> {
    let doc = run_rustdoc(project)?;
    lower(&doc, root_label)
}

/// Run `cargo rustdoc --output-format json` on the pinned nightly over
/// `project`, into a fresh target dir, and read back the single emitted crate
/// JSON. Fails honestly when the nightly toolchain is missing rather than faking
/// a surface.
fn run_rustdoc(project: &Path) -> Result<RustdocCrate> {
    if !is_cargo_project(project) {
        bail!(
            "{} is not a cargo project (no Cargo.toml) — the Rust api path needs a crate to run \
             rustdoc over",
            project.display()
        );
    }
    let toolchain =
        std::env::var("HINZU_RUSTDOC_TOOLCHAIN").unwrap_or_else(|_| RUSTDOC_NIGHTLY.to_string());

    // A fresh target dir keeps the emitted doc/*.json isolated from any prior
    // build and easy to locate.
    let target_dir = std::env::temp_dir().join(format!("hinzu-api-rustdoc-{}", std::process::id()));
    std::fs::create_dir_all(&target_dir)
        .with_context(|| format!("creating rustdoc target dir {}", target_dir.display()))?;

    let status = Command::new("cargo")
        .current_dir(project)
        .arg(format!("+{toolchain}"))
        .args(["rustdoc", "--lib", "-Z", "unstable-options"])
        .env("CARGO_TARGET_DIR", &target_dir)
        .args(["--", "-Z", "unstable-options", "--output-format", "json"])
        .status()
        .with_context(|| {
            format!(
                "running `cargo +{toolchain} rustdoc` over {} — is the nightly toolchain \
                 installed? (rustup toolchain install {toolchain})",
                project.display()
            )
        })?;
    if !status.success() {
        bail!(
            "rustdoc JSON extraction failed for {} — the crate must build with `cargo +{toolchain} \
             rustdoc --lib`",
            project.display()
        );
    }

    let json_path = find_doc_json(&target_dir.join("doc"))?;
    let text = std::fs::read_to_string(&json_path)
        .with_context(|| format!("reading rustdoc JSON from {}", json_path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("parsing rustdoc JSON from {}", json_path.display()))
}

/// Locate the single crate JSON rustdoc emitted under `doc_dir`. rustdoc writes
/// one `<crate>.json` for the target crate; more than one is unexpected and
/// reported rather than guessed at.
fn find_doc_json(doc_dir: &Path) -> Result<PathBuf> {
    let mut jsons: Vec<PathBuf> = std::fs::read_dir(doc_dir)
        .with_context(|| format!("reading rustdoc output dir {}", doc_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    jsons.sort();
    match jsons.len() {
        0 => bail!("rustdoc produced no JSON under {}", doc_dir.display()),
        1 => Ok(jsons.remove(0)),
        _ => bail!(
            "rustdoc produced multiple JSON files under {} ({:?}); expected exactly one crate doc",
            doc_dir.display(),
            jsons
        ),
    }
}

// ---- the lean rustdoc JSON envelope --------------------------------------

/// The top of a rustdoc JSON document — only the fields this lowering needs.
/// Item payloads stay as [`Value`] so an unrecognized shape from a different
/// nightly degrades gracefully instead of failing the parse.
#[derive(Deserialize)]
struct RustdocCrate {
    /// The crate root module's id.
    root: Id,
    /// The crate version from its `Cargo.toml`, when set.
    crate_version: Option<String>,
    /// Every documented item, keyed by id.
    index: BTreeMap<String, Value>,
    /// The full path + kind for referenceable ids.
    paths: BTreeMap<String, PathInfo>,
    /// rustdoc's own JSON schema version, recorded for drift-awareness.
    format_version: u32,
}

/// A rustdoc item id. rustdoc numbers ids; they key `index` and `paths` as
/// strings.
#[derive(Deserialize, Clone)]
#[serde(transparent)]
struct Id(Value);

impl Id {
    /// The id as the string key rustdoc uses in `index`/`paths`.
    fn key(&self) -> String {
        match &self.0 {
            Value::Number(n) => n.to_string(),
            Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    }
}

/// The `paths` entry for an id: its full path segments. (rustdoc also carries a
/// `kind` here, ignored — the item's own `inner` is the authority on kind.)
#[derive(Deserialize)]
struct PathInfo {
    path: Vec<String>,
}

// ---- lowering rustdoc JSON into the ApiReport ----------------------------

/// Lower a parsed rustdoc document into a normalized [`ApiReport`]. Walks every
/// public top-level item, groups by module path, attaches inherent methods and
/// implemented traits to their owning type, and hands the per-module items to
/// the pure [`build_api`] for sorting.
fn lower(doc: &RustdocCrate, root_label: &str) -> Result<ApiReport> {
    let index = &doc.index;
    let paths = &doc.paths;

    // The crate name is the first segment of the root module's path.
    let crate_name = paths
        .get(&doc.root.key())
        .and_then(|p| p.path.first())
        .cloned()
        .unwrap_or_else(|| "crate".to_string());

    // Group items by module path. A module's own item seeds the module's
    // file/doc; every other item lands in its containing module.
    let mut modules: BTreeMap<String, Module> = BTreeMap::new();

    for (id, item) in index {
        let Some(inner) = item.get("inner").and_then(Value::as_object) else {
            continue;
        };
        // Only local, publicly-visible declarations. rustdoc ran without
        // `--document-private-items`, but methods/variants carry `default`
        // visibility inherited from their parent, so the top-level filter is on
        // the outer item's own visibility.
        if item.get("crate_id").and_then(Value::as_u64) != Some(0) {
            continue;
        }
        let Some(path_info) = paths.get(id) else {
            continue;
        };

        if inner.contains_key("module") {
            // Seed the module's own file + doc (its path is its full path).
            let mpath = path_info.path.join("::");
            let file = span_file(item);
            let d = doc_of(item);
            let m = module_entry(&mut modules, &mpath);
            m.file = file;
            m.doc = d;
            continue;
        }

        if !is_public(item) {
            continue;
        }
        let module_path = module_path_of(&path_info.path);
        let full_id = path_info.path.join("::");

        // A type item (struct/enum/trait) contributes its own item plus the
        // methods and traits hung off it; a plain item contributes just itself.
        let mut produced = Vec::new();
        if let Some(kind) = type_kind(inner) {
            produced.push(build_type_item(
                kind,
                &full_id,
                &module_path,
                item,
                inner,
                index,
                &mut Vec::new(),
            ));
            collect_type_members(&full_id, &module_path, inner, index, &mut produced);
        } else if let Some(base) = simple_item(&full_id, &module_path, item, inner) {
            produced.push(base);
        }

        if !produced.is_empty() {
            module_entry(&mut modules, &module_path)
                .items
                .extend(produced);
        }
    }

    let package = PackageInfo {
        name: crate_name,
        language: "rust".to_string(),
        root: root_label.to_string(),
        version: doc.crate_version.clone(),
    };
    let fidelity = rust_fidelity(doc.format_version);
    Ok(build_api(
        package,
        fidelity,
        modules.into_values().collect(),
    ))
}

/// The [`Module`] for `path`, inserting an empty one on first sight.
fn module_entry<'a>(modules: &'a mut BTreeMap<String, Module>, path: &str) -> &'a mut Module {
    modules.entry(path.to_string()).or_insert_with(|| Module {
        path: path.to_string(),
        file: None,
        doc: None,
        items: Vec::new(),
    })
}

/// The honest fidelity block for the Rust rustdoc-json path.
fn rust_fidelity(format_version: u32) -> Fidelity {
    Fidelity {
        source: "rustdoc-json".to_string(),
        format_version: Some(format_version.to_string()),
        complete: false,
        notes: vec![
            "Source is `rustdoc --output-format=json` on the declared public surface \
             (visibility=public), not a reachability-scoped view."
                .to_string(),
            "Types are rendered strings (e.g. `Vec<String>`, `Option<Bar>`), not \
             cross-referenced type ids — a documented follow-up."
                .to_string(),
            "Lifetimes are elided from rendered types for portability.".to_string(),
            "`throws` is not modeled for Rust; a `Result<_, E>` return's error type is \
             captured in signature.errorType when E is written explicitly."
                .to_string(),
            "Trait implementations are recorded by name in `implements`; their methods \
             are attributed to the trait, not re-emitted per impl. Auto-trait, blanket, \
             and negative impls are omitted."
                .to_string(),
            "Private struct fields are stripped by rustdoc and not shown; only the public \
             field shape is reported."
                .to_string(),
        ],
    }
}

/// The kind string for a type-like item's inner (`struct`/`enum`/`trait`), or
/// `None` for anything else.
fn type_kind(inner: &serde_json::Map<String, Value>) -> Option<&'static str> {
    if inner.contains_key("struct") {
        Some("struct")
    } else if inner.contains_key("enum") {
        Some("enum")
    } else if inner.contains_key("trait") {
        Some("trait")
    } else {
        None
    }
}

/// Build the top-level item for a type (struct/enum/trait): its common metadata,
/// its own fields/variants, and the traits it implements. Methods are collected
/// separately by [`collect_type_members`].
fn build_type_item(
    kind: &str,
    full_id: &str,
    module_path: &str,
    item: &Value,
    inner: &serde_json::Map<String, Value>,
    index: &BTreeMap<String, Value>,
    _scratch: &mut Vec<String>,
) -> ApiItem {
    let mut api = base_item(kind, full_id, module_path, item);
    match kind {
        "struct" => {
            api.fields = struct_fields(inner.get("struct"), index);
            api.implements = implemented_traits(inner.get("struct"), index);
            api.generics = generics_of(inner.get("struct"));
        }
        "enum" => {
            api.variants = enum_variants(inner.get("enum"), index);
            api.implements = implemented_traits(inner.get("enum"), index);
            api.generics = generics_of(inner.get("enum"));
        }
        "trait" => {
            api.generics = generics_of(inner.get("trait"));
        }
        _ => {}
    }
    api
}

/// Collect the method items hung off a type: the functions of its inherent
/// impls, and — for a trait — the trait's own methods. Each becomes a `method`
/// [`ApiItem`] owned by the type.
fn collect_type_members(
    owner_id: &str,
    module_path: &str,
    inner: &serde_json::Map<String, Value>,
    index: &BTreeMap<String, Value>,
    out: &mut Vec<ApiItem>,
) {
    // Inherent-impl methods (struct/enum).
    for tk in ["struct", "enum"] {
        let Some(impls) = inner.get(tk).and_then(|v| v.get("impls")) else {
            continue;
        };
        for impl_id in impls.as_array().into_iter().flatten() {
            let Some(impl_item) = index.get(&Id(impl_id.clone()).key()) else {
                continue;
            };
            let Some(imp) = impl_item.get("inner").and_then(|v| v.get("impl")) else {
                continue;
            };
            // Only inherent impls (no trait) contribute methods; trait-impl
            // methods belong to the trait (see fidelity notes).
            if !imp.get("trait").map(Value::is_null).unwrap_or(true) {
                continue;
            }
            push_methods(imp.get("items"), owner_id, module_path, index, out);
        }
    }
    // Trait's own methods.
    push_methods(
        inner.get("trait").and_then(|v| v.get("items")),
        owner_id,
        module_path,
        index,
        out,
    );
}

/// Push a `method` item for each id in `ids` (an impl's or trait's `items` list)
/// that resolves to a public function. Shared by the inherent-impl and trait
/// method walks.
fn push_methods(
    ids: Option<&Value>,
    owner_id: &str,
    module_path: &str,
    index: &BTreeMap<String, Value>,
    out: &mut Vec<ApiItem>,
) {
    for m_id in ids.and_then(Value::as_array).into_iter().flatten() {
        if let Some(m) = method_item(owner_id, module_path, &Id(m_id.clone()).key(), index) {
            out.push(m);
        }
    }
}

/// A `method` item for an impl/trait function `id` owned by `owner_id`, or
/// `None` when the id is not a public function.
fn method_item(
    owner_id: &str,
    module_path: &str,
    id: &str,
    index: &BTreeMap<String, Value>,
) -> Option<ApiItem> {
    let item = index.get(id)?;
    let func = item.get("inner")?.get("function")?;
    if !is_public(item) {
        return None;
    }
    let name = item.get("name")?.as_str()?.to_string();
    let full_id = format!("{owner_id}::{name}");
    let mut api = base_item("method", &full_id, module_path, item);
    api.name = name;
    api.signature = Some(signature_of(func, true));
    api.generics = api
        .signature
        .as_ref()
        .map(|s| s.generics.clone())
        .unwrap_or_default();
    Some(api)
}

/// A non-type, non-module item (function / typeAlias / const), or `None` for a
/// kind this path does not surface.
fn simple_item(
    full_id: &str,
    module_path: &str,
    item: &Value,
    inner: &serde_json::Map<String, Value>,
) -> Option<ApiItem> {
    if let Some(func) = inner.get("function") {
        let mut api = base_item("function", full_id, module_path, item);
        api.signature = Some(signature_of(func, false));
        api.generics = api
            .signature
            .as_ref()
            .map(|s| s.generics.clone())
            .unwrap_or_default();
        Some(api)
    } else if let Some(ta) = inner.get("type_alias") {
        let mut api = base_item("typeAlias", full_id, module_path, item);
        api.alias_target = ta.get("type").map(render_type);
        api.generics = generics_of(Some(ta));
        Some(api)
    } else if let Some(c) = inner.get("constant") {
        let mut api = base_item("const", full_id, module_path, item);
        api.const_type = c.get("type").map(render_type);
        api.const_value = c
            .get("const")
            .and_then(|k| k.get("value").or_else(|| k.get("expr")))
            .and_then(Value::as_str)
            .map(str::to_string);
        Some(api)
    } else {
        None
    }
}

/// Resolve a rustdoc id to its `index` item and the named `inner` payload
/// (`"variant"`, `"impl"`, …), or `None` when the id is absent or carries a
/// different payload. The shared front of the variant/impl walks.
fn resolve_inner<'a>(
    index: &'a BTreeMap<String, Value>,
    id: &Value,
    tag: &str,
) -> Option<(&'a Value, &'a Value)> {
    let item = index.get(&Id(id.clone()).key())?;
    let inner = item.get("inner")?.get(tag)?;
    Some((item, inner))
}

/// The common item metadata every kind shares. Payload fields (signature,
/// fields, …) start empty and are filled by the caller for the item's kind.
fn base_item(kind: &str, full_id: &str, module_path: &str, item: &Value) -> ApiItem {
    let name = item.get("name").and_then(Value::as_str).unwrap_or("");
    let mut api = ApiItem::new(kind, full_id, name, module_path);
    api.visibility = visibility_of(item);
    api.file = span_file(item);
    api.line = span_line(item);
    api.doc = doc_of(item);
    api.deprecated = !item.get("deprecation").map(Value::is_null).unwrap_or(true);
    api
}

/// Whether a rustdoc item's own visibility is public.
fn is_public(item: &Value) -> bool {
    item.get("visibility").and_then(Value::as_str) == Some("public")
}

/// The item's visibility as a normalized string: `public`, `crate`, `private`,
/// or `restricted` for a `pub(in path)`.
fn visibility_of(item: &Value) -> String {
    match item.get("visibility") {
        Some(Value::String(s)) if s == "public" => "public".to_string(),
        Some(Value::String(s)) if s == "crate" => "crate".to_string(),
        Some(Value::String(s)) if s == "default" => "private".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(_)) => "restricted".to_string(),
        _ => "private".to_string(),
    }
}

/// The item's doc comment, trimmed to `None` when empty.
fn doc_of(item: &Value) -> Option<String> {
    match item.get("docs") {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// The item's defining file from its span.
fn span_file(item: &Value) -> Option<String> {
    item.get("span")?
        .get("filename")?
        .as_str()
        .map(str::to_string)
}

/// The item's first source line from its span (`begin = [line, col]`).
fn span_line(item: &Value) -> Option<u32> {
    item.get("span")?
        .get("begin")?
        .as_array()?
        .first()?
        .as_u64()
        .map(|n| n as u32)
}

/// The module path for a full path — every segment but the last.
fn module_path_of(path: &[String]) -> String {
    if path.len() <= 1 {
        return path.join("::");
    }
    path[..path.len() - 1].join("::")
}

/// The rendered generic parameters of an item's inner (`struct`/`enum`/`fn`/…),
/// e.g. `T`, `T: Clone`, `'a`. Synthetic (compiler-inserted) params are skipped.
fn generics_of(inner: Option<&Value>) -> Vec<String> {
    let Some(params) = inner
        .and_then(|v| v.get("generics"))
        .and_then(|g| g.get("params"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    params.iter().filter_map(render_generic_param).collect()
}

/// Render one generic parameter definition (`name` + optional bounds), or `None`
/// for a synthetic type param the compiler inserted (e.g. a derive's `__S`).
fn render_generic_param(p: &Value) -> Option<String> {
    let name = p.get("name")?.as_str()?.to_string();
    let kind = p.get("kind")?;
    if let Some(tk) = kind.get("type") {
        if tk.get("is_synthetic").and_then(Value::as_bool) == Some(true) {
            return None;
        }
        let bounds = tk
            .get("bounds")
            .and_then(Value::as_array)
            .map(|b| b.iter().filter_map(render_bound).collect::<Vec<_>>())
            .unwrap_or_default();
        if bounds.is_empty() {
            Some(name)
        } else {
            Some(format!("{name}: {}", bounds.join(" + ")))
        }
    } else {
        // Lifetime or const param: the bare name (`'a`, `N`) is enough for v1.
        Some(name)
    }
}

/// Render a trait bound to its trait path, or `None` for a bound this path does
/// not render (an outlives/lifetime bound).
fn render_bound(b: &Value) -> Option<String> {
    b.get("trait_bound")?
        .get("trait")?
        .get("path")?
        .as_str()
        .map(str::to_string)
}

/// The public struct fields of a `struct` inner, in source order. Tuple-struct
/// fields render with empty names; private fields are already stripped by
/// rustdoc.
fn struct_fields(st: Option<&Value>, index: &BTreeMap<String, Value>) -> Vec<Field> {
    let Some(kind) = st.and_then(|v| v.get("kind")) else {
        return Vec::new();
    };
    // Plain (named) struct: kind = { "plain": { "fields": [ids] } }.
    if let Some(ids) = kind.get("plain").and_then(|p| p.get("fields")) {
        return field_items(ids, index);
    }
    // Tuple struct: kind = { "tuple": [id_or_null, ...] }.
    if let Some(ids) = kind.get("tuple").and_then(Value::as_array) {
        return field_items(&Value::Array(ids.clone()), index);
    }
    Vec::new()
}

/// Resolve a list of field ids (possibly containing `null` for a stripped
/// positional field) into [`Field`]s, in order.
fn field_items(ids: &Value, index: &BTreeMap<String, Value>) -> Vec<Field> {
    let Some(arr) = ids.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for id in arr {
        if id.is_null() {
            continue;
        }
        let Some(item) = index.get(&Id(id.clone()).key()) else {
            continue;
        };
        let Some(ty) = item.get("inner").and_then(|v| v.get("struct_field")) else {
            continue;
        };
        let rendered = render_type(ty);
        out.push(Field {
            name: item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            optional: is_option(&rendered),
            ty: rendered,
            visibility: visibility_of(item),
            doc: doc_of(item),
        });
    }
    out
}

/// The variants of an `enum` inner, in source order, each with its payload
/// fields (tuple or struct) and any explicit discriminant.
fn enum_variants(en: Option<&Value>, index: &BTreeMap<String, Value>) -> Vec<Variant> {
    let Some(ids) = en.and_then(|v| v.get("variants")).and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for id in ids {
        let Some((item, var)) = resolve_inner(index, id, "variant") else {
            continue;
        };
        out.push(Variant {
            name: item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            fields: variant_fields(var.get("kind"), index),
            discriminant: var
                .get("discriminant")
                .and_then(|d| d.get("value").or_else(|| d.get("expr")))
                .and_then(Value::as_str)
                .map(str::to_string),
            doc: doc_of(item),
        });
    }
    out
}

/// The payload fields of a variant kind: `plain` → none, `tuple` → positional,
/// `struct` → named.
fn variant_fields(kind: Option<&Value>, index: &BTreeMap<String, Value>) -> Vec<Field> {
    let Some(kind) = kind else {
        return Vec::new();
    };
    if kind.as_str() == Some("plain") {
        return Vec::new();
    }
    if let Some(ids) = kind.get("tuple") {
        return field_items(ids, index);
    }
    if let Some(ids) = kind.get("struct").and_then(|s| s.get("fields")) {
        return field_items(ids, index);
    }
    Vec::new()
}

/// The names of the traits a type implements, from its `impls` list: real,
/// user-visible trait impls only — auto-trait, blanket, synthetic, and negative
/// impls are omitted (see the fidelity notes).
fn implemented_traits(ty: Option<&Value>, index: &BTreeMap<String, Value>) -> Vec<String> {
    let Some(ids) = ty.and_then(|v| v.get("impls")).and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for id in ids {
        let Some((_item, imp)) = resolve_inner(index, id, "impl") else {
            continue;
        };
        if imp.get("is_synthetic").and_then(Value::as_bool) == Some(true)
            || imp.get("is_negative").and_then(Value::as_bool) == Some(true)
            || !imp.get("blanket_impl").map(Value::is_null).unwrap_or(true)
        {
            continue;
        }
        if let Some(name) = imp
            .get("trait")
            .and_then(|t| t.get("path"))
            .and_then(Value::as_str)
        {
            // `Structural{PartialEq,Eq}` are compiler-inserted markers behind a
            // `derive`, not a user-visible interface — drop them as noise.
            if name == "StructuralPartialEq" || name == "StructuralEq" {
                continue;
            }
            out.push(name.to_string());
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Build a [`Signature`] from a rustdoc `function` inner. When `is_method`, a
/// leading `self`/`&self`/`&mut self` input is lifted into
/// [`Signature::receiver`] and dropped from the params.
fn signature_of(func: &Value, is_method: bool) -> Signature {
    let header = func.get("header");
    let is_async = header
        .and_then(|h| h.get("is_async"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let sig = func.get("sig");
    let inputs = sig
        .and_then(|s| s.get("inputs"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut receiver = None;
    let mut params = Vec::new();
    for (i, input) in inputs.iter().enumerate() {
        let Some(pair) = input.as_array() else {
            continue;
        };
        let pname = pair.first().and_then(Value::as_str).unwrap_or("");
        let pty = pair.get(1);
        if is_method && i == 0 && pname == "self" {
            receiver = Some(render_receiver(pty));
            continue;
        }
        let rendered = pty.map(render_type).unwrap_or_default();
        params.push(Param {
            name: pname.to_string(),
            optional: is_option(&rendered),
            ty: rendered,
            default: None,
        });
    }

    let output = sig.and_then(|s| s.get("output"));
    let return_type = output.filter(|o| !o.is_null()).map(render_type);
    let error_type = output.and_then(result_error_type);

    Signature {
        params,
        return_type,
        is_async,
        receiver,
        error_type,
        generics: generics_of(Some(func)),
    }
}

/// Render a method receiver type to `self` / `&self` / `&mut self`, or the
/// rendered owning type for a typed receiver (`self: Box<Self>`).
fn render_receiver(ty: Option<&Value>) -> String {
    let Some(ty) = ty else {
        return "self".to_string();
    };
    if let Some(b) = ty.get("borrowed_ref") {
        let inner = b.get("type");
        if inner.map(is_self_generic).unwrap_or(false) {
            return if b.get("is_mutable").and_then(Value::as_bool) == Some(true) {
                "&mut self".to_string()
            } else {
                "&self".to_string()
            };
        }
    }
    if is_self_generic(ty) {
        return "self".to_string();
    }
    render_type(ty)
}

/// Whether a type node is the bare `Self` generic.
fn is_self_generic(ty: &Value) -> bool {
    ty.get("generic").and_then(Value::as_str) == Some("Self")
}

/// The error type of a `Result<_, E>` return, rendered — or `None` when the
/// output is not a two-argument `Result` (a bare `io::Result<_>` aliases the
/// error, so it is honestly left unknown).
fn result_error_type(output: &Value) -> Option<String> {
    let rp = output.get("resolved_path")?;
    let path = rp.get("path")?.as_str()?;
    if path != "Result" && !path.ends_with("::Result") {
        return None;
    }
    let args = rp
        .get("args")?
        .get("angle_bracketed")?
        .get("args")?
        .as_array()?;
    if args.len() != 2 {
        return None;
    }
    args[1].get("type").map(render_type)
}

/// Whether a rendered type string is an `Option<…>`.
fn is_option(rendered: &str) -> bool {
    rendered.starts_with("Option<") || rendered.starts_with("Option <")
}

// ---- the type renderer ----------------------------------------------------

/// Render a rustdoc type node into an honest string (`Vec<String>`,
/// `Option<Bar>`, `&mut T`). Navigates the node as [`Value`] so it survives
/// format-version drift; an unrecognized shape falls back to `"_"` rather than
/// panicking. Lifetimes are elided for portability.
fn render_type(ty: &Value) -> String {
    if let Some(prim) = ty.get("primitive").and_then(Value::as_str) {
        return prim.to_string();
    }
    if let Some(g) = ty.get("generic").and_then(Value::as_str) {
        return g.to_string();
    }
    if let Some(rp) = ty.get("resolved_path") {
        return render_resolved_path(rp);
    }
    if let Some(b) = ty.get("borrowed_ref") {
        let mut_kw = if b.get("is_mutable").and_then(Value::as_bool) == Some(true) {
            "mut "
        } else {
            ""
        };
        let inner = b.get("type").map(render_type).unwrap_or_default();
        return format!("&{mut_kw}{inner}");
    }
    if let Some(s) = ty.get("slice") {
        return format!("[{}]", render_type(s));
    }
    if let Some(a) = ty.get("array") {
        let inner = a.get("type").map(render_type).unwrap_or_default();
        let len = a.get("len").and_then(Value::as_str).unwrap_or("_");
        return format!("[{inner}; {len}]");
    }
    if let Some(t) = ty.get("tuple").and_then(Value::as_array) {
        let parts: Vec<String> = t.iter().map(render_type).collect();
        return format!("({})", parts.join(", "));
    }
    if let Some(rp) = ty.get("raw_pointer") {
        let mut_kw = if rp.get("is_mutable").and_then(Value::as_bool) == Some(true) {
            "mut "
        } else {
            "const "
        };
        let inner = rp.get("type").map(render_type).unwrap_or_default();
        return format!("*{mut_kw}{inner}");
    }
    if let Some(q) = ty.get("qualified_path") {
        let base = q.get("self_type").map(render_type).unwrap_or_default();
        let name = q.get("name").and_then(Value::as_str).unwrap_or("_");
        return format!("{base}::{name}");
    }
    if let Some(d) = ty.get("dyn_trait") {
        return render_dyn_trait(d);
    }
    if let Some(bounds) = ty.get("impl_trait").and_then(Value::as_array) {
        let parts: Vec<String> = bounds.iter().filter_map(render_bound).collect();
        return format!("impl {}", parts.join(" + "));
    }
    if ty.get("function_pointer").is_some() {
        return "fn(..)".to_string();
    }
    "_".to_string()
}

/// Render a `resolved_path` node: its path plus any angle-bracketed generic
/// arguments (`Vec<String>`, `BTreeMap<K, V>`). The path is used as rustdoc
/// spelled it (often the short name for a local type).
fn render_resolved_path(rp: &Value) -> String {
    let path = rp.get("path").and_then(Value::as_str).unwrap_or("_");
    let Some(args) = rp.get("args") else {
        return path.to_string();
    };
    let Some(angle) = args.get("angle_bracketed").and_then(|a| a.get("args")) else {
        return path.to_string();
    };
    let Some(arr) = angle.as_array() else {
        return path.to_string();
    };
    let rendered: Vec<String> = arr.iter().filter_map(render_generic_arg).collect();
    if rendered.is_empty() {
        path.to_string()
    } else {
        format!("{path}<{}>", rendered.join(", "))
    }
}

/// Render one generic argument of a path — a type argument becomes the rendered
/// type; a lifetime/const argument is elided (returns `None`).
fn render_generic_arg(arg: &Value) -> Option<String> {
    arg.get("type").map(render_type)
}

/// Render a `dyn Trait` node to `dyn Trait` (its first trait's path).
fn render_dyn_trait(d: &Value) -> String {
    let first = d
        .get("traits")
        .and_then(Value::as_array)
        .and_then(|t| t.first())
        .and_then(|t| t.get("trait"))
        .and_then(|t| t.get("path"))
        .and_then(Value::as_str)
        .unwrap_or("_");
    format!("dyn {first}")
}
