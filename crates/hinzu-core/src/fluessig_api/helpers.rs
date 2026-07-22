//! Pure string-level helpers for the fluessig converter: the rendered-TS-type
//! tokenizer/splitter primitives ([`Brackets`], [`split_top`], [`balanced`]),
//! the atom/literal predicates, and the alias-union expansion. Split out of
//! [`super`] to keep each file under the size limit; every function here is a
//! pure `&str`→value transform with no I/O.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::{
    build_op, ApiItem, Converter, FlConst, FlEnum, FlEnumVariant, FlField, FlForeign, FlInterface,
    FlModel, FlType, FlUnionVariant, Parsed, Stats,
};

// ─────────────────────────── string helpers ─────────────────────────────────

/// Strip `//` line and `/* … */` block comments from a rendered type string,
/// respecting string-literal state so a `"http://…"` literal is left intact. TS
/// renders wide union aliases with interleaved source comments (`// State`,
/// `// Model`); left in place they would fuse onto the adjacent union member and
/// break its `{ … }` parse. A stripped comment leaves whitespace behind so
/// adjacent tokens stay separated (whitespace is collapsed by [`normalize`]).
/// Mirrors [`Brackets`]'s quote handling (no backslash-escape tracking, since
/// rendered type-level string literals do not carry escaped quotes).
pub(super) fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_str: Option<char> = None;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(q) = in_str {
            out.push(c);
            if c == q {
                in_str = None;
            }
            continue;
        }
        match c {
            '"' | '\'' | '`' => {
                in_str = Some(c);
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                // Line comment: drop through the end of line, preserving the
                // newline so the following member stays separated.
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                // Block comment: drop through the closing `*/`, leaving a space.
                chars.next();
                let mut prev = '\0';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    out
}

/// Collapse newlines/tabs/runs-of-spaces so a multi-line rendered type is one
/// tidy line, drop source comments, and drop a leading `|` (TS renders wide
/// unions with a leading bar).
pub(super) fn normalize(s: &str) -> String {
    let stripped = strip_comments(s);
    let mut out = String::with_capacity(stripped.len());
    let mut prev_space = false;
    for c in stripped.chars() {
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    let t = out.trim();
    t.strip_prefix("| ")
        .or_else(|| t.strip_prefix('|'))
        .unwrap_or(t)
        .trim()
        .to_string()
}

/// Tracks bracket-nesting depth and string-literal state while walking a
/// rendered type string left to right — the shared spine behind [`split_top`],
/// [`balanced`], and [`has_top_level_arrow`], which otherwise each repeat the
/// same depth/quote bookkeeping loop body.
#[derive(Default)]
struct Brackets {
    depth: i32,
    in_str: Option<char>,
    /// The previous non-consumed char, so `=>` is recognized: the `>` of an arrow
    /// is NOT a generic close and must not decrement depth (otherwise a top-level
    /// `|`/`,` after a nested `(x) => void` would be miscounted).
    prev: Option<char>,
}

impl Brackets {
    /// Advance past one char, updating the running depth/string state. Returns
    /// `true` iff this char is ordinary **top-level content** — outside any
    /// string literal, at bracket-depth 0, and not itself a bracket or quote —
    /// i.e. the character stream a splitter/scanner reasons about. Brackets and
    /// quotes drive the state and return `false`.
    fn feed(&mut self, c: char) -> bool {
        let prev = self.prev.replace(c);
        if let Some(q) = self.in_str {
            if c == q {
                self.in_str = None;
            }
            return false;
        }
        match c {
            '"' | '\'' | '`' => {
                self.in_str = Some(c);
                false
            }
            '<' | '(' | '[' | '{' => {
                self.depth += 1;
                false
            }
            // The `>` in an arrow `=>` is not a generic close — leave depth alone
            // (it is ordinary top-level content when at depth 0).
            '>' if prev == Some('=') => self.depth == 0,
            '>' | ')' | ']' | '}' => {
                self.depth -= 1;
                false
            }
            _ => self.depth == 0,
        }
    }
}

/// Split `s` on every top-level char satisfying `is_sep`, respecting `<>`,
/// `()`, `[]`, `{}` nesting and string literals. The shared spine behind
/// [`split_top`] and [`split_object_members`].
pub(super) fn split_top_by(s: &str, is_sep: impl Fn(char) -> bool) -> Vec<String> {
    let mut parts = Vec::new();
    let mut b = Brackets::default();
    let mut cur = String::new();
    for c in s.chars() {
        if b.feed(c) && is_sep(c) {
            parts.push(cur.trim().to_string());
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    if !cur.trim().is_empty() {
        parts.push(cur.trim().to_string());
    }
    parts
}

/// Split `s` on a top-level `sep` (nesting- and string-aware).
pub(super) fn split_top(s: &str, sep: char) -> Vec<String> {
    split_top_by(s, |c| c == sep)
}

/// Split an inline object body into its member strings, respecting nesting and
/// string literals. Object-type members are separated by `;` (or `,`).
pub(super) fn split_object_members(s: &str) -> Vec<String> {
    split_top_by(s, |c| c == ';' || c == ',')
}

/// Split one object member `name: T` / `name?: T` at its first top-level colon
/// into `(name, type)`. `None` when there is no top-level colon (a bare call
/// signature like `close(): void` splits at the colon after `)`, leaving a
/// non-ident name half that the caller rejects) — the `readonly` modifier, if
/// present, is stripped from the name half.
pub(super) fn split_object_member(s: &str) -> Option<(String, String)> {
    let mut b = Brackets::default();
    for (i, c) in s.char_indices() {
        if b.feed(c) && c == ':' {
            let name = s[..i].trim();
            let name = name.strip_prefix("readonly ").unwrap_or(name).trim();
            let ty = s[i + 1..].trim();
            if name.is_empty() || ty.is_empty() {
                return None;
            }
            return Some((name.to_string(), ty.to_string()));
        }
    }
    None
}

/// A deterministic, order-independent signature of a field-set, so two inline
/// objects with the same fields (in any order) dedupe to a single minted model.
pub(super) fn object_signature(fields: &[FlField]) -> String {
    let mut parts: Vec<String> = fields
        .iter()
        .map(|f| format!("{}={}|{}", f.name, fltype_key(&f.ty), f.nullable))
        .collect();
    parts.sort();
    parts.join(";")
}

/// A stable string key for an [`FlType`] (used only for dedup signatures — kept
/// self-contained so it stays inside hinzu-core's pure region).
pub(super) fn fltype_key(t: &FlType) -> String {
    match t {
        FlType::Scalar(s) => format!("s:{s}"),
        FlType::Model { model } => format!("m:{model}"),
        FlType::Enum { r#enum } => format!("e:{}", r#enum),
        FlType::List { list } => format!("l[{}]", fltype_key(list)),
        FlType::Nullable { nullable } => format!("n[{}]", fltype_key(nullable)),
        FlType::Union { union } => format!("u:{union}"),
        FlType::Foreign { foreign } => format!("f:{}", foreign.name),
        FlType::Callback { callback } => {
            let params: Vec<String> = callback.params.iter().map(fltype_key).collect();
            format!("cb[{}]", params.join(","))
        }
    }
}

/// `Foo<Bar>` with `head == "Foo"` → `Some("Bar")` (the whole inner, including
/// any nested generics/commas). Returns `None` unless `s` is exactly
/// `head<...>`.
pub(super) fn strip_generic(s: &str, head: &str) -> Option<String> {
    let rest = s.strip_prefix(head)?;
    let rest = rest.strip_prefix('<')?;
    let inner = rest.strip_suffix('>')?;
    // Guard against `head` being a prefix of a longer identifier
    // (`PromiseLike<…>` must not match `Promise`).
    if !balanced(inner) {
        return None;
    }
    Some(inner.trim().to_string())
}

/// Any generic `Head<Inner>` → `Some(("Head", "Inner"))`.
pub(super) fn split_generic_head(s: &str) -> Option<(&str, String)> {
    let open = s.find('<')?;
    if !s.ends_with('>') {
        return None;
    }
    let head = &s[..open];
    if !is_ident(head) {
        return None;
    }
    let inner = &s[open + 1..s.len() - 1];
    if !balanced(inner) {
        return None;
    }
    Some((head, inner.trim().to_string()))
}

/// A trailing balanced `[]` array suffix → the element type.
pub(super) fn strip_array_suffix(s: &str) -> Option<&str> {
    let inner = s.strip_suffix("[]")?;
    if inner.is_empty() || !balanced(inner) {
        return None;
    }
    Some(inner.trim())
}

/// Whether every bracket kind is balanced across `s` (so a split/strip did not
/// cut through a nested generic or tuple).
pub(super) fn balanced(s: &str) -> bool {
    let mut b = Brackets::default();
    for c in s.chars() {
        b.feed(c);
        if b.depth < 0 {
            return false;
        }
    }
    b.depth == 0
}

/// A top-level `=>` (a function type), ignoring arrows inside nested brackets.
pub(super) fn has_top_level_arrow(s: &str) -> bool {
    let mut b = Brackets::default();
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if b.feed(c) && c == '=' && chars.get(i + 1) == Some(&'>') {
            return true;
        }
    }
    false
}

pub(super) fn is_string_literal(s: &str) -> bool {
    (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
        || (s.starts_with('`') && s.ends_with('`') && s.len() >= 2)
}

/// Whether a rendered TS type is intrinsically **untyped** — `any`, `unknown`, or
/// `object`. Such a const has no fluessig type form (it would collapse to a
/// zero-information `Json`), so it is skipped rather than emitted. A named ref that
/// merely lacks a DTO form (a `class`) is NOT untyped and is handled by
/// [`super::Converter::parse_type`] instead.
pub(super) fn is_untyped_ts_type(s: &str) -> bool {
    matches!(s.trim(), "any" | "unknown" | "object")
}

/// If `s` is exactly ONE fully-quoted string literal (`"foo"`, `'foo'`, or a
/// substitution-free `` `foo` ``), return its unquoted contents. Returns `None`
/// for any compound or non-literal expression — a concatenation (`"a" + "b"`), a
/// disjunction (`pkg.version || "0.0.0"`), or a template with a `${…}`
/// substitution — so only a genuinely simple string literal yields a const value.
pub(super) fn simple_string_literal(s: &str) -> Option<String> {
    let s = s.trim();
    let mut chars = s.chars();
    let q = chars.next()?;
    if q != '"' && q != '\'' && q != '`' {
        return None;
    }
    if s.chars().count() < 2 || !s.ends_with(q) {
        return None;
    }
    let inner = &s[q.len_utf8()..s.len() - q.len_utf8()];
    // Reject a further unescaped closing quote of the same kind inside — that means
    // `s` is not a single literal (`"a" + "b"` → inner holds an unescaped `"`).
    let mut prev = '\0';
    for c in inner.chars() {
        if c == q && prev != '\\' {
            return None;
        }
        prev = c;
    }
    // A template literal with a substitution is a runtime expression, not a literal.
    if q == '`' && inner.contains("${") {
        return None;
    }
    Some(inner.to_string())
}

pub(super) fn is_numeric_literal(s: &str) -> bool {
    let t = s.strip_suffix('n').unwrap_or(s); // bigint literal `42n`
    !t.is_empty()
        && t.chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == '-' || c == '+')
        && t.chars().any(|c| c.is_ascii_digit())
}

pub(super) fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// The declared generic-parameter NAMES from an item/signature's rendered
/// generics (`["T", "TSchema extends ZodType", "K = string"]` → `{T, TSchema,
/// K}`). Each entry's leading identifier is taken, before any ` extends `, `:`,
/// or ` = ` constraint/default. Non-identifier entries (a Rust lifetime `'a`) are
/// skipped.
pub(super) fn generic_names(generics: &[String]) -> BTreeSet<String> {
    generics
        .iter()
        .filter_map(|g| {
            let head = g.split([' ', ':', '=']).next().unwrap_or("").trim();
            if is_ident(head) {
                Some(head.to_string())
            } else {
                None
            }
        })
        .collect()
}

/// Whether a bare name is a **generic type parameter** by its universal spelling:
/// a single uppercase ASCII letter (`T`, `U`, `K`, `R`). Multi-letter declared
/// generics (`TSchema`) are recognized from the owning item's generic list
/// instead (see `Converter::current_generics`), so they need no naming heuristic.
pub(super) fn is_generic_param(s: &str) -> bool {
    s.len() == 1 && s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

/// A dotted, module-qualified name (`net.Server`, `NodeJS.ProcessEnv`) → a
/// [`FlForeign`] opaque-handle payload. `rust_path` maps `a.B` → `a::B`. `None`
/// unless every dot-separated segment is a plain identifier and there are at
/// least two of them (so a lone `Foo` or a malformed expression is not opaqued
/// here — bare builtins go through [`builtin_foreign`]).
pub(super) fn dotted_foreign(s: &str) -> Option<FlForeign> {
    if !s.contains('.') {
        return None;
    }
    let segs: Vec<&str> = s.split('.').collect();
    if segs.len() < 2 || !segs.iter().all(|seg| is_ident(seg)) {
        return None;
    }
    Some(FlForeign {
        name: s.to_string(),
        rust_path: segs.join("::"),
    })
}

/// A curated allowlist of bare node/DOM/JS builtin type names that render WITHOUT
/// a module qualifier (`Server`, `ChildProcess`, `AbortSignal`) yet are truly
/// external — fluessig has no model for them. Maps each to its canonical source
/// name (a dotted node-module path where one applies) and a best-effort Rust path
/// for the generated opaque handle. Returns `None` for anything not known to be a
/// host builtin, so an unknown PascalCase name is presumed **pi-internal** (kept
/// as honest `Json`) rather than opaqued — the conservative default.
pub(super) fn builtin_foreign(s: &str) -> Option<FlForeign> {
    let (name, rust_path) = match s {
        // node:net — the socket/IPC server & connection. (Orchestrator's
        // `startIpcServer` returns `Server` imported from `node:net`.)
        "Server" => ("net.Server", "net::Server"),
        "Socket" => ("net.Socket", "net::Socket"),
        // node:child_process
        "ChildProcess" => ("child_process.ChildProcess", "std::process::Child"),
        // node:stream
        "Readable" => ("stream.Readable", "stream::Readable"),
        "Writable" => ("stream.Writable", "stream::Writable"),
        "Duplex" => ("stream.Duplex", "stream::Duplex"),
        // node:fs streams
        "ReadStream" => ("fs.ReadStream", "fs::ReadStream"),
        "WriteStream" => ("fs.WriteStream", "fs::WriteStream"),
        // Web/host globals — rendered bare upstream too (no module qualifier).
        "AbortSignal" => ("AbortSignal", "AbortSignal"),
        "AbortController" => ("AbortController", "AbortController"),
        _ => return None,
    };
    Some(FlForeign {
        name: name.to_string(),
        rust_path: rust_path.to_string(),
    })
}

/// A curated table of bare JS/lib **builtin** type names that have no pi
/// declaration yet carry a faithful cross-language DTO shape — so rather than
/// degrading them to `Json` (as an unresolved ref) they are mapped to a minted,
/// declared model. Currently the sole entry is the standard `Error` interface,
/// modeled as `{ name: string, message: string, stack?: string }` — the shape a
/// JS `Error` presents when handed to a callback across the binding boundary.
///
/// The model is named **`JsError`** (not `Error`) so it cannot collide with
/// Rust's `std::error::Error` / `anyhow::Error` in fluessig-gen's rust-core
/// output. `stack` is optional in the platform (`Error.prototype.stack` is
/// non-standard/absent in some engines), so it lowers to a nullable field.
///
/// FAITHFULNESS: only the three standard interface fields are modeled;
/// implementation-specific extras a concrete error subclass may carry (`cause`,
/// custom own-properties) are NOT captured — this is the portable common shape,
/// not a lossless mirror of any one engine's `Error`.
///
/// Returns `None` for any name not in the builtin table, so an unknown
/// PascalCase ref stays on the existing unresolved/foreign/pi-internal paths.
pub(super) fn builtin_model(s: &str) -> Option<FlModel> {
    match s {
        "Error" => Some(FlModel {
            name: "JsError".to_string(),
            doc: Some(
                "The standard JavaScript `Error` interface (name/message/stack), \
                 mapped from the JS builtin `Error` by hinzu api-fluessig. \
                 Implementation-specific extras (`cause`, custom properties) are \
                 not modeled."
                    .to_string(),
            ),
            fields: vec![
                FlField {
                    name: "name".to_string(),
                    ty: FlType::Scalar("string".to_string()),
                    nullable: false,
                },
                FlField {
                    name: "message".to_string(),
                    ty: FlType::Scalar("string".to_string()),
                    nullable: false,
                },
                FlField {
                    name: "stack".to_string(),
                    ty: FlType::Scalar("string".to_string()),
                    nullable: true,
                },
            ],
        }),
        _ => None,
    }
}

/// If a type came back `Nullable<T>`, peel it and report that it was nullable
/// (so the field/param `nullable`/`optional` flag carries it instead).
pub(super) fn unwrap_nullable(t: FlType) -> (FlType, bool) {
    match t {
        FlType::Nullable { nullable } => (*nullable, true),
        other => (other, false),
    }
}

/// A rendered destructured/rest param name (`{ a, b }`, `...args`) is not a Rust
/// ident; give it a stable placeholder.
pub(super) fn sanitize_param(name: &str) -> String {
    if name.is_empty() {
        "arg".to_string()
    } else if let Some(rest) = name.strip_prefix("...") {
        rest.to_string()
    } else if is_ident(name) {
        name.to_string()
    } else {
        "arg".to_string()
    }
}

/// Parse a rendered union type-alias target into enum variants iff every member
/// is a string literal.
pub(super) fn string_literal_union(alias: Option<&str>) -> Option<Vec<FlEnumVariant>> {
    let s = normalize(alias?);
    let members = split_top(&s, '|');
    if members.len() < 2 || !members.iter().all(|m| is_string_literal(m.trim())) {
        return None;
    }
    Some(
        members
            .iter()
            .map(|m| {
                let inner = m.trim().trim_matches(|c| c == '"' || c == '\'' || c == '`');
                FlEnumVariant {
                    name: inner.to_string(),
                    value: None,
                }
            })
            .collect(),
    )
}

/// Classify a top-level `typeAlias` target as a **liftable** union and, if so,
/// return its flattened member type-strings. Liftable shapes:
///
/// * a union of named types (`RpcCommand | RpcExtensionUIResponse`), and
/// * an `X[keyof X]` indexed-access — expanded to the value types of `X`'s
///   members when `X` is a known interface/record — optionally unioned with
///   extra members (`ResponseMap[keyof ResponseMap] | ErrorResponse`).
///
/// Returns `None` (leaving the alias dropped) for: a conditional/generic target
/// (`T extends … infer …`), a bare single alias (`type A = B`), an
/// all-string-literal union (that's the catalog-enum path, handled in pass 1),
/// or an indexed access whose base is not a known model (degrade gracefully).
///
/// NOTE: this parses the rendered `alias_target` **string** — the shape the
/// ApiReport carries today. A later pass (structured type refs) will have hinzu
/// emit structured members and retire this string parsing.
pub(super) fn expand_alias_union_members(
    target: Option<&str>,
    indexable: &BTreeMap<String, Vec<String>>,
) -> Option<Vec<String>> {
    let s = normalize(target?);
    // A conditional / `infer` / generic-constraint target is irreducible here.
    if s.contains(" extends ") || s.contains("infer ") {
        return None;
    }
    let top = split_top(&s, '|');
    if top.is_empty() {
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    let mut expanded_indexed = false;
    for member in &top {
        let m = member.trim();
        if let Some(base) = indexed_access_base(m) {
            // `X[keyof X]` → the value types of X's members (or ungraceful drop
            // when X is not a known interface/record).
            let values = indexable.get(base)?;
            out.extend(values.iter().cloned());
            expanded_indexed = true;
        } else {
            out.push(m.to_string());
        }
    }
    if out.is_empty() {
        return None;
    }
    // Liftable iff we expanded an indexed access, or it is a genuine multi-member
    // union (a single bare alias like `type A = B` is a different gap).
    if expanded_indexed || top.len() >= 2 {
        // Do not steal the string-literal-union → catalog-enum path.
        if out.iter().all(|m| is_string_literal(m.trim())) {
            return None;
        }
        Some(out)
    } else {
        None
    }
}

/// Recognize an `X[keyof X]` indexed-access expression and return its base `X`
/// (the map type whose value types the access enumerates). `None` for any other
/// shape, including `X[K]` with a specific key or a mismatched `X[keyof Y]`.
pub(super) fn indexed_access_base(s: &str) -> Option<&str> {
    let open = s.find('[')?;
    let inner = s.get(open + 1..)?.strip_suffix(']')?;
    let base = s[..open].trim();
    let key = inner.trim().strip_prefix("keyof ")?.trim();
    if is_ident(base) && key == base {
        Some(base)
    } else {
        None
    }
}

/// The flat interface name for a package's free functions: PascalCase of the
/// last path segment of the package name (`@earendil-works/pi-orchestrator` →
/// `PiOrchestrator`).
pub(super) fn package_interface_name(pkg: &str) -> String {
    let last = pkg.rsplit('/').next().unwrap_or(pkg);
    let p = pascal(last);
    if p.is_empty() {
        "Api".to_string()
    } else {
        p
    }
}

/// PascalCase from an arbitrary label (splitting on `-`, `_`, and spaces).
pub(super) fn pascal(s: &str) -> String {
    s.split(['-', '_', ' ', '.'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut cs = w.chars();
            match cs.next() {
                Some(f) => f.to_ascii_uppercase().to_string() + cs.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// Push a union variant with a collision-free camelCase tag derived from
/// `label` (disambiguating against `seen` with a numeric suffix). The shared
/// tail of both union builders ([`Converter::synthesize_union`] for anonymous
/// inline unions and [`Converter::lift_alias_union`] for named alias unions).
pub(super) fn push_unique_variant(
    variants: &mut Vec<FlUnionVariant>,
    seen: &mut BTreeSet<String>,
    label: &str,
    ty: FlType,
) {
    let mut tag = camel(label);
    let mut i = 2;
    while seen.contains(&tag) {
        tag = format!("{}{i}", camel(label));
        i += 1;
    }
    seen.insert(tag.clone());
    variants.push(FlUnionVariant { tag, ty });
}

/// A generic label for a structural (non-ident) union member: its scalar name,
/// else `"member"`. Used to name/tag a union built from such a member.
pub(super) fn structural_label(ty: &FlType) -> String {
    match ty {
        FlType::Scalar(s) => s.clone(),
        _ => "member".to_string(),
    }
}

/// camelCase from a label (PascalCase with a lowercased first char).
pub(super) fn camel(s: &str) -> String {
    let p = pascal(s);
    let mut cs = p.chars();
    match cs.next() {
        Some(f) => f.to_ascii_lowercase().to_string() + cs.as_str(),
        None => String::new(),
    }
}

// ─────────────────────────── callbacks ──────────────────────────────────────

/// The signature of an [`FlType::Callback`]. Byte-serde-identical to fluessig's
/// `CallbackSig` (camelCase, `skip_serializing_if` on the optional tail), so a
/// forward-only sync-void callback serializes to just `{"params":[…]}` — the
/// `returns`/`isAsync`/`fallible` keys are omitted. The converter only ever
/// mints the sync-void shape (`returns` = `void`, `is_async`/`fallible` false),
/// the only callback any fluessig backend lowers today.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FlCallbackSig {
    pub params: Vec<FlType>,
    #[serde(skip_serializing_if = "is_void_return")]
    pub returns: Box<FlType>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_async: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub fallible: bool,
}

impl FlCallbackSig {
    /// A forward-only sync-void callback over `params` — the only shape emitted.
    pub(super) fn sync_void(params: Vec<FlType>) -> Self {
        FlCallbackSig {
            params,
            returns: Box::new(FlType::Scalar("void".to_string())),
            is_async: false,
            fallible: false,
        }
    }
}

/// Is a callback `returns` the `void` scalar? Drives `skip_serializing_if` so a
/// sync-void callback omits the field — mirrors fluessig's `is_void_return`.
#[allow(clippy::borrowed_box)]
fn is_void_return(t: &Box<FlType>) -> bool {
    matches!(t.as_ref(), FlType::Scalar(s) if s == "void")
}

/// Does a return type carry a callback (possibly under a `Nullable`)? Such a
/// return is a register→unsubscribe idiom whose only fluessig home is
/// `Shape::Subscription`; the caller degrades it when no stateful (ctor-bearing)
/// interface is available.
pub(super) fn returns_is_callback(t: &FlType) -> bool {
    match t {
        FlType::Callback { .. } => true,
        FlType::Nullable { nullable } => returns_is_callback(nullable),
        _ => false,
    }
}

/// Split a normalized type on its FIRST top-level `=>` into `(head, ret)`
/// (nesting- and string-aware), or `None` when there is no top-level arrow.
pub(super) fn split_top_arrow(s: &str) -> Option<(&str, &str)> {
    let mut b = Brackets::default();
    for (i, c) in s.char_indices() {
        if b.feed(c) && c == '=' && s[i + 1..].starts_with('>') {
            return Some((s[..i].trim(), s[i + 2..].trim()));
        }
    }
    None
}

/// If `s` is a single fully-parenthesized group — the first `(` matches the last
/// char — return its trimmed interior. `(A | B)` → `A | B`; `((x) => void)` →
/// `(x) => void`; but `(x: T) => void` (whose first `(` closes before the end)
/// and `() => void` return `None`.
///
/// Tracks only grouping-bracket (`()`/`[]`/`{}`) depth and string state — it
/// deliberately does NOT treat `<`/`>` as brackets, so the `>` in an inner arrow
/// (`=>`) does not spuriously close the group. (The shared [`Brackets`] scanner
/// counts `<>` for generics, which would misread a parenthesized callback here.)
pub(super) fn strip_paren_group(s: &str) -> Option<&str> {
    let s = s.trim();
    if !s.starts_with('(') {
        return None;
    }
    let mut depth = 0i32;
    let mut in_str: Option<char> = None;
    let mut byte = 0usize;
    for c in s.chars() {
        if let Some(q) = in_str {
            if c == q {
                in_str = None;
            }
        } else {
            match c {
                '"' | '\'' | '`' => in_str = Some(c),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => {
                    depth -= 1;
                    if depth == 0 {
                        // The paren matching the leading `(` — a full group only
                        // when it is the final char.
                        return (byte + c.len_utf8() == s.len() && c == ')')
                            .then(|| s[1..byte].trim());
                    }
                }
                _ => {}
            }
        }
        byte += c.len_utf8();
    }
    None
}

/// Strip a `name:` / `name?:` / `readonly name:` prefix from one callback param,
/// yielding just its type. A bare type param (no top-level colon) is returned
/// unchanged. Reuses [`split_object_member`]'s top-level-colon split, so a colon
/// nested in the param's own type (`cb: (e: E) => void`) is not mistaken for the
/// name separator.
pub(super) fn strip_param_name(raw: &str) -> String {
    match split_object_member(raw) {
        Some((_, ty)) => ty,
        None => raw.trim().to_string(),
    }
}

/// Is a callback return string the void return (`void`/`undefined`/`never`/
/// empty)? A non-void or `Promise<…>` (async) return is outside the lowered shape.
fn is_void_ret(ret: &str) -> bool {
    let r = normalize(ret);
    r.is_empty() || matches!(r.as_str(), "void" | "undefined" | "never")
}

impl Converter {
    /// Parse a top-level function type `(params) => Ret` (known to carry a
    /// top-level `=>`). A forward-only sync-void callback (`Ret` is `void`/empty,
    /// not async) becomes a clean [`FlType::Callback`]; a non-void or async
    /// (`Promise<…>`) return, or an unparenthesized head, degrades honestly with a
    /// distinct counted reason. Each param's `name:`/`name?:` prefix is stripped
    /// and its type recursed through [`Converter::parse_type`] — so a param
    /// referencing an unresolved cross-package type degrades exactly as elsewhere
    /// (via `--context`), while the `Callback` wrapper itself stays clean.
    pub(super) fn parse_function_type(&mut self, s: &str) -> Parsed {
        let Some((head, ret)) = split_top_arrow(s) else {
            return self.degrade("function type");
        };
        // An async arrow renders its return as `Promise<…>`.
        if strip_generic(&normalize(ret), "Promise").is_some() {
            return self.degrade("async callback");
        }
        if !is_void_ret(ret) {
            return self.degrade("non-void callback");
        }
        // The head must be a parenthesized param list `( … )` (possibly empty).
        let Some(params_src) = strip_paren_group(head) else {
            return self.degrade("function type");
        };
        let mut params = Vec::new();
        let mut degraded = false;
        for raw in split_top(params_src, ',') {
            let p = self.parse_type(&strip_param_name(&raw));
            degraded |= p.degraded;
            params.push(p.ty);
        }
        Parsed {
            ty: FlType::Callback {
                callback: FlCallbackSig::sync_void(params),
            },
            degraded,
        }
    }
}

// ─────────────────────────── item → output builders ─────────────────────────

/// Build a DTO model from an `interface`/`record` item, tallying degraded fields.
pub(super) fn build_model(conv: &mut Converter, stats: &mut Stats, it: &ApiItem) -> FlModel {
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
pub(super) fn build_class_interface(
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
            if let Some(op) = build_op(conv, stats, &class.name, it) {
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

// ─────────────────────── interface refs & subscriptions ─────────────────────

/// Build a catalog [`FlEnum`] from a real `enum` item's variants (name + wire
/// discriminant). Shared by the primary and context namespace passes.
pub(super) fn enum_from_item(it: &ApiItem) -> FlEnum {
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

/// The names of primary-package `class` items that carry at least one method — a
/// method-bearing handle emitted as an interface. A value ref to one of these is
/// a typed handle (a `{"model":..}` reuse of the Model namespace, disambiguated
/// downstream by interface-set membership) rather than an untyped `Json`.
pub(super) fn method_bearing_classes(items: &[&ApiItem]) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    for c in items.iter().filter(|it| it.kind == "class") {
        let has_method = items.iter().any(|it| {
            it.kind == "method"
                && (it
                    .signature
                    .as_ref()
                    .and_then(|s| s.receiver.as_deref())
                    .map(|r| r == c.name)
                    .unwrap_or(false)
                    || it.id.starts_with(&format!("{}.", c.id)))
        });
        if has_method {
            set.insert(c.name.clone());
        }
    }
    set
}

/// A deferred register→unsubscribe op whose subscription-vs-degrade fate is
/// resolved by the constructibility post-pass (its owning interface must be
/// referenced by an interface-model ref somewhere to accept a `Shape::Subscription`).
pub(super) struct SubCandidate {
    pub interface: String,
    pub op: String,
    /// The op has exactly one callback param — the register→unsubscribe idiom.
    pub idiom: bool,
    /// A param (not the callback return) degraded, so promoting to a subscription
    /// does not by itself make the op clean.
    pub param_degraded: bool,
}

/// Collect the interface-model refs reachable in a type (unwrapping `Nullable`/
/// `List`, and descending into `Callback` params/return) into `out`.
fn collect_model_refs(t: &FlType, out: &mut BTreeSet<String>) {
    match t {
        FlType::Model { model } => {
            out.insert(model.clone());
        }
        FlType::List { list } => collect_model_refs(list, out),
        FlType::Nullable { nullable } => collect_model_refs(nullable, out),
        FlType::Callback { callback } => {
            for p in &callback.params {
                collect_model_refs(p, out);
            }
            collect_model_refs(&callback.returns, out);
        }
        _ => {}
    }
}

/// The set of CONSTRUCTIBLE interfaces: declared interfaces (`iface_set`) that are
/// referenced via a `FlType::Model` anywhere in an emitted return/value position.
/// fluessig accepts a `Shape::Subscription` only on such an interface.
pub(super) fn constructible_interfaces(
    interfaces: &[FlInterface],
    models: &[FlModel],
    consts: &[FlConst],
    iface_set: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut refs = BTreeSet::new();
    for i in interfaces {
        for op in &i.ops {
            collect_model_refs(&op.returns, &mut refs);
            for p in &op.params {
                collect_model_refs(&p.ty, &mut refs);
            }
        }
    }
    for m in models {
        for f in &m.fields {
            collect_model_refs(&f.ty, &mut refs);
        }
    }
    for c in consts {
        collect_model_refs(&c.ty, &mut refs);
    }
    refs.intersection(iface_set).cloned().collect()
}

/// Finalize each deferred register→unsubscribe op. When its owning interface is
/// constructible AND it is the single-callback-param idiom, flip it to a
/// `subscription` returning `void` (not counted degraded — a subscription fluessig
/// accepts). Otherwise keep the honest unary shape and degrade the callback return
/// to `Json` (counted), so no rejectable subscription is emitted.
pub(super) fn promote_subscriptions(
    interfaces: &mut [FlInterface],
    candidates: &[SubCandidate],
    constructible: &BTreeSet<String>,
    stats: &mut Stats,
    reasons: &mut BTreeMap<String, usize>,
) {
    for c in candidates {
        let op = interfaces
            .iter_mut()
            .find(|i| i.name == c.interface)
            .and_then(|i| i.ops.iter_mut().find(|o| o.name == c.op));
        if c.idiom && constructible.contains(&c.interface) {
            if let Some(op) = op {
                op.shape = "subscription".to_string();
                op.returns = FlType::Scalar("void".to_string());
            }
            if c.param_degraded {
                stats.ops_degraded += 1;
            } else {
                stats.ops_clean += 1;
            }
        } else {
            if let Some(op) = op {
                op.returns = FlType::json();
            }
            Stats::bump(reasons, "subscription return: interface has no ctor op");
            stats.returns_degraded += 1;
            stats.ops_degraded += 1;
        }
    }
}
