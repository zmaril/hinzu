//! The Rust structural extractor behind `hinzu similar`: parse a cargo project's
//! `.rs` files with `syn` and reduce each function body to a language-neutral
//! [`StructuralSignature`] the pure engine ([`hinzu_core::similarity`]) consumes.
//!
//! This is the CLI/adapter layer, so it is the only place that reads files. It is
//! deliberately **syntactic**: it walks the AST and never resolves a type, a
//! macro body, or a call target to its definition. That honesty is carried into
//! the analysis by the Rust/syn [`hinzu_core::similarity::LanguageProfile`], so a
//! finding always states what this extraction could and could not see.
//!
//! Each function (free `fn`, inherent/trait impl method, and trait default
//! method) becomes one signature: the body's AST-node-kind sequence drives the
//! shingles and the statement histogram; `if`/`match`/loops/`?`/`await` drive the
//! control-flow skeleton; call and method-call expressions drive the ordered call
//! sequence; and the signature's parameter/return types drive the arity and the
//! erased type shape.

use std::path::Path;

use anyhow::Result;
use hinzu_core::similarity::{Arity, Cfg, SignatureDoc, StructuralSignature, TypeShape, SHINGLE_K};
use syn::spanned::Spanned;
use syn::visit::{self, Visit};

/// Extract structural signatures from every `.rs` file under a cargo project
/// (skipping `target/`). Returns a [`SignatureDoc`] stamped `rust` / `syn`.
/// A file that fails to parse is skipped with a stderr warning rather than
/// failing the whole run — a project may contain a generated or edition-specific
/// file `syn` cannot read, and one bad file should not sink the analysis.
pub fn extract(project: &Path) -> Result<SignatureDoc> {
    let mut signatures = Vec::new();
    for (rel, parsed) in crate::rust_source::parsed_rust_files(project)? {
        walk_items(&parsed.items, &[], &rel, &mut signatures);
    }
    Ok(SignatureDoc {
        language: "rust".to_string(),
        extractor: "syn".to_string(),
        signatures,
    })
}

/// Recursively walk items, tracking the item path (module / impl-type / trait
/// segments) so each function gets a stable, qualified symbol id.
fn walk_items(
    items: &[syn::Item],
    path: &[String],
    file_rel: &str,
    out: &mut Vec<StructuralSignature>,
) {
    for item in items {
        match item {
            syn::Item::Fn(f) => {
                out.push(build_signature(
                    &f.sig,
                    Some(&f.block),
                    path,
                    file_rel,
                    "function",
                    f.span(),
                ));
            }
            syn::Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    let mut child = path.to_vec();
                    child.push(m.ident.to_string());
                    walk_items(inner, &child, file_rel, out);
                }
            }
            syn::Item::Impl(im) => {
                let mut child = path.to_vec();
                child.push(self_type_name(&im.self_ty));
                for ii in &im.items {
                    if let syn::ImplItem::Fn(m) = ii {
                        out.push(build_signature(
                            &m.sig,
                            Some(&m.block),
                            &child,
                            file_rel,
                            "impl_method",
                            m.span(),
                        ));
                    }
                }
            }
            syn::Item::Trait(tr) => {
                let mut child = path.to_vec();
                child.push(tr.ident.to_string());
                for ti in &tr.items {
                    if let syn::TraitItem::Fn(m) = ti {
                        // Only default methods have a body to fingerprint.
                        if let Some(block) = &m.default {
                            out.push(build_signature(
                                &m.sig,
                                Some(block),
                                &child,
                                file_rel,
                                "trait_method",
                                m.span(),
                            ));
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// The last path segment of an impl's self type, for the symbol-id qualifier
/// (`impl Foo` → `Foo`). Non-path self types fall back to `impl`.
fn self_type_name(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(p) => p
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_else(|| "impl".to_string()),
        _ => "impl".to_string(),
    }
}

/// Build one signature from a function's signature + body. `span` is the item's
/// span, for line numbers.
fn build_signature(
    sig: &syn::Signature,
    block: Option<&syn::Block>,
    path: &[String],
    file_rel: &str,
    kind: &str,
    span: proc_macro2::Span,
) -> StructuralSignature {
    let name = sig.ident.to_string();
    let mut segments = path.to_vec();
    segments.push(name.clone());
    let symbol_id = format!("{file_rel}::{}", segments.join("::"));

    let mut v = BodyVisitor::default();
    if let Some(block) = block {
        v.visit_block(block);
    }

    let stmt_histogram = histogram(&v.node_kinds);
    let shingles = shingles(&v.node_kinds);
    let token_len = v.node_kinds.len() as u32;

    let arity = Arity {
        params: sig.inputs.len() as u32,
        results: result_count(&sig.output),
        generics: sig
            .generics
            .params
            .iter()
            .filter(|p| matches!(p, syn::GenericParam::Type(_)))
            .count() as u32,
    };

    let type_shape = TypeShape {
        params: sig.inputs.iter().map(param_shape).collect(),
        result: result_shape(&sig.output),
    };

    let mut features = std::collections::BTreeMap::new();
    if v.has_macro {
        features.insert("has_macro".to_string(), "true".to_string());
    }
    if sig.asyncness.is_some() {
        features.insert("is_async".to_string(), "true".to_string());
    }
    if v.has_await {
        features.insert("has_await".to_string(), "true".to_string());
    }

    let line_start = span.start().line as u32;
    let line_end = span.end().line as u32;

    StructuralSignature {
        symbol_id,
        display: name,
        language: "rust".to_string(),
        kind: kind.to_string(),
        file: file_rel.to_string(),
        line_start,
        line_end,
        arity,
        cfg: Cfg {
            branch_count: v.branch_count,
            match_arms: v.match_arms,
            loop_count: v.loop_count,
            try_count: v.try_count,
            return_points: v.return_points,
            max_nesting: v.max_depth,
        },
        stmt_histogram,
        call_sequence: v.call_sequence,
        type_shape,
        shingles,
        token_len,
        features,
    }
}

/// The number of declared results: 0 for a unit/absent return, else 1 (a tuple
/// return counts once — it is one shape).
fn result_count(output: &syn::ReturnType) -> u32 {
    match output {
        syn::ReturnType::Default => 0,
        syn::ReturnType::Type(_, ty) => {
            if is_unit(ty) {
                0
            } else {
                1
            }
        }
    }
}

/// The erased result shape (`"_"` for unit/absent).
fn result_shape(output: &syn::ReturnType) -> String {
    match output {
        syn::ReturnType::Default => "_".to_string(),
        syn::ReturnType::Type(_, ty) => type_shape_str(ty),
    }
}

/// Whether a type is the unit tuple `()`.
fn is_unit(ty: &syn::Type) -> bool {
    matches!(ty, syn::Type::Tuple(t) if t.elems.is_empty())
}

/// The erased shape of one function parameter (`self` → `"self"`).
fn param_shape(arg: &syn::FnArg) -> String {
    match arg {
        syn::FnArg::Receiver(_) => "self".to_string(),
        syn::FnArg::Typed(pt) => type_shape_str(&pt.ty),
    }
}

/// Reduce a type to its structural shape, erasing identifiers and lifetimes to
/// `_` while keeping constructors: `Vec<Foo>` → `Vec<_>`, `Result<T,E>` →
/// `Result<_,_>`, `&str` → `&_`, a bare nominal `Foo`/`u32` → `_`. A segment
/// counts as a constructor precisely when it carries generic arguments, which is
/// what makes "same shape, different types" a strong signal.
fn type_shape_str(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Reference(r) => format!("&{}", type_shape_str(&r.elem)),
        syn::Type::Ptr(p) => format!("*{}", type_shape_str(&p.elem)),
        syn::Type::Slice(s) => format!("[{}]", type_shape_str(&s.elem)),
        syn::Type::Array(a) => format!("[{};_]", type_shape_str(&a.elem)),
        syn::Type::Group(g) => type_shape_str(&g.elem),
        syn::Type::Paren(p) => type_shape_str(&p.elem),
        syn::Type::Tuple(t) => {
            if t.elems.is_empty() {
                "()".to_string()
            } else {
                let inner: Vec<String> = t.elems.iter().map(type_shape_str).collect();
                format!("({})", inner.join(","))
            }
        }
        syn::Type::Path(p) => path_shape(p),
        syn::Type::ImplTrait(_) => "impl _".to_string(),
        syn::Type::TraitObject(_) => "dyn _".to_string(),
        syn::Type::BareFn(_) => "fn(_)".to_string(),
        syn::Type::Never(_) => "!".to_string(),
        _ => "_".to_string(),
    }
}

/// The shape of a path type: the last segment's constructor name kept when it has
/// generic arguments (each argument recursively shaped), else the whole nominal
/// erased to `_`.
fn path_shape(p: &syn::TypePath) -> String {
    let Some(last) = p.path.segments.last() else {
        return "_".to_string();
    };
    match &last.arguments {
        syn::PathArguments::AngleBracketed(args) => {
            let inner: Vec<String> = args
                .args
                .iter()
                .filter_map(|a| match a {
                    syn::GenericArgument::Type(t) => Some(type_shape_str(t)),
                    // Lifetimes vanish; const/binding generics erase to `_`.
                    syn::GenericArgument::Lifetime(_) => None,
                    _ => Some("_".to_string()),
                })
                .collect();
            if inner.is_empty() {
                // e.g. `Foo<'a>` — a constructor with only a lifetime arg.
                format!("{}<_>", last.ident)
            } else {
                format!("{}<{}>", last.ident, inner.join(","))
            }
        }
        // A bare nominal (no generics) is a leaf identifier → erased.
        _ => "_".to_string(),
    }
}

/// The statement histogram: node-kind counts over the visited sequence.
fn histogram(kinds: &[String]) -> std::collections::BTreeMap<String, u32> {
    let mut h = std::collections::BTreeMap::new();
    for k in kinds {
        *h.entry(k.clone()).or_insert(0) += 1;
    }
    h
}

/// k-gram (k=[`SHINGLE_K`]) FNV-1a hashes over the node-kind sequence. A sequence
/// shorter than k yields a single shingle over the whole sequence, so tiny bodies
/// still fingerprint.
fn shingles(kinds: &[String]) -> Vec<u64> {
    if kinds.is_empty() {
        return Vec::new();
    }
    if kinds.len() < SHINGLE_K {
        return vec![fnv1a64(&kinds.join("|"))];
    }
    let mut out = Vec::with_capacity(kinds.len() - SHINGLE_K + 1);
    for window in kinds.windows(SHINGLE_K) {
        out.push(fnv1a64(&window.join("|")));
    }
    out
}

/// FNV-1a over a string, for stable, platform-independent shingle hashes.
fn fnv1a64(s: &str) -> u64 {
    let mut hash: u64 = 0xCBF29CE484222325;
    for b in s.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001B3);
    }
    hash
}

/// The AST walker that reduces a body to its structural counts. Block nesting is
/// tracked by `visit_block`; every expression pushes its node kind (for the
/// shingles + histogram) and updates the control-flow counters; call and
/// method-call expressions also record the callee simple-name.
#[derive(Default)]
struct BodyVisitor {
    node_kinds: Vec<String>,
    call_sequence: Vec<String>,
    branch_count: u32,
    match_arms: u32,
    loop_count: u32,
    try_count: u32,
    return_points: u32,
    depth: u32,
    max_depth: u32,
    has_macro: bool,
    has_await: bool,
}

impl<'ast> Visit<'ast> for BodyVisitor {
    fn visit_block(&mut self, b: &'ast syn::Block) {
        self.depth += 1;
        self.max_depth = self.max_depth.max(self.depth);
        visit::visit_block(self, b);
        self.depth -= 1;
    }

    fn visit_stmt(&mut self, s: &'ast syn::Stmt) {
        match s {
            syn::Stmt::Local(_) => self.node_kinds.push("let".to_string()),
            syn::Stmt::Macro(_) => {
                self.node_kinds.push("macro".to_string());
                self.has_macro = true;
            }
            _ => {}
        }
        visit::visit_stmt(self, s);
    }

    fn visit_expr(&mut self, e: &'ast syn::Expr) {
        let kind = match e {
            syn::Expr::Call(c) => {
                if let syn::Expr::Path(p) = &*c.func {
                    if let Some(seg) = p.path.segments.last() {
                        self.call_sequence.push(seg.ident.to_string());
                    }
                }
                "call"
            }
            syn::Expr::MethodCall(m) => {
                self.call_sequence.push(m.method.to_string());
                "method_call"
            }
            syn::Expr::If(_) => {
                self.branch_count += 1;
                "if"
            }
            syn::Expr::Match(m) => {
                self.match_arms += m.arms.len() as u32;
                "match"
            }
            syn::Expr::While(_) | syn::Expr::ForLoop(_) | syn::Expr::Loop(_) => {
                self.loop_count += 1;
                "loop"
            }
            syn::Expr::Try(_) => {
                self.try_count += 1;
                "try"
            }
            syn::Expr::Await(_) => {
                self.has_await = true;
                "await"
            }
            syn::Expr::Return(_) => {
                self.return_points += 1;
                "return"
            }
            syn::Expr::Assign(_) => "assign",
            syn::Expr::Closure(_) => "closure",
            syn::Expr::Macro(_) => {
                self.has_macro = true;
                "macro"
            }
            syn::Expr::Binary(_) => "binary",
            syn::Expr::Unary(_) => "unary",
            syn::Expr::Field(_) => "field",
            syn::Expr::Index(_) => "index",
            syn::Expr::Struct(_) => "struct",
            syn::Expr::Reference(_) => "ref",
            syn::Expr::Tuple(_) => "tuple",
            syn::Expr::Array(_) => "array",
            syn::Expr::Range(_) => "range",
            syn::Expr::Let(_) => "let",
            syn::Expr::Path(_) => "path",
            syn::Expr::Lit(_) => "lit",
            _ => "expr",
        };
        self.node_kinds.push(kind.to_string());
        visit::visit_expr(self, e);
    }
}
