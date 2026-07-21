//! Read a crate's public API from a pre-generated **rustdoc JSON** file and
//! reduce named items to Tier-A virtual [`VirtualSignature`]s the core scores
//! local signatures against.
//!
//! Generate the JSON with the pinned nightly (the same one the StableMIR driver
//! uses):
//!
//! ```sh
//! cargo +nightly-2026-07-18 rustdoc -p itertools -- \
//!     -Zunstable-options --output-format json
//! # → target/doc/itertools.json
//! ```
//!
//! rustdoc JSON exposes a public item's *signature and bounds*, never its body or
//! semantics — so a virtual signature built here carries only the erased
//! `type_shape` and arity, and the core matches it on those body-free signals.
//! That honesty is the `rustdoc` source profile's whole point. This reader is
//! deliberately tolerant: an item shape it does not recognize erases to `_`
//! rather than failing, and a missing item is reported, never faked.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use hinzu_core::similarity::{
    Arity, ExternalKind, ExternalRef, ExternalSource, MatchMode, StructuralSignature, TypeShape,
    VirtualSignature,
};
use serde_json::Value;

/// Build virtual signatures for the named `items` in a crate's rustdoc JSON.
/// Items not found are skipped with a stderr note (honest — a stale item name is
/// not faked into a match).
pub fn virtual_signatures_from_json(
    json_path: &Path,
    crate_name: &str,
    items: &[String],
    trust: f64,
    version: Option<String>,
) -> Result<Vec<VirtualSignature>> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(json_path)
        .with_context(|| format!("reading rustdoc JSON {}", json_path.display()))?;
    let doc: Value = serde_json::from_str(&text)
        .with_context(|| format!("parsing rustdoc JSON {}", json_path.display()))?;
    let index = doc
        .get("index")
        .and_then(Value::as_object)
        .context("rustdoc JSON has no `index` object")?;

    // Name → the first function item with that name.
    let mut wanted: BTreeMap<&str, bool> = items.iter().map(|i| (i.as_str(), false)).collect();
    let mut out = Vec::new();

    for entry in index.values() {
        let Some(name) = entry.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(seen) = wanted.get_mut(name) else {
            continue;
        };
        if *seen {
            continue; // take the first occurrence only
        }
        let Some(func) = entry.get("inner").and_then(|i| i.get("function")) else {
            continue;
        };
        *seen = true;
        let sig = reduce_function(name, crate_name, func);
        out.push(VirtualSignature {
            external: ExternalRef {
                library: crate_name.to_string(),
                item: name.to_string(),
                kind: ExternalKind::Function,
                source: ExternalSource::Rustdoc,
                version: version.clone(),
            },
            trust: trust.clamp(0.0, 1.0),
            match_mode: MatchMode::Signature,
            eliminates: format!(
                "a hand-written body with the signature shape of `{crate_name}::{name}`"
            ),
            signature: sig,
        });
    }

    for (name, seen) in &wanted {
        if !*seen {
            eprintln!(
                "libraries: rustdoc item `{crate_name}::{name}` not found in {} — skipped",
                json_path.display()
            );
        }
    }
    Ok(out)
}

/// Reduce a rustdoc `function` inner to a virtual [`StructuralSignature`]: the
/// erased type-shape of its inputs/output and its arity. There is no body, so
/// the control-flow, call-sequence, shingle, and histogram fields are left empty
/// — the core's `Signature` match mode never reads them.
fn reduce_function(name: &str, crate_name: &str, func: &Value) -> StructuralSignature {
    let sig = func.get("sig");
    let inputs = sig
        .and_then(|s| s.get("inputs"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let params: Vec<String> = inputs
        .iter()
        .map(|pair| {
            // Each input is `[name, type]`.
            pair.get(1)
                .map(render_type)
                .unwrap_or_else(|| "_".to_string())
        })
        .collect();
    let result = sig
        .and_then(|s| s.get("output"))
        .map(render_type)
        .unwrap_or_else(|| "_".to_string());

    let generics = func
        .get("generics")
        .and_then(|g| g.get("params"))
        .and_then(Value::as_array)
        .map(|a| a.len() as u32)
        .unwrap_or(0);

    StructuralSignature {
        symbol_id: format!("<rustdoc>::{crate_name}::{name}"),
        display: name.to_string(),
        language: "rust".to_string(),
        kind: "library_function".to_string(),
        file: format!("<rustdoc:{crate_name}>"),
        line_start: 0,
        line_end: 0,
        arity: Arity {
            params: params.len() as u32,
            results: if result == "_" { 0 } else { 1 },
            generics,
        },
        cfg: Default::default(),
        stmt_histogram: BTreeMap::new(),
        call_sequence: Vec::new(),
        type_shape: TypeShape { params, result },
        shingles: Vec::new(),
        token_len: 0,
        features: BTreeMap::new(),
    }
}

/// Render a rustdoc-JSON type to hinzu's erased type-shape: identifiers erased to
/// `_`, constructors kept when they carry generic arguments (`Result<_,_>`,
/// `Vec<_>`, `&_`) — the same erasure rule the syn extractor uses.
fn render_type(ty: &Value) -> String {
    // A generic param (`Self`, `T`, `F`) is a leaf → `_`.
    if ty.get("generic").is_some() {
        return "_".to_string();
    }
    // A primitive (`u32`, `str`) is a leaf → `_`.
    if ty.get("primitive").is_some() {
        return "_".to_string();
    }
    // `&T` / `&mut T`.
    if let Some(br) = ty.get("borrowed_ref") {
        if let Some(inner) = br.get("type") {
            return format!("&{}", render_type(inner));
        }
    }
    // A slice `[T]`.
    if let Some(inner) = ty.get("slice") {
        return format!("[{}]", render_type(inner));
    }
    // A tuple.
    if let Some(arr) = ty.get("tuple").and_then(Value::as_array) {
        if arr.is_empty() {
            return "()".to_string();
        }
        let inner: Vec<String> = arr.iter().map(render_type).collect();
        return format!("({})", inner.join(","));
    }
    // A nominal path, possibly with generic args.
    if let Some(rp) = ty.get("resolved_path") {
        let path = rp.get("path").and_then(Value::as_str).unwrap_or("_");
        let ctor = path.rsplit("::").next().unwrap_or(path);
        let args = rp
            .get("args")
            .and_then(|a| a.get("angle_bracketed"))
            .and_then(|a| a.get("args"))
            .and_then(Value::as_array);
        match args {
            Some(list) if !list.is_empty() => {
                let inner: Vec<String> = list
                    .iter()
                    .filter_map(|a| a.get("type").map(render_type))
                    .collect();
                if inner.is_empty() {
                    // A constructor with only lifetime/const args.
                    format!("{ctor}<_>")
                } else {
                    format!("{ctor}<{}>", inner.join(","))
                }
            }
            // A bare nominal (no generic args) is a leaf → erased.
            _ => "_".to_string(),
        }
    } else {
        "_".to_string()
    }
}
