//! A `rustc_public` (StableMIR) custom rustc driver that extracts a
//! monomorphized call graph plus standard-library effect roots from a real
//! crate, emitting JSON facts in hinzu's `FactSet` schema (definitions, edges,
//! effect_roots) — so the output deserializes directly through
//! `hinzu_core::facts::FactSet::from_json`.
//!
//! # Reference-level edges (the higher-order / import-time rung)
//!
//! Walking `TerminatorKind::Call` alone is *call-only*: a function used as a
//! **value** — passed as an argument (`register(foo)`), assigned (`let f =
//! foo;`), returned, reified to a fn-pointer, or captured in a closure handed
//! elsewhere — is invisible, so its effect never reaches the function that
//! handed it off. Import-time effects in `static`/`const` initializers are
//! likewise missed (their bodies were dropped as bare, un-walked definitions).
//! This driver adds `Edge{kind: reference, resolution: reference}` edges for
//! those non-call *uses*, resolved through the SAME `Instance::resolve` →
//! provenance → effect path as calls (see [`CallCollector`]). It mirrors the
//! Python tree-sitter reference rung (PR #20) but works natively from MIR, which
//! gives strictly more than a tree-sitter + LSP pass would. The rung is
//! **sound-additive**: it only ADDS edges/effects, so no real violation the
//! call-only pass found can vanish; what it adds is the higher-order and
//! import-time effects the call graph missed.
//!
//! Ported from the slice-1 spike. Pinned to nightly-2026-07-18 (rustc 1.99.0),
//! where the crate is named `rustc_public` (the renamed `stable_mir`). The API
//! shape was read from the shipped rustc-src: `rustc_public::{run,
//! all_local_items, CrateDef}`, `mir::mono::Instance::{try_from, resolve}`,
//! `mir::TerminatorKind::Call`, `ty::TyKind::RigidTy(RigidTy::FnDef(..))`.
//! Template lineage: rust-lang/rust
//! tests/ui-fulldeps/rustc_public/check_instance.rs.
//!
//! Run as a `RUSTC_WORKSPACE_WRAPPER`, so cargo invokes it as
//! `hinzu-rustc-driver <real-rustc> <rustc args…>`; the injected rustc path at
//! argv[1] is stripped (the clippy-driver pattern). Each wrapped crate writes
//! its own `facts-<crate>-<pid>.json` into `HINZU_FACTS_DIR` (default `/tmp`);
//! the CLI adapter merges them.
#![feature(rustc_private)]

extern crate rustc_driver;
extern crate rustc_interface;
extern crate rustc_middle;
extern crate rustc_public;

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::ops::ControlFlow;

use rustc_public::mir::mono::{Instance, InstanceKind};
use rustc_public::mir::{AggregateKind, MirVisitor, Operand, Rvalue, TerminatorKind};
use rustc_public::ty::{AdtDef, FnDef, GenericArgKind, GenericArgs, RigidTy, Ty, TyKind};
use rustc_public::{CrateDef, ItemKind};
use serde::Serialize;

/// The whole fact set, serialized in hinzu's `FactSet` JSON schema.
#[derive(Serialize, Default)]
struct Facts {
    definitions: Vec<Def>,
    edges: Vec<Edge>,
    effect_roots: Vec<EffectRoot>,
}

/// A callable. `id` is the monomorphized display name, which is also what the
/// edges use as caller/callee, so a definition's summary attaches by id.
#[derive(Serialize)]
struct Def {
    id: String,
    display: String,
    language: String, // always "rust"
    file: String,
    line_start: u32,
    line_end: u32,
}

/// A "caller uses callee" edge. `kind` is `call` for a MIR `Call` terminator
/// (`resolution` = `call` when statically resolved, `unresolved` for an indirect
/// fn-pointer / dyn call), `reference` for a function/closure *used as a value*
/// rather than called (`resolution` = `reference`), or `type` for a
/// signature-type dependency — a function → an ADT named in its parameters or
/// return, resolved statically to the ADT's declaration (`resolution` =
/// `reference`). Call and reference edges carry effects; a `type` edge never
/// does (a signature dependency is not a call).
#[derive(Serialize)]
struct Edge {
    caller: String,
    callee: String,
    kind: String,
    resolution: String,
    evidence_file: String,
    evidence_line: u32,
}

/// A seeded effectful root: a standard-library operation that *is* an effect.
#[derive(Serialize)]
struct EffectRoot {
    symbol: String,
    effect: String,
}

/// Strip balanced `<...>` generic-argument groups from a monomorphized name so
/// effect matching runs on the callee's *path*, not on type arguments. Without
/// this, `Option::<std::fs::FileType>::is_some_and` matches `std::fs` — a false
/// positive, because `std::fs::FileType` is a type argument, not a callee.
fn strip_generics(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut depth = 0usize;
    for c in name.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// Match a callee path (generics stripped) to a hinzu effect category, or
/// `None`. The spellings are exactly hinzu's `Effect` enum, so the emitted
/// `effect_roots` parse back through `FactSet::from_json`.
fn effect_category(name: &str) -> Option<&'static str> {
    let path = strip_generics(name);
    let checks: &[(&str, &str)] = &[
        ("std::fs::", "fs"),
        ("std::net::", "net"),
        ("std::process::", "process"),
        ("std::time::", "clock"),
        ("std::env::", "env"),
        ("rand::", "random"),
        ("rand_core::", "random"),
        ("tokio::net", "net"),
        ("mio::", "net"),
        ("socket2::", "net"),
    ];
    for (needle, cat) in checks {
        if path.contains(needle) {
            return Some(cat);
        }
    }
    None
}

/// The precise monomorphic name of a function item, falling back to the
/// polymorphic def name when `Instance::resolve` cannot monomorphize it (a
/// generic or trait call in a polymorphic body — the fallback keeps the
/// statically-known trait-method name). Shared by the call path
/// ([`CallCollector::visit_terminator`]) and the reference path
/// ([`CallCollector::reference_operand`]) so both resolve a callee identically.
fn fndef_name(def: FnDef, _args: &GenericArgs) -> String {
    // Derive the callee name from its polymorphic def path rather than resolving a
    // monomorphic `Instance`.
    //
    // `Instance::resolve(def, args)` monomorphizes the callee, which forces
    // codegen-mode normalization of its generic arguments. For some instances
    // (observed with an `impl IntoIterator<Item = …>` argument in
    // `pidgin-agent`) rustc's `normalize_erasing_regions` cannot normalize an
    // opaque projection and raises `bug!` — a fatal *diagnostic* plus a panic.
    // Catching the panic is not enough: the emitted `bug!` diagnostic poisons the
    // whole crate's compilation, so cargo never writes the crate's `.rmeta`, and
    // every dependent crate then fails to build ("extern location … does not
    // exist"), taking its own extraction down with it. The only reliable fix is
    // to never trigger that normalization.
    //
    // The def-path name is also the correct granularity for a *dependency* graph:
    //   * a local generic fn is only ever represented by its polymorphic def name
    //     (it comes back from `all_local_items` un-monomorphized and is keyed by
    //     `item.name()`), so a callee keyed by `def.name()` matches its definition
    //     node, where a `foo::<Concrete>` mono name would miss it;
    //   * a local concrete fn has `inst.name() == def.name()`, so nothing changes;
    //   * an external mono call (`Vec::<T>::new`) is a foreign leaf either way, and
    //     effect classification strips generics before matching, so it is
    //     unaffected.
    // This mirrors the pre-existing `Err(_) => def.name()` fallback, now taken
    // unconditionally so a single unnormalizable instance can never abort — nor
    // silently corrupt — an extraction.
    def.name()
}

/// A span's file and 1-based start line.
fn span_file_line(span: rustc_public::ty::Span) -> (String, u32) {
    let file = span.get_filename();
    let line = span.get_lines().start_line as u32;
    (file, line)
}

/// A placeholder callee id for an indirect (fn-pointer / dyn) call site. It
/// matches no definition or root, so nothing propagates through it — which
/// faithfully surfaces the soundness gap rather than faking a resolution.
const INDIRECT_CALLEE: &str = "<indirect>";

struct CallCollector<'a> {
    caller: String,
    locals: &'a [rustc_public::mir::LocalDecl],
    edges: Vec<Edge>,
    roots: Vec<EffectRoot>,
    seen_root: &'a mut HashSet<String>,
    /// `(callee)` of reference edges already emitted from this caller, so a fn
    /// value used repeatedly (a loop body, the same callback twice) yields one
    /// reference edge rather than a duplicate per occurrence.
    seen_ref: HashSet<String>,
}

impl CallCollector<'_> {
    /// Seed an effect root for `callee` when its path is a known effectful
    /// operation — shared by the call and reference paths so a referenced
    /// effectful item (`register(std::fs::read)`) taints its user exactly as a
    /// direct call would.
    fn seed_root(&mut self, callee: &str) {
        if let Some(cat) = effect_category(callee) {
            if self.seen_root.insert(callee.to_string()) {
                self.roots.push(EffectRoot {
                    symbol: callee.to_string(),
                    effect: cat.to_string(),
                });
            }
        }
    }

    /// Record a direct call to a **named** callee, seeding an effect root when the
    /// callee's path is a known effectful operation. The callee is a statically
    /// named function item, so the edge is `resolution: call` even when
    /// `Instance::resolve` could not *monomorphize* it — a generic/trait call in a
    /// polymorphic body (a closure or `static`/`const` initializer walked from its
    /// own generic MIR) keeps the trait-method name (`std::convert::AsRef::as_ref`),
    /// which the engine's name-based resolution order classifies soundly (a std
    /// name clears to trusted-pure; an unresolvable user name still degrades to
    /// `Unknown`). `resolution: unresolved` is reserved for a genuinely *anonymous*
    /// target — a fn-pointer / `dyn` call — emitted by [`Self::push_indirect`]; a
    /// named target is never that gap.
    fn push_direct(&mut self, callee: String, file: String, line: u32) {
        self.seed_root(&callee);
        self.edges.push(Edge {
            caller: self.caller.clone(),
            callee,
            kind: "call".to_string(),
            resolution: "call".to_string(),
            evidence_file: file,
            evidence_line: line,
        });
    }

    /// Record an indirect (fn-pointer / dyn) call site as an unresolved edge to
    /// the placeholder callee — the soundness gap, surfaced not faked.
    fn push_indirect(&mut self, file: String, line: u32) {
        self.edges.push(Edge {
            caller: self.caller.clone(),
            callee: INDIRECT_CALLEE.to_string(),
            kind: "call".to_string(),
            resolution: "unresolved".to_string(),
            evidence_file: file,
            evidence_line: line,
        });
    }

    /// Record a `reference` edge: `caller` uses `callee` as a *value* (a
    /// callback, an assignment/return operand, a reified fn-pointer, a closure)
    /// rather than calling it. Seeds an effect root the same way a call does, so
    /// the referenced item's effect is attributed to the function that hands it
    /// off. Deduped per caller.
    fn push_reference(&mut self, callee: String, file: String, line: u32) {
        if !self.seen_ref.insert(callee.clone()) {
            return;
        }
        self.seed_root(&callee);
        self.edges.push(Edge {
            caller: self.caller.clone(),
            callee,
            kind: "reference".to_string(),
            resolution: "reference".to_string(),
            evidence_file: file,
            evidence_line: line,
        });
    }

    /// If `op` is a constant whose type is a function item (`FnDef`) or a
    /// closure, emit a reference edge to it — the caller is using it as a value,
    /// not calling it. A `FnDef` resolves to a monomorphic `Instance` for the
    /// precise callee name (falling back to the polymorphic def name), the same
    /// resolution the call path uses; a non-capturing closure is a ZST constant
    /// of closure type (`register(|| ...)`, `run_closure(closure)`), keyed by its
    /// def name — the same id its own body is walked under. Non-fn operands
    /// (locals, non-fn constants) are not references.
    fn reference_operand(&mut self, op: &Operand, file: &str, line: u32) {
        if let Operand::Constant(_) = op {
            match op.ty(self.locals).map(|t| t.kind()) {
                Ok(TyKind::RigidTy(RigidTy::FnDef(def, args))) => {
                    self.push_reference(fndef_name(def, &args), file.to_string(), line);
                }
                Ok(TyKind::RigidTy(RigidTy::Closure(def, _))) => {
                    self.push_reference(def.name(), file.to_string(), line);
                }
                _ => {}
            }
        }
    }
}

impl MirVisitor for CallCollector<'_> {
    fn visit_terminator(
        &mut self,
        term: &rustc_public::mir::Terminator,
        loc: rustc_public::mir::visit::Location,
    ) {
        if let TerminatorKind::Call { func, args, .. } = &term.kind {
            let (file, line) = span_file_line(loc.span());
            match func.ty(self.locals).map(|fty| fty.kind()) {
                // Direct, statically named callee. Resolve to a monomorphic
                // Instance for the precise name; if it cannot be monomorphized
                // (a generic/trait call in a polymorphic body), keep the
                // trait-method name — it is still a named `call`, classified by
                // name downstream, not an anonymous unresolved target.
                Ok(TyKind::RigidTy(RigidTy::FnDef(def, args))) => {
                    self.push_direct(fndef_name(def, &args), file.clone(), line);
                }
                // A fn-pointer / dyn value, or an operand whose type is
                // unavailable: unresolved either way.
                _ => self.push_indirect(file.clone(), line),
            }
            // Any *argument* that is itself a function item is a value being
            // handed off — `register(foo)` — not a call to it. Emit a reference
            // so `foo`'s effect is attributed to this caller. (A fn-pointer
            // reified in a prior statement, `_2 = foo as fn()`, is caught by
            // `visit_operand` instead; a bare fn-item argument is caught here.)
            for arg in args {
                self.reference_operand(arg, &file, line);
            }
            // Deliberately do NOT descend into the Call via `super_terminator`:
            // its `func`/`args` operands would re-enter `visit_operand`, and the
            // callee `func` would be mis-emitted as a reference. Statements are
            // visited independently by `super_basic_block`, so nothing is lost.
            return;
        }
        self.super_terminator(term, loc);
    }

    fn visit_operand(&mut self, op: &Operand, loc: rustc_public::mir::visit::Location) {
        // Reached for operands inside statements (an assignment RHS `_3 = foo`,
        // a `ReifyFnPointer` cast operand `foo as fn()`, an aggregate element,
        // the `_0 = foo` that lowers `return foo`) and non-Call terminators —
        // but NOT a Call's own func/args (handled in `visit_terminator`). A
        // function item in any of these positions is a value reference.
        let (file, line) = span_file_line(loc.span());
        self.reference_operand(op, &file, line);
        self.super_operand(op, loc);
    }

    fn visit_rvalue(&mut self, rvalue: &Rvalue, loc: rustc_public::mir::visit::Location) {
        // A closure (or coroutine-closure) construction is a value: the closure
        // is its own `Instance` with its own body, walked separately and keyed by
        // its def name, so a reference edge to it transitively carries whatever
        // effects the closure body performs — the higher-order case where the
        // closure is handed to another function to invoke.
        if let Rvalue::Aggregate(kind, _) = rvalue {
            let closure = match kind {
                AggregateKind::Closure(def, _) => Some(def.name()),
                AggregateKind::CoroutineClosure(def, _) => Some(def.name()),
                _ => None,
            };
            if let Some(name) = closure {
                let (file, line) = span_file_line(loc.span());
                self.push_reference(name, file, line);
            }
        }
        self.super_rvalue(rvalue, loc);
    }
}

/// The file and 1-based start and end lines of an item's span.
fn item_location(span: rustc_public::ty::Span) -> (String, u32, u32) {
    let (file, start) = span_file_line(span);
    let end = span.get_lines().end_line as u32;
    (file, start, end)
}

/// A definition keyed by `id`, in hinzu's schema. `display` mirrors `id`, which
/// is also what the edges use, so a summary attaches by id.
fn make_def(id: String, file: String, line_start: u32, line_end: u32) -> Def {
    Def {
        id: id.clone(),
        display: id,
        language: "rust".to_string(),
        file,
        line_start,
        line_end,
    }
}

/// Walk one body, attributing its call/reference edges and effect roots to
/// `caller`, and fold the result into `facts`.
fn walk_body(
    caller: &str,
    body: &rustc_public::mir::Body,
    facts: &mut Facts,
    seen_root: &mut HashSet<String>,
) {
    let mut c = CallCollector {
        caller: caller.to_string(),
        locals: body.locals(),
        edges: Vec::new(),
        roots: Vec::new(),
        seen_root,
        seen_ref: HashSet::new(),
    };
    c.visit_body(body);
    facts.edges.append(&mut c.edges);
    facts.effect_roots.append(&mut c.roots);
}

/// Collect the local ADTs (struct/enum/union) named in a type, peeling through
/// references, raw pointers, slices, arrays, patterns, tuples, and generic
/// arguments so `&Widget`, `&[Widget]`, `Vec<Widget>`, and `Option<Widget>` all
/// reach `Widget`. Only ADTs whose def is in the local crate are collected — a
/// foreign type (a dependency's type, or a std type like `String`) is an
/// assumed-available boundary, not a port target. Recursion is bounded by the
/// (finite, monomorphized) type structure and a depth guard.
fn collect_local_adts(ty: Ty, depth: u32, out: &mut Vec<AdtDef>) {
    if depth > 16 {
        return;
    }
    let TyKind::RigidTy(rigid) = ty.kind() else {
        return;
    };
    match rigid {
        RigidTy::Adt(adt, args) => {
            if adt.krate().is_local {
                out.push(adt);
            }
            // Peel generic arguments (Vec<Widget>, Option<Widget>, …).
            for arg in args.0 {
                if let GenericArgKind::Type(inner) = arg {
                    collect_local_adts(inner, depth + 1, out);
                }
            }
        }
        RigidTy::Ref(_, inner, _) | RigidTy::RawPtr(inner, _) => {
            collect_local_adts(inner, depth + 1, out)
        }
        RigidTy::Slice(inner) | RigidTy::Array(inner, _) | RigidTy::Pat(inner, _) => {
            collect_local_adts(inner, depth + 1, out)
        }
        RigidTy::Tuple(inners) => {
            for inner in inners {
                collect_local_adts(inner, depth + 1, out);
            }
        }
        _ => {}
    }
}

/// Emit signature-type dependency edges for a callable: one `type` edge from
/// `caller` to every local ADT named in its parameters or return. Registers each
/// ADT as a definition (so the edge resolves to a local port target, not an
/// external leaf) and dedups edges per callee. `sig_ty` is the callable's type
/// (`FnDef`); a non-fn type has no signature and no-ops. Type edges do not seed
/// effect roots — a signature dependency is not a call, so it must never carry a
/// runtime effect (`hinzu_core` also excludes `type` edges from propagation).
fn emit_signature_type_edges(
    caller: &str,
    caller_file: &str,
    caller_line: u32,
    sig_ty: Ty,
    facts: &mut Facts,
    def_ids: &mut HashMap<String, ()>,
) {
    let Some(sig) = sig_ty.kind().fn_sig() else {
        return;
    };
    let sig = sig.skip_binder();
    let mut adts: Vec<AdtDef> = Vec::new();
    for ty in &sig.inputs_and_output {
        collect_local_adts(*ty, 0, &mut adts);
    }
    let mut emitted: HashSet<String> = HashSet::new();
    for adt in adts {
        let name = adt.name();
        if name == caller {
            continue;
        }
        // Register the ADT as a local definition, so the type edge resolves to a
        // local node (a real port dependency), not an external leaf.
        if def_ids.insert(name.clone(), ()).is_none() {
            let (file, start, end) = item_location(adt.span());
            facts.definitions.push(make_def(name.clone(), file, start, end));
        }
        if !emitted.insert(name.clone()) {
            continue;
        }
        facts.edges.push(Edge {
            caller: caller.to_string(),
            callee: name,
            kind: "type".to_string(),
            resolution: "reference".to_string(),
            evidence_file: caller_file.to_string(),
            evidence_line: caller_line,
        });
    }
}

fn analyze() -> ControlFlow<()> {
    let mut facts = Facts::default();
    let mut seen_root: HashSet<String> = HashSet::new();
    let mut def_ids: HashMap<String, ()> = HashMap::new();

    for item in rustc_public::all_local_items() {
        let name = item.name();
        let (file, line_start, line_end) = item_location(item.span());

        // Only monomorphic fn-like items have a body with call terminators.
        let inst = match Instance::try_from(item) {
            Ok(i) => i,
            Err(_) => {
                // Not directly instantiable via `Instance::try_from`. Two cases:
                //   * a generic fn (`requires_monomorphization`): record the
                //     definition only — its calls appear via the concrete
                //     instances the compiler actually instantiated.
                //   * a concrete body `try_from` cannot key on its own — a
                //     closure (its upvars/kind are implicit) or a `static` /
                //     `const` initializer. These have a real body carrying
                //     effects (a closure that reads a file; an import-time
                //     `static X = read_config()`), so walk it directly, keyed by
                //     the item's own name — the Rust analogue of Python's
                //     module-scope import-time effects.
                // Either way its signature-type dependencies come from the item
                // type (the ADTs are concrete named types, monomorphization or
                // not) and are emitted below.
                if def_ids.insert(name.clone(), ()).is_none() {
                    facts
                        .definitions
                        .push(make_def(name.clone(), file.clone(), line_start, line_end));
                }
                // A closure reports `requires_monomorphization` like a generic
                // fn, but its body is concrete and is NOT re-walked via any
                // separate monomorphization — recognize it by its closure type
                // and walk it directly, so a reference edge into it lands on a
                // node that actually carries the closure body's effects.
                let is_closure = matches!(
                    item.ty().kind(),
                    TyKind::RigidTy(RigidTy::Closure(..))
                        | TyKind::RigidTy(RigidTy::CoroutineClosure(..))
                );
                let walkable = matches!(item.kind(), ItemKind::Static | ItemKind::Const)
                    || is_closure
                    || !item.requires_monomorphization();
                if walkable {
                    if let Some(body) = item.body() {
                        walk_body(&name, &body, &mut facts, &mut seen_root);
                    }
                }
                emit_signature_type_edges(
                    &name,
                    &file,
                    line_start,
                    item.ty(),
                    &mut facts,
                    &mut def_ids,
                );
                continue;
            }
        };
        if matches!(inst.kind, InstanceKind::Intrinsic) {
            continue;
        }
        let disp = inst.name();
        if def_ids.insert(disp.clone(), ()).is_none() {
            facts
                .definitions
                .push(make_def(disp.clone(), file.clone(), line_start, line_end));
        }

        // Signature-type dependency edges: the callable's monomorphized type
        // gives its parameter and return types; each local ADT among them is a
        // port dependency of this function.
        emit_signature_type_edges(
            &disp,
            &file,
            line_start,
            inst.ty(),
            &mut facts,
            &mut def_ids,
        );

        let Some(body) = inst.body() else { continue };
        walk_body(&disp, &body, &mut facts, &mut seen_root);
    }

    let dir = std::env::var("HINZU_FACTS_DIR").unwrap_or_else(|_| "/tmp".to_string());
    let crate_name = rustc_public::local_crate().name;
    let out = format!("{dir}/facts-{crate_name}-{}.json", std::process::id());
    let json = serde_json::to_string_pretty(&facts).unwrap();
    let mut f = std::fs::File::create(&out).unwrap();
    f.write_all(json.as_bytes()).unwrap();
    eprintln!(
        "[hinzu-rustc-driver] crate={crate_name} defs={} edges={} roots={} -> {out}",
        facts.definitions.len(),
        facts.edges.len(),
        facts.effect_roots.len(),
    );
    ControlFlow::Continue(())
}

fn main() {
    let mut args: Vec<String> = std::env::args().collect();
    // As a RUSTC_WORKSPACE_WRAPPER, cargo calls us as
    //   hinzu-rustc-driver <path-to-real-rustc> <rustc args…>
    // run_compiler wants args[0]=program, args[1..]=rustc args, so drop the
    // injected rustc path at index 1 (clippy-driver does the same).
    if args.len() > 1 && (args[1].ends_with("rustc") || args[1].ends_with("rustc.exe")) {
        args.remove(1);
    }
    let _ = rustc_public::run!(&args, analyze);
}

#[cfg(test)]
mod tests {
    use super::{effect_category, strip_generics};

    /// Generics are stripped so effect matching runs on the callee's *path*, not
    /// on its type arguments — a type argument that mentions `std::fs` must not
    /// masquerade as an fs call.
    #[test]
    fn strip_generics_removes_type_arguments() {
        assert_eq!(
            strip_generics("std::fs::read_to_string::<&str>"),
            "std::fs::read_to_string::"
        );
        assert_eq!(
            strip_generics("Option::<std::fs::FileType>::is_some_and"),
            "Option::::is_some_and"
        );
    }

    /// The same effect classifier the call and reference paths share: an fs path
    /// is `fs`, a net path is `net`, and a pure callee — including one whose type
    /// argument merely mentions `std::fs` — is `None`.
    #[test]
    fn effect_category_matches_the_callee_path_not_type_args() {
        assert_eq!(
            effect_category("std::fs::read_to_string::<&str>"),
            Some("fs")
        );
        assert_eq!(effect_category("std::net::TcpStream::connect"), Some("net"));
        assert_eq!(
            effect_category("Option::<std::fs::FileType>::is_some_and"),
            None
        );
        assert_eq!(effect_category("core::cmp::max::<usize>"), None);
    }
}
