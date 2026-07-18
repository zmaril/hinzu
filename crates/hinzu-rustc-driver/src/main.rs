//! A `rustc_public` (StableMIR) custom rustc driver that extracts a
//! monomorphized call graph plus standard-library effect roots from a real
//! crate, emitting JSON facts in hinzu's `FactSet` schema (definitions, edges,
//! effect_roots) — so the output deserializes directly through
//! `hinzu_core::facts::FactSet::from_json`.
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
use rustc_public::mir::{MirVisitor, TerminatorKind};
use rustc_public::ty::{RigidTy, TyKind};
use rustc_public::CrateDef;
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

/// A "caller uses callee" edge. `kind` is always `call` here (MIR terminators
/// are call sites); `resolution` is `call` for a statically resolved direct
/// call and `unresolved` for an indirect (fn-pointer / dyn) call.
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
}

impl CallCollector<'_> {
    /// Record a resolved direct call to `callee`, seeding an effect root when
    /// the callee's path is a known effectful operation.
    fn push_direct(&mut self, callee: String, resolved: bool, file: String, line: u32) {
        if let Some(cat) = effect_category(&callee) {
            if self.seen_root.insert(callee.clone()) {
                self.roots.push(EffectRoot {
                    symbol: callee.clone(),
                    effect: cat.to_string(),
                });
            }
        }
        self.edges.push(Edge {
            caller: self.caller.clone(),
            callee,
            kind: "call".to_string(),
            resolution: if resolved { "call" } else { "unresolved" }.to_string(),
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
}

impl MirVisitor for CallCollector<'_> {
    fn visit_terminator(
        &mut self,
        term: &rustc_public::mir::Terminator,
        loc: rustc_public::mir::visit::Location,
    ) {
        if let TerminatorKind::Call { func, .. } = &term.kind {
            let (file, line) = span_file_line(loc.span());
            match func.ty(self.locals).map(|fty| fty.kind()) {
                // Direct, statically known callee. Resolve to a monomorphic
                // Instance for the precise callee name.
                Ok(TyKind::RigidTy(RigidTy::FnDef(def, args))) => {
                    let (callee, resolved) = match Instance::resolve(def, &args) {
                        Ok(inst) => (inst.name(), true),
                        Err(_) => (def.name(), false),
                    };
                    self.push_direct(callee, resolved, file, line);
                }
                // A fn-pointer / dyn value, or an operand whose type is
                // unavailable: unresolved either way.
                _ => self.push_indirect(file, line),
            }
        }
        self.super_terminator(term, loc);
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
                // Generic / not directly instantiable
                // (requires_monomorphization). Record the definition; its
                // internal calls appear via the concrete instances the compiler
                // actually instantiated.
                if def_ids.insert(name.clone(), ()).is_none() {
                    facts.definitions.push(make_def(name, file, line_start, line_end));
                }
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
                .push(make_def(disp.clone(), file, line_start, line_end));
        }

        let Some(body) = inst.body() else { continue };
        let mut c = CallCollector {
            caller: disp.clone(),
            locals: body.locals(),
            edges: Vec::new(),
            roots: Vec::new(),
            seen_root: &mut seen_root,
        };
        c.visit_body(&body);
        facts.edges.append(&mut c.edges);
        facts.effect_roots.append(&mut c.roots);
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
