//! Minting a NAMED INTERFACE from an anonymous object-of-methods.
//!
//! The #37 machinery mints a data [`super::FlModel`] from an inline object of
//! DATA fields (`{ a: T; b?: U }`). A distinct shape — an inline object whose
//! members are all METHOD signatures (`{ handleRpc(c: C): Promise<R>; close():
//! void }`) — is not a data record: it is a handle. Rather than degrading it to
//! `Json`, this module mints a named [`super::FlInterface`] with one op per
//! method (reusing the #37 naming/dedup discipline), declares it in `interfaces[]`,
//! and the caller references it with `{"model":"<MintedName>"}`. Per fluessig #92
//! (an interface returned by an op is constructible) + #93 (a Model-named
//! interface lowers to a handle class, async handle methods supported), the
//! returned handle lowers as a handle class with no fluessig change.
//!
//! Policy for a MIXED object (both data fields AND methods): an [`super::FlInterface`]
//! carries only ops, so a mixed object cannot be minted as one faithfully. Such an
//! object is NOT classified here (only an all-method object is) and falls through
//! to the #37 data path, which degrades honestly. No mixed objects occur in the
//! current pi surface; an all-data object stays a #37 model, unchanged.

use std::collections::BTreeMap;

use super::helpers::{
    fltype_key, is_ident, matching_close, parse_op_return, pascal, sanitize_param,
    split_object_member, split_top, unwrap_nullable,
};
use super::{Converter, FlInterface, FlOp, FlParam, FlType, Parsed, Stats};

/// The minting accumulator carried on the [`Converter`]: the minted interfaces
/// (keyed by name), the method-set-signature dedup table, and the coverage tally
/// for the ops the mints ADD to the API. Folded into the output and [`Stats`] once
/// every op has been parsed (see [`fold_minted_interfaces`]).
#[derive(Default)]
pub(super) struct MintAccum {
    /// Interfaces minted from anonymous method-objects, keyed by name.
    pub interfaces: BTreeMap<String, FlInterface>,
    /// Method-set signature → minted interface name, so two identical
    /// method-objects collapse to one interface (as #37 dedups models).
    pub by_sig: BTreeMap<String, String>,
    pub ops_total: usize,
    pub ops_clean: usize,
    pub ops_degraded: usize,
    pub params_total: usize,
    pub params_degraded: usize,
    pub returns_degraded: usize,
}

/// One method-signature member of an anonymous object-of-methods.
struct MethodSig {
    name: String,
    params_src: String,
    ret: String,
}

/// If EVERY non-empty member of an inline-object body is a method signature,
/// return the parsed methods; otherwise `None` (so the caller falls through to
/// the #37 data-field path). An empty set is `None`.
fn as_method_object(members: &[String]) -> Option<Vec<MethodSig>> {
    let mut out = Vec::new();
    for m in members {
        let m = m.trim();
        if m.is_empty() {
            continue;
        }
        out.push(parse_method_member(m)?);
    }
    (!out.is_empty()).then_some(out)
}

/// Parse one member as a method signature `name(params): ret` (or `name?(...)`).
/// `None` when it is not a method — a data field (`name: T`, whose top-level colon
/// precedes any `(`), an index signature (`[k: string]: T`), or an unparsable
/// member — so the object is not misclassified as method-bearing.
fn parse_method_member(s: &str) -> Option<MethodSig> {
    let s = s.trim();
    let open = s.find('(')?;
    // A data field whose type is a function (`onEvent: (e) => void`) has a
    // top-level colon BEFORE the first `(` — that is the #37 callback lane, not a
    // method. Reject so it is not swallowed here.
    let name_half = &s[..open];
    if name_half.contains(':') {
        return None;
    }
    let name = name_half
        .trim()
        .strip_suffix('?')
        .unwrap_or(name_half.trim());
    let name = name.trim();
    if !is_ident(name) {
        return None;
    }
    let close = matching_close(s, open)?;
    let params_src = s[open + 1..close].trim().to_string();
    let after = s[close + 1..].trim();
    let ret = match after.strip_prefix(':') {
        Some(r) => {
            let r = r.trim();
            if r.is_empty() {
                return None;
            }
            r.to_string()
        }
        // A rendered method signature always carries a `: ret`; a bare `foo()`
        // with no return annotation is not a shape we mint.
        None if after.is_empty() => "void".to_string(),
        None => return None,
    };
    Some(MethodSig {
        name: name.to_string(),
        params_src,
        ret,
    })
}

/// A deterministic, order-independent signature of a minted interface's op-set, so
/// two identical method-objects dedupe to a single minted interface.
fn method_set_signature(ops: &[FlOp]) -> String {
    let mut parts: Vec<String> = ops
        .iter()
        .map(|o| {
            let params: Vec<String> = o.params.iter().map(|p| fltype_key(&p.ty)).collect();
            format!(
                "{}({})->{}|async={}|{}",
                o.name,
                params.join(","),
                fltype_key(&o.returns),
                o.is_async,
                o.shape,
            )
        })
        .collect();
    parts.sort();
    parts.join(";")
}

impl Converter {
    /// If `members` is an anonymous object-of-methods, mint (deduped) a named
    /// [`FlInterface`] for it and return a `{"model":..}` handle ref; otherwise
    /// `None` so [`Converter::parse_inline_object`] handles the data-field case.
    pub(super) fn try_mint_method_interface(&mut self, members: &[String]) -> Option<Parsed> {
        let methods = as_method_object(members)?;
        // Build the ops FIRST (recursing `parse_type` on each param/return — with
        // its own idempotent side effects: union synthesis, context worklist). Only
        // then dedup, so an identical shape reuses the existing interface without
        // re-counting its ops.
        let mut ops = Vec::with_capacity(methods.len());
        let mut degraded = Vec::with_capacity(methods.len());
        for m in &methods {
            let (op, d) = self.build_mint_op(m);
            ops.push(op);
            degraded.push(d);
        }
        ops.sort_by(|a, b| a.name.cmp(&b.name));

        let sig = method_set_signature(&ops);
        if let Some(existing) = self.mint.by_sig.get(&sig) {
            return Some(Parsed::clean(FlType::Model {
                model: existing.clone(),
            }));
        }
        let name = self.unique_minted_name();
        self.known_models.insert(name.clone());
        self.mint.by_sig.insert(sig, name.clone());

        // Tally the ops this mint ADDS to the API (interface ops ARE counted in
        // `ops_total`, mirroring `build_op`), counted once per unique interface.
        for (op, d) in ops.iter().zip(&degraded) {
            self.mint.ops_total += 1;
            self.mint.params_total += op.params.len();
            if *d {
                self.mint.ops_degraded += 1;
            } else {
                self.mint.ops_clean += 1;
            }
        }
        self.mint.interfaces.insert(
            name.clone(),
            FlInterface {
                name: name.clone(),
                doc: Some(
                    "Minted from an anonymous object-of-methods returned by an op (a handle)."
                        .to_string(),
                ),
                ops,
            },
        );
        Stats::bump(
            &mut self.notes,
            "anonymous method-object → minted interface",
        );
        Some(Parsed::clean(FlType::Model { model: name }))
    }

    /// Build one [`FlOp`] from a parsed method signature, mirroring [`super::build_op`]:
    /// unwrap `Promise<…>` (→ async) / `AsyncIterable<…>` (→ stream) on the return,
    /// recurse `parse_type` on the return and each param, and unwrap a `| undefined`
    /// into the param's `optional`. Returns `(op, degraded)` where `degraded` is set
    /// when a param or (non-void) return fell back to `Json`.
    fn build_mint_op(&mut self, m: &MethodSig) -> (FlOp, bool) {
        let mut degraded = false;

        // Return: unwrap Promise (→ async) / Async{Iterable,Generator} (→ stream),
        // sharing the exact logic `build_op` uses for a source op.
        self.name_hint.push(format!("{}Result", pascal(&m.name)));
        let (returns, is_async, shape, ret_degraded) = parse_op_return(self, Some(&m.ret));
        self.name_hint.pop();
        if ret_degraded {
            self.mint.returns_degraded += 1;
            degraded = true;
        }

        let mut params = Vec::new();
        for raw in split_top(&m.params_src, ',') {
            if raw.trim().is_empty() {
                continue;
            }
            let (pname, pty) = match split_object_member(&raw) {
                Some((n, t)) => (n, t),
                // A bare positional type with no `name:` — keep the type, no name.
                None => (String::new(), raw.trim().to_string()),
            };
            let (pname, optional_marked) = match pname.strip_suffix('?') {
                Some(n) => (n.trim().to_string(), true),
                None => (pname, false),
            };
            let role = if pname.is_empty() {
                "Arg".to_string()
            } else {
                pascal(&pname)
            };
            self.name_hint.push(format!("{}{}", pascal(&m.name), role));
            let parsed = self.parse_type(&pty);
            self.name_hint.pop();
            if parsed.degraded {
                self.mint.params_degraded += 1;
                degraded = true;
            }
            let (ty, was_nullable) = unwrap_nullable(parsed.ty);
            let optional = if optional_marked || was_nullable {
                Some(true)
            } else {
                None
            };
            params.push(FlParam {
                name: sanitize_param(&pname),
                ty,
                optional,
            });
        }

        (
            FlOp {
                name: m.name.clone(),
                doc: None,
                shape: shape.to_string(),
                is_async,
                params,
                returns,
            },
            degraded,
        )
    }
}

/// Fold the interfaces minted from anonymous method-objects into `interfaces`, and
/// account the ops they ADDED into `stats` (interface ops are counted in
/// `ops_total`). Call once, after every op/model has been parsed.
pub(super) fn fold_minted_interfaces(
    conv: &mut Converter,
    stats: &mut Stats,
    interfaces: &mut Vec<FlInterface>,
) {
    let minted = std::mem::take(&mut conv.mint.interfaces);
    stats.interfaces_minted = minted.len();
    interfaces.extend(minted.into_values());
    stats.ops_total += conv.mint.ops_total;
    stats.ops_clean += conv.mint.ops_clean;
    stats.ops_degraded += conv.mint.ops_degraded;
    stats.params_total += conv.mint.params_total;
    stats.params_degraded += conv.mint.params_degraded;
    stats.returns_degraded += conv.mint.returns_degraded;
}
