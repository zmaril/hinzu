//! The syn extractor for the curated-library derive tier: walk a cargo project's
//! `.rs` files and reduce each type's `impl`/`enum` blocks to the
//! [`TypeImplFacts`] the pure core's Tier-B predicates read (which derive a given
//! type's hand-written impls resemble).
//!
//! This is the CLI/adapter layer, so it is the only place that reads files. It is
//! deliberately **syntactic**: it reads a trait impl by its written path (so a
//! re-exported or aliased trait can be missed — a false negative, never faked),
//! and the honesty of that is carried by the `curated-pattern` source profile.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use hinzu_core::similarity::{TraitImpl, TypeImplFacts};
use syn::spanned::Spanned;

/// Extract the impl/enum facts of every local type under a cargo project
/// (skipping `target/`). One entry per `(file, type name)` seen. A file that
/// fails to parse is skipped with a warning rather than sinking the run.
pub fn extract(project: &Path) -> Result<Vec<TypeImplFacts>> {
    // Keyed by (file-rel, type-name) so impls attach to their type's declaration.
    let mut by_type: BTreeMap<(String, String), TypeImplFacts> = BTreeMap::new();

    for (rel, parsed) in crate::rust_source::parsed_rust_files(project)? {
        walk_items(&parsed.items, &rel, &mut by_type);
    }

    // Keep only types that actually carry a trait impl — a bare declaration is
    // not a derive-tier candidate.
    Ok(by_type
        .into_values()
        .filter(|f| !f.traits.is_empty())
        .collect())
}

/// Recursively walk items, recording enum/struct declarations and trait impls.
fn walk_items(
    items: &[syn::Item],
    file_rel: &str,
    by_type: &mut BTreeMap<(String, String), TypeImplFacts>,
) {
    for item in items {
        match item {
            syn::Item::Enum(e) => {
                let facts = entry(by_type, file_rel, &e.ident.to_string());
                facts.is_enum = true;
                facts.variant_count = e.variants.len() as u32;
                facts.line_start = e.span().start().line as u32;
                facts.line_end = e.span().end().line as u32;
            }
            syn::Item::Struct(s) => {
                let facts = entry(by_type, file_rel, &s.ident.to_string());
                facts.line_start = s.span().start().line as u32;
                facts.line_end = s.span().end().line as u32;
            }
            syn::Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    walk_items(inner, file_rel, by_type);
                }
            }
            syn::Item::Impl(im) => {
                if let Some(tr) = trait_impl_facts(im) {
                    let type_name = self_type_name(&im.self_ty);
                    let facts = entry(by_type, file_rel, &type_name);
                    // If the declaration was not seen (declared elsewhere), seed
                    // the location from the impl so the finding still points
                    // somewhere real.
                    if facts.line_start == 0 {
                        facts.line_start = tr.line_start;
                        facts.line_end = tr.line_end;
                    }
                    facts.traits.push(tr);
                }
            }
            _ => {}
        }
    }
}

/// The mutable facts entry for a `(file, type)`, created empty on first touch.
fn entry<'a>(
    by_type: &'a mut BTreeMap<(String, String), TypeImplFacts>,
    file_rel: &str,
    type_name: &str,
) -> &'a mut TypeImplFacts {
    by_type
        .entry((file_rel.to_string(), type_name.to_string()))
        .or_insert_with(|| TypeImplFacts {
            type_name: type_name.to_string(),
            file: file_rel.to_string(),
            line_start: 0,
            line_end: 0,
            is_enum: false,
            variant_count: 0,
            traits: Vec::new(),
        })
}

/// If this impl is a trait impl the curated patterns care about, reduce it to a
/// [`TraitImpl`]. Returns `None` for inherent impls and traits outside the set.
fn trait_impl_facts(im: &syn::ItemImpl) -> Option<TraitImpl> {
    let (_, path, _) = im.trait_.as_ref()?;
    let last = path.segments.last()?;
    let trait_name = last.ident.to_string();
    // Only the traits the catalog reads — keeps the fact set small and honest.
    if !matches!(
        trait_name.as_str(),
        "Display" | "Error" | "From" | "FromStr"
    ) {
        return None;
    }
    let trait_full = path
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect::<Vec<_>>()
        .join("::");

    let mut tr = TraitImpl {
        trait_name: trait_name.clone(),
        trait_full,
        from_arg_shape: None,
        body_is_match_self: false,
        is_wrapping: false,
        line_start: im.span().start().line as u32,
        line_end: im.span().end().line as u32,
    };

    match trait_name.as_str() {
        "Display" => {
            if let Some(block) = method_body(im, "fmt") {
                tr.body_is_match_self = body_matches_on_self(block);
            }
        }
        "From" => {
            tr.from_arg_shape = Some(from_arg_shape(last));
            if let Some(block) = method_body(im, "from") {
                tr.is_wrapping = from_body_is_wrapping(im, block);
            }
        }
        _ => {}
    }
    Some(tr)
}

/// The block of a named method in an impl, if present.
fn method_body<'a>(im: &'a syn::ItemImpl, name: &str) -> Option<&'a syn::Block> {
    im.items.iter().find_map(|it| match it {
        syn::ImplItem::Fn(f) if f.sig.ident == name => Some(&f.block),
        _ => None,
    })
}

/// The erased shape of the single generic argument of `From<T>` (`String`,
/// `Box<X>`, …), for the finding's evidence. Approximate: the last segment's
/// ident, or `_` for a bare/unknown type.
fn from_arg_shape(seg: &syn::PathSegment) -> String {
    if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
        if let Some(syn::GenericArgument::Type(ty)) = args.args.first() {
            return type_name(ty);
        }
    }
    "_".to_string()
}

/// The last path segment ident of a `Type::Path`, if any — the one piece
/// [`type_name`] and [`self_type_name`] share.
fn last_path_ident(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

/// A short display name for a type (its last path segment), else `_`.
fn type_name(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Reference(r) => format!("&{}", type_name(&r.elem)),
        _ => last_path_ident(ty).unwrap_or_else(|| "_".to_string()),
    }
}

/// The last path segment of an impl's self type (`impl … for Foo` → `Foo`).
fn self_type_name(ty: &syn::Type) -> String {
    last_path_ident(ty).unwrap_or_else(|| "impl".to_string())
}

/// Whether a block contains a `match self { … }` (the per-variant dispatch shape
/// a `Display`/`strum` derive generates). Unwraps `&self` / `*self`.
fn body_matches_on_self(block: &syn::Block) -> bool {
    struct Finder {
        found: bool,
    }
    impl<'ast> syn::visit::Visit<'ast> for Finder {
        fn visit_expr_match(&mut self, m: &'ast syn::ExprMatch) {
            if expr_is_self(&m.expr) {
                self.found = true;
            }
            syn::visit::visit_expr_match(self, m);
        }
    }
    let mut f = Finder { found: false };
    syn::visit::Visit::visit_block(&mut f, block);
    f.found
}

/// Whether an expression is `self` (through references / derefs).
fn expr_is_self(e: &syn::Expr) -> bool {
    match e {
        syn::Expr::Path(p) => p.path.is_ident("self"),
        syn::Expr::Reference(r) => expr_is_self(&r.expr),
        syn::Expr::Unary(syn::ExprUnary {
            op: syn::UnOp::Deref(_),
            expr,
            ..
        }) => expr_is_self(expr),
        syn::Expr::Paren(p) => expr_is_self(&p.expr),
        _ => false,
    }
}

/// Whether a `From::from` body just wraps its single argument into a
/// constructor: a single tail expression `Ctor(arg)` (or `Self::V(arg)` /
/// `E::V(arg)`) whose only argument is the parameter itself — the
/// `derive_more::From` shape. Conservative: a body that transforms its argument
/// (calls a method, converts, computes) does NOT match.
fn from_body_is_wrapping(im: &syn::ItemImpl, block: &syn::Block) -> bool {
    // The `from` parameter ident.
    let Some(param) = from_param_ident(im) else {
        return false;
    };
    // A single tail expression, no other statements.
    if block.stmts.len() != 1 {
        return false;
    }
    let tail = match &block.stmts[0] {
        syn::Stmt::Expr(e, _) => e,
        _ => return false,
    };
    match tail {
        // `Ctor(arg)` / `Self::V(arg)` / `E::V(arg)`.
        syn::Expr::Call(call) => {
            matches!(&*call.func, syn::Expr::Path(_))
                && call.args.len() == 1
                && expr_is_ident(&call.args[0], &param)
        }
        // Tuple-struct-like `Self(arg)` is also a Call; a struct literal
        // `Self { field: arg }` counts when its one field is the arg.
        syn::Expr::Struct(s) => s.fields.len() == 1 && expr_is_ident(&s.fields[0].expr, &param),
        _ => false,
    }
}

/// The single value parameter ident of a `from` method (skipping any receiver).
fn from_param_ident(im: &syn::ItemImpl) -> Option<String> {
    let f = im.items.iter().find_map(|it| match it {
        syn::ImplItem::Fn(f) if f.sig.ident == "from" => Some(f),
        _ => None,
    })?;
    for arg in &f.sig.inputs {
        if let syn::FnArg::Typed(pt) = arg {
            if let syn::Pat::Ident(id) = &*pt.pat {
                return Some(id.ident.to_string());
            }
        }
    }
    None
}

/// Whether an expression is exactly the identifier `name`.
fn expr_is_ident(e: &syn::Expr, name: &str) -> bool {
    matches!(e, syn::Expr::Path(p) if p.path.is_ident(name))
}
