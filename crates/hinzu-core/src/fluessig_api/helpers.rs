//! Pure string-level helpers for the fluessig converter: the rendered-TS-type
//! tokenizer/splitter primitives ([`Brackets`], [`split_top`], [`balanced`]),
//! the atom/literal predicates, and the alias-union expansion. Split out of
//! [`super`] to keep each file under the size limit; every function here is a
//! pure `&str`→value transform with no I/O.

use std::collections::{BTreeMap, BTreeSet};

use super::{FlEnumVariant, FlField, FlForeign, FlType, FlUnionVariant};

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
}

impl Brackets {
    /// Advance past one char, updating the running depth/string state. Returns
    /// `true` iff this char is ordinary **top-level content** — outside any
    /// string literal, at bracket-depth 0, and not itself a bracket or quote —
    /// i.e. the character stream a splitter/scanner reasons about. Brackets and
    /// quotes drive the state and return `false`.
    fn feed(&mut self, c: char) -> bool {
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
