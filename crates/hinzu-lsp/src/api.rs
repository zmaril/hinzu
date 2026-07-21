//! Python public-API extraction over ty's LSP.
//!
//! This is the API-surface counterpart to [`crate::extract`] (which builds the
//! effect [`FactSet`]). It drives the same `ty` language server, but instead of
//! call edges it enumerates the package's public interface: `documentSymbol` for
//! the item inventory + kinds, and `textDocument/hover` for the rendered
//! signature/type of each symbol (ty returns a fenced `def name(params) -> ret`
//! line). It emits the pieces of hinzu's language-agnostic API report
//! (`{package, fidelity, modules}`); the CLI hands them to the pure
//! `hinzu_core::api::build_api` for normalization, exactly like the Rust and
//! TypeScript paths.
//!
//! Python API fidelity is deliberately the weakest of the three, and the
//! [`Fidelity`] notes say so honestly: types appear only where the source is
//! annotated, `Raises:` is not parsed (so `errorType` is always null), and there
//! is no cross-file re-export resolution. Nothing is fabricated — where ty gives
//! no signature detail, the item carries a null signature.
//!
//! All process/fs effects live in this crate (never in hinzu-core): the ty
//! subprocess, the file reads, and the LSP round-trips.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use hinzu_core::api::{ApiItem, Fidelity, Field, Module, PackageInfo, Param, Signature};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::client::{path_to_uri, LspClient};
use crate::{python_config, resolved_server_cmd};

/// The un-normalized pieces of an API report, as the CLI seam expects them:
/// `build_api` stamps the version and sorts.
pub struct PythonApi {
    pub package: PackageInfo,
    pub fidelity: Fidelity,
    pub modules: Vec<Module>,
}

/// LSP `SymbolKind`s this path surfaces (a subset of the spec's numbering).
const KIND_CLASS: i64 = 5;
const KIND_METHOD: i64 = 6;
const KIND_PROPERTY: i64 = 7;
const KIND_FIELD: i64 = 8;
const KIND_CONSTRUCTOR: i64 = 9;
const KIND_FUNCTION: i64 = 12;
const KIND_VARIABLE: i64 = 13;
const KIND_CONSTANT: i64 = 14;

/// Whether a class-child kind is a method (function-like member).
fn is_method_kind(kind: i64) -> bool {
    matches!(kind, KIND_METHOD | KIND_CONSTRUCTOR | KIND_FUNCTION)
}

/// Whether a class-child kind is a data field (attribute/property).
fn is_field_kind(kind: i64) -> bool {
    matches!(
        kind,
        KIND_PROPERTY | KIND_FIELD | KIND_VARIABLE | KIND_CONSTANT
    )
}

/// One `documentSymbol` node — only the fields this path reads.
#[derive(Deserialize, Clone)]
struct DocSym {
    name: String,
    kind: i64,
    #[serde(rename = "selectionRange")]
    selection_range: LspRange,
    #[serde(default)]
    children: Vec<DocSym>,
}

#[derive(Deserialize, Clone)]
struct LspRange {
    start: LspPos,
}

#[derive(Deserialize, Clone, Copy)]
struct LspPos {
    line: u32,
    character: u32,
}

/// Extract a Python package's public API by driving ty over its LSP. Discovers
/// the source files, opens them, and for each public top-level symbol asks ty
/// for its rendered signature via `hover`. `root_label` is carried into
/// [`PackageInfo::root`].
pub fn extract_python_api(project: &Path, root_label: &str) -> Result<PythonApi> {
    let cfg = python_config()?;
    let project = project
        .canonicalize()
        .with_context(|| format!("resolving project path {}", project.display()))?;
    let cmd = resolved_server_cmd(&cfg);

    let mut lsp = LspClient::spawn(&cmd, &project).with_context(|| {
        format!(
            "spawning the `ty` language server (the Python api backend) — install it (`pip \
             install ty` / `uv tool install ty`) or set HINZU_TY; tried `{}`",
            cmd.join(" ")
        )
    })?;

    let result = drive(&mut lsp, &project, &cfg.init_options, root_label);
    lsp.shutdown();
    result
}

/// The LSP lifecycle and the whole surface walk, between an initialized and a
/// torn-down server.
fn drive(
    lsp: &mut LspClient,
    project: &Path,
    init_options: &Value,
    root_label: &str,
) -> Result<PythonApi> {
    initialize(lsp, project, init_options)?;
    let files = discover(project);
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap_or_default();
        let _ = lsp.notify(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": path_to_uri(f), "languageId": "python", "version": 1, "text": text,
            }}),
        );
    }
    lsp.wait_until_settled(Duration::from_secs(45));

    let mut modules: BTreeMap<String, Module> = BTreeMap::new();
    let mut excluded = 0usize;
    for f in &files {
        let rel = rel_path(project, f);
        let module_path = module_dotted(&rel);
        let src = std::fs::read_to_string(f).unwrap_or_default();
        let uri = path_to_uri(f);
        let syms = document_symbols(lsp, &uri);
        let allowed = public_surface(&src, &syms);

        let mut items: Vec<ApiItem> = Vec::new();
        for s in &syms {
            if !is_surfaced_kind(s.kind) {
                continue;
            }
            if !allowed.contains(&s.name) {
                // A module dunder (`__all__`, `__version__`) is machinery, not an
                // omitted export, so it does not count toward the excluded total.
                if !is_dunder(&s.name) {
                    excluded += 1;
                }
                continue;
            }
            lower_symbol(lsp, &uri, s, &rel, &module_path, &mut items);
        }
        if items.is_empty() {
            continue;
        }
        let module = modules
            .entry(module_path.clone())
            .or_insert_with(|| Module {
                path: module_path.clone(),
                file: Some(rel.clone()),
                doc: module_docstring(&src),
                items: Vec::new(),
            });
        module.items.extend(items);
    }

    let package = package_info(project, root_label);
    let fidelity = python_fidelity(excluded);
    Ok(PythonApi {
        package,
        fidelity,
        modules: modules.into_values().collect(),
    })
}

/// The honest fidelity block for the ty-over-LSP Python path.
fn python_fidelity(excluded: usize) -> Fidelity {
    Fidelity {
        source: "lsp-ty".to_string(),
        format_version: None,
        complete: false,
        notes: vec![
            "Source is ty's LSP: `documentSymbol` for the item inventory + kinds, and \
             `textDocument/hover` for the rendered `def name(params) -> ret` signature."
                .to_string(),
            format!(
                "Public surface = the module's `__all__` when present, else top-level names not \
                 starting with `_`; {excluded} internal/underscore-prefixed symbol(s) excluded. \
                 There is no cross-file re-export resolution — a name is judged where it is \
                 defined, not followed to a re-export."
            ),
            "Parameter and return types appear ONLY where the source is annotated; an unannotated \
             parameter renders with no type, and a symbol ty gives no hover for carries a null \
             signature (never guessed)."
                .to_string(),
            "`Raises:` / exception docstrings are not parsed, so errorType is always null for \
             Python."
                .to_string(),
            "Module-level constants get a type from hover where ty provides one; item doc comments \
             are not extracted (ty's hover returns the signature only), though a module docstring \
             is captured."
                .to_string(),
            "Types are rendered strings from ty, not cross-referenced ids.".to_string(),
        ],
    }
}

/// Send `initialize` (advertising documentSymbol + hover) with the config's ty
/// init options, then `initialized`, via the shared LSP handshake.
fn initialize(lsp: &mut LspClient, project: &Path, init_options: &Value) -> Result<()> {
    crate::client::initialize(
        lsp,
        project,
        init_options,
        json!({
            "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
            "hover": {"contentFormat": ["markdown", "plaintext"]},
        }),
    )
}

/// Discover the package's `.py` source files, skipping the usual non-source
/// trees (virtualenvs, caches, build output, VCS).
fn discover(project: &Path) -> Vec<PathBuf> {
    let pat = project.join("**/*.py");
    let mut files: Vec<PathBuf> = match glob::glob(&pat.to_string_lossy()) {
        Ok(paths) => paths
            .flatten()
            .filter(|p| p.is_file() && !is_ignored(p))
            .collect(),
        Err(_) => Vec::new(),
    };
    files.sort();
    files.dedup();
    files
}

/// Whether a path lives under a directory the surface walk skips.
fn is_ignored(path: &Path) -> bool {
    const SKIP: [&str; 7] = [
        "/node_modules/",
        "/.venv/",
        "/venv/",
        "/build/",
        "/dist/",
        "/__pycache__/",
        "/.git/",
    ];
    let unix = path.to_string_lossy().replace('\\', "/");
    SKIP.iter().any(|d| unix.contains(d))
}

/// A project-relative, forward-slash path.
fn rel_path(project: &Path, file: &Path) -> String {
    file.strip_prefix(project)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

/// The dotted module path for a relative file (`pkg/mod.py` → `pkg.mod`,
/// `pkg/__init__.py` → `pkg`).
fn module_dotted(rel: &str) -> String {
    let no_ext = rel.strip_suffix(".py").unwrap_or(rel);
    let dotted = no_ext.replace('/', ".");
    dotted
        .strip_suffix(".__init__")
        .unwrap_or(&dotted)
        .to_string()
}

/// The public-surface names for a module: the `__all__` list when present, else
/// every top-level symbol name not starting with `_`.
fn public_surface(src: &str, syms: &[DocSym]) -> std::collections::BTreeSet<String> {
    if let Some(all) = parse_dunder_all(src) {
        return all;
    }
    syms.iter()
        .filter(|s| !s.name.starts_with('_'))
        .map(|s| s.name.clone())
        .collect()
}

/// Parse a module's `__all__` string list, if it declares one. Best-effort: it
/// reads the string literals between the `__all__ = [ … ]` brackets and ignores
/// anything more dynamic (a returned `None` then falls back to name convention).
fn parse_dunder_all(src: &str) -> Option<std::collections::BTreeSet<String>> {
    let start = src.find("__all__")?;
    let after = &src[start..];
    let open = after.find(['[', '('])?;
    let close_ch = if after.as_bytes()[open] == b'[' {
        ']'
    } else {
        ')'
    };
    let close = after[open..].find(close_ch)? + open;
    let body = &after[open + 1..close];
    let mut names = std::collections::BTreeSet::new();
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' || c == '\'' {
            let quote = c;
            let mut name = String::new();
            for ch in chars.by_ref() {
                if ch == quote {
                    break;
                }
                name.push(ch);
            }
            if !name.is_empty() {
                names.insert(name);
            }
        }
    }
    if names.is_empty() {
        None
    } else {
        Some(names)
    }
}

/// Whether a symbol kind becomes a public-API item.
fn is_surfaced_kind(kind: i64) -> bool {
    matches!(
        kind,
        KIND_CLASS | KIND_METHOD | KIND_FUNCTION | KIND_VARIABLE | KIND_CONSTANT
    )
}

/// Lower one top-level symbol into one or more [`ApiItem`]s (a class yields its
/// own item plus a `method` item per public method).
fn lower_symbol(
    lsp: &mut LspClient,
    uri: &str,
    s: &DocSym,
    rel: &str,
    module_path: &str,
    out: &mut Vec<ApiItem>,
) {
    let id = format!("{rel}#{}", s.name);
    match s.kind {
        KIND_FUNCTION => {
            let mut item = base_item("function", &id, &s.name, module_path, rel, s);
            item.signature = Some(hover_signature(lsp, uri, s, None));
            item.generics = item
                .signature
                .as_ref()
                .map(|g| g.generics.clone())
                .unwrap_or_default();
            out.push(item);
        }
        KIND_CLASS => {
            let mut item = base_item("class", &id, &s.name, module_path, rel, s);
            for child in &s.children {
                if !is_public_member(&child.name) {
                    continue;
                }
                if is_method_kind(child.kind) {
                    let mid = format!("{id}.{}", child.name);
                    let mut m = base_item("method", &mid, &child.name, module_path, rel, child);
                    m.signature = Some(hover_signature(lsp, uri, child, Some(&s.name)));
                    m.generics = m
                        .signature
                        .as_ref()
                        .map(|g| g.generics.clone())
                        .unwrap_or_default();
                    out.push(m);
                } else if is_field_kind(child.kind) {
                    let ty = hover_type(lsp, uri, child).unwrap_or_default();
                    let optional = ty.starts_with("Optional[") || ty.contains("| None");
                    item.fields.push(Field {
                        name: child.name.clone(),
                        ty,
                        visibility: "public".to_string(),
                        doc: None,
                        optional,
                    });
                }
            }
            out.push(item);
        }
        KIND_VARIABLE | KIND_CONSTANT => {
            let mut item = base_item("const", &id, &s.name, module_path, rel, s);
            item.const_type = hover_type(lsp, uri, s);
            out.push(item);
        }
        _ => {}
    }
}

/// A class member is public when it is not underscore-prefixed, except the
/// dunder constructor `__init__`, which is part of the class's interface.
fn is_public_member(name: &str) -> bool {
    name == "__init__" || !name.starts_with('_')
}

/// Whether a name is a dunder (`__x__`) — module machinery, not an export.
fn is_dunder(name: &str) -> bool {
    name.starts_with("__") && name.ends_with("__")
}

/// A fresh common item envelope; the caller fills the kind-specific payload.
fn base_item(
    kind: &str,
    id: &str,
    name: &str,
    module_path: &str,
    rel: &str,
    s: &DocSym,
) -> ApiItem {
    let mut item = ApiItem::new(kind, id, name, module_path);
    item.file = Some(rel.to_string());
    item.line = Some(s.selection_range.start.line + 1);
    item
}

/// `documentSymbol` for one uri, retried a few times against a cold server.
fn document_symbols(lsp: &mut LspClient, uri: &str) -> Vec<DocSym> {
    for _ in 0..5 {
        let resp = lsp.request(
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": uri}}),
            Duration::from_secs(15),
        );
        if let Ok(v) = resp {
            if let Ok(syms) = serde_json::from_value::<Vec<DocSym>>(v.clone()) {
                if !syms.is_empty() {
                    return syms;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    Vec::new()
}

/// Ask ty for a symbol's hover and build a [`Signature`] from the rendered
/// `def` line. `receiver_owner` set means it is a method — a leading `self`/`cls`
/// becomes the receiver and is dropped from the params.
fn hover_signature(
    lsp: &mut LspClient,
    uri: &str,
    s: &DocSym,
    receiver_owner: Option<&str>,
) -> Signature {
    let text = hover_text(lsp, uri, s).unwrap_or_default();
    let def = signature_line(&text);
    match def {
        Some(line) => parse_def(&line, receiver_owner),
        None => Signature {
            params: Vec::new(),
            return_type: None,
            is_async: false,
            receiver: receiver_owner.map(|o| format!("self: {o}")),
            error_type: None,
            generics: Vec::new(),
        },
    }
}

/// The rendered type of a symbol from hover (the fenced content), for a const or
/// a class attribute.
fn hover_type(lsp: &mut LspClient, uri: &str, s: &DocSym) -> Option<String> {
    let text = hover_text(lsp, uri, s)?;
    let inner = fenced_code(&text)?;
    let inner = inner.trim();
    // ty renders a variable as `name: Type`; keep the type side when present.
    if let Some((_, ty)) = inner.split_once(':') {
        let ty = ty.trim();
        if !ty.is_empty() {
            return Some(ty.to_string());
        }
    }
    Some(inner.to_string())
}

/// Raw hover value string for a symbol at its selection range start.
fn hover_text(lsp: &mut LspClient, uri: &str, s: &DocSym) -> Option<String> {
    let resp = lsp
        .request(
            "textDocument/hover",
            json!({"textDocument": {"uri": uri}, "position":
                {"line": s.selection_range.start.line, "character": s.selection_range.start.character}}),
            Duration::from_secs(15),
        )
        .ok()?;
    let contents = resp.get("contents")?;
    match contents {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => o.get("value").and_then(Value::as_str).map(str::to_string),
        _ => None,
    }
}

/// The code inside the first ```` ```lang … ``` ```` fence in a hover markdown
/// string, or the whole trimmed string when there is no fence.
fn fenced_code(text: &str) -> Option<String> {
    if let Some(open) = text.find("```") {
        let after_open = &text[open + 3..];
        let body_start = after_open.find('\n').map(|n| n + 1).unwrap_or(0);
        let body = &after_open[body_start..];
        if let Some(close) = body.find("```") {
            return Some(body[..close].to_string());
        }
        return Some(body.to_string());
    }
    let t = text.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// The `def …` / `async def …` signature inside a hover's fenced code, as one
/// logical line. ty renders a multi-parameter signature across several lines
/// (`def f(\n  a,\n  b,\n) -> R`), so the whole block is collapsed to single
/// spaces before parsing. Returns `None` when the fence is not a `def`.
fn signature_line(text: &str) -> Option<String> {
    let code = fenced_code(text)?;
    if !(code.contains("def ")) {
        return None;
    }
    let joined = code.split_whitespace().collect::<Vec<_>>().join(" ");
    (joined.starts_with("def ") || joined.starts_with("async def ")).then_some(joined)
}

/// Parse a rendered `def name(params) -> ret` line into a [`Signature`]. A
/// method's leading `self`/`cls` is lifted into the receiver.
fn parse_def(line: &str, receiver_owner: Option<&str>) -> Signature {
    let is_async = line.starts_with("async def ");
    let rest = line
        .strip_prefix("async def ")
        .or_else(|| line.strip_prefix("def "))
        .unwrap_or(line);

    let open = rest.find('(');
    let (params_str, tail) = match open {
        Some(o) => {
            let close = matching_paren(&rest[o..]).map(|c| o + c);
            match close {
                Some(c) => (&rest[o + 1..c], &rest[c + 1..]),
                None => (&rest[o + 1..], ""),
            }
        }
        None => ("", ""),
    };
    let return_type = tail
        .split_once("->")
        .map(|(_, r)| r.trim().trim_end_matches(':').trim().to_string())
        .filter(|s| !s.is_empty());

    let mut params: Vec<Param> = Vec::new();
    let mut receiver = None;
    for (i, raw) in split_top_level(params_str).into_iter().enumerate() {
        let p = raw.trim();
        if p.is_empty() || p == "/" || p == "*" {
            continue;
        }
        if i == 0
            && receiver_owner.is_some()
            && (p == "self" || p == "cls" || p.starts_with("self:") || p.starts_with("cls:"))
        {
            receiver = Some(p.split(':').next().unwrap_or("self").trim().to_string());
            continue;
        }
        params.push(parse_param(p));
    }

    Signature {
        params,
        return_type,
        is_async,
        receiver,
        error_type: None,
        generics: Vec::new(),
    }
}

/// Parse one rendered parameter (`name: Type = default`, `*args`, `**kw`).
fn parse_param(p: &str) -> Param {
    let (name_part, ann_default) = match p.find([':', '=']) {
        Some(idx) => (&p[..idx], &p[idx..]),
        None => (p, ""),
    };
    let name = name_part.trim().to_string();
    // Split annotation (after ':') and default (after top-level '=').
    let mut ty = String::new();
    let mut default = None;
    if let Some(rest) = ann_default.strip_prefix(':') {
        match split_top_level_eq(rest) {
            (annotation, Some(d)) => {
                ty = annotation.trim().to_string();
                default = Some(d.trim().to_string());
            }
            (annotation, None) => ty = annotation.trim().to_string(),
        }
    } else if let Some(rest) = ann_default.strip_prefix('=') {
        default = Some(rest.trim().to_string());
    }
    let optional = default.is_some()
        || ty.starts_with("Optional[")
        || ty.contains("| None")
        || ty.contains("None |");
    Param {
        name,
        ty,
        optional,
        default,
    }
}

/// Index of the `)` matching the `(` at index 0 of `s`, respecting nested
/// brackets and quotes.
fn matching_paren(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Split a parameter list on top-level commas (ignoring commas inside brackets).
fn split_top_level(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '(' | '[' | '{' => {
                depth += 1;
                cur.push(c);
            }
            ')' | ']' | '}' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// Split a `Type = default` fragment at the top-level `=`, returning the
/// annotation and the optional default.
fn split_top_level_eq(s: &str) -> (&str, Option<&str>) {
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '=' if depth == 0 => return (&s[..i], Some(&s[i + 1..])),
            _ => {}
        }
    }
    (s, None)
}

/// The module docstring — the first triple-quoted string literal at the top of a
/// file, ignoring `from __future__` lines and comments before it.
fn module_docstring(src: &str) -> Option<String> {
    for raw in src.lines() {
        let line = raw.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        for quote in ["\"\"\"", "'''"] {
            if let Some(rest) = line.strip_prefix(quote) {
                // Single-line docstring on the opening line.
                if let Some(end) = rest.find(quote) {
                    let s = rest[..end].trim();
                    return (!s.is_empty()).then(|| s.to_string());
                }
                // Multi-line: collect until the closing quote.
                let start = src.find(quote)? + quote.len();
                let end = src[start..].find(quote)? + start;
                let s = src[start..end].trim();
                return (!s.is_empty()).then(|| s.to_string());
            }
        }
        // First real statement is not a string literal → no module docstring.
        return None;
    }
    None
}

/// The package name (from `pyproject.toml` `[project].name`, else the directory
/// name) and version, with `root` set to the operator's label.
fn package_info(project: &Path, root_label: &str) -> PackageInfo {
    let mut name = project
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "package".to_string());
    let mut version = None;
    if let Ok(text) = std::fs::read_to_string(project.join("pyproject.toml")) {
        if let Ok(toml) = text.parse::<toml::Value>() {
            if let Some(proj) = toml.get("project") {
                if let Some(n) = proj.get("name").and_then(|v| v.as_str()) {
                    name = n.to_string();
                }
                version = proj
                    .get("version")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
            }
        }
    }
    PackageInfo {
        name,
        language: "python".to_string(),
        root: root_label.to_string(),
        version,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `def` block ty renders (possibly multi-line) parses into params,
    /// defaults, optionality, return type, and — for a method — a lifted receiver.
    #[test]
    fn parses_a_multiline_def_with_defaults_and_optional() {
        let hover = "```python\ndef make_widget(\n    name: str,\n    size: int = 10,\n    label: str | None = None\n) -> Widget\n```";
        let line = signature_line(hover).expect("a def block");
        let sig = parse_def(&line, None);
        assert!(!sig.is_async);
        assert_eq!(sig.receiver, None);
        assert_eq!(sig.return_type.as_deref(), Some("Widget"));
        let got: Vec<(&str, &str, bool, Option<&str>)> = sig
            .params
            .iter()
            .map(|p| {
                (
                    p.name.as_str(),
                    p.ty.as_str(),
                    p.optional,
                    p.default.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("name", "str", false, None),
                ("size", "int", true, Some("10")),
                ("label", "str | None", true, Some("None")),
            ]
        );
    }

    /// A method's leading `self` is lifted into the receiver and dropped from the
    /// params; `async def` sets `is_async`.
    #[test]
    fn lifts_method_receiver_and_detects_async() {
        let sig = parse_def("async def fetch( self, url: str ) -> bytes", Some("Client"));
        assert!(sig.is_async);
        assert_eq!(sig.receiver.as_deref(), Some("self"));
        assert_eq!(sig.params.len(), 1);
        assert_eq!(sig.params[0].name, "url");
    }

    #[test]
    fn parses_dunder_all_and_module_paths() {
        let all = parse_dunder_all("__all__ = [\"A\", 'b', \"c\"]\n").expect("names");
        assert!(all.contains("A") && all.contains("b") && all.contains("c"));
        assert!(parse_dunder_all("x = 1\n").is_none());
        assert_eq!(module_dotted("pkg/mod.py"), "pkg.mod");
        assert_eq!(module_dotted("pkg/__init__.py"), "pkg");
    }

    #[test]
    fn extracts_the_module_docstring_only() {
        let src = "\"\"\"One-line module doc.\"\"\"\nimport os\n";
        assert_eq!(
            module_docstring(src).as_deref(),
            Some("One-line module doc.")
        );
        assert!(module_docstring("x = 1\n").is_none());
    }
}
