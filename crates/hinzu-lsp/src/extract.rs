//! The generic, language-agnostic fact extractor. Nothing here knows any
//! specific language: it is parameterized entirely by a [`LanguageConfig`]. It
//! drives any LSP server implementing `textDocument/documentSymbol` +
//! `textDocument/prepareCallHierarchy` + `callHierarchy/outgoingCalls` and emits
//! hinzu's [`FactSet`] directly, in-process — no JSON round-trip through a
//! script.
//!
//! Pipeline (the Go/gopls spike, ported to Rust and generalized; it now drives
//! both Go and Python through the same code):
//!   1. spawn the server; `initialize` (advertising callHierarchy + documentSymbol,
//!      the project `rootUri`, and the config's `initializationOptions`);
//!      `initialized`; `didOpen` every matched file; wait for the workspace to
//!      settle, then a ready-probe so resolution does not race cold start.
//!   2. definitions: `documentSymbol` per file → function/method symbols (nested
//!      included), container-qualified.
//!   3. call edges: `prepareCallHierarchy` at each definition's name, then
//!      `callHierarchy/outgoingCalls` → callees with call-site ranges. A LOCAL
//!      callee is mapped back to a collected definition BY SOURCE LOCATION (call
//!      hierarchy drops the receiver, so name-matching is lossy); an EXTERNAL
//!      callee's defining-file uri → provenance → effect via the config map, with
//!      the callee's class-qualified name reconstructed from the target file's
//!      own `documentSymbol`.
//!   4. an external callee no rule classifies is a plain edge to a canonical
//!      `<package>::<qualname>` symbol with no root, so hinzu-core's
//!      Unknown-by-default classification fails closed on it — exactly like the
//!      native adapters.
//!
//! Fidelity, stated honestly: `callHierarchy/outgoingCalls` is **call-only** — it
//! reports only the calls the server resolved, missing higher-order `reference`
//! uses (a function passed as a value/callback/decorator) and module-level
//! (import-time) usage it never anchors. A second, syntactic rung closes that gap
//! for Python: [`crate::treesitter`] parses each file with tree-sitter and
//! enumerates those reference sites; [`Extractor::collect_references`] resolves
//! each through the same `textDocument/definition` → provenance → effect path and
//! emits `reference` edges (see step 5 below). The rung is SOUND-ADDITIVE — it
//! only adds edges/effects — so no violation the call pass found can vanish. What
//! remains uncovered is an ambient attribute read (`os.environ`) and a call site
//! the server could not resolve at all; Unknown-by-default over what it does
//! resolve keeps the result sound, never silently pure. Go and the other LSP-tier
//! languages reuse the same reference rung once their grammar's node/field table
//! is added — a documented follow-up. See notes/python-catalog.md.
//!
//!   5. reference edges (Python): a tree-sitter pass over each source file
//!      enumerates non-call reference sites — a name used as a value, plus
//!      module-scope call callees — attributes each to its enclosing collected
//!      function (or a synthetic per-file `<module>` definition for import-time /
//!      class-body code), resolves it via `textDocument/definition`, and emits a
//!      `reference` edge through the shared classifier, deduped against the call
//!      edges by position (a callee inside a function is left to step 3).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use hinzu_core::facts::{Definition, Edge, EdgeKind, EdgeResolution, Effect, EffectRoot, FactSet};
use serde::Deserialize;

use crate::client::{path_to_uri, uri_to_path, LspClient};
use crate::config::{LanguageConfig, Origin};

// --- LSP payload shapes we read (positions are the server's, already encoded) --

#[derive(Clone, Copy, Deserialize)]
struct Position {
    line: u32,
    character: u32,
}

#[derive(Clone, Copy, Deserialize)]
struct Range {
    start: Position,
    end: Position,
}

#[derive(Deserialize)]
struct DocSymbol {
    name: String,
    kind: i64,
    range: Range,
    #[serde(rename = "selectionRange")]
    selection_range: Range,
    #[serde(default)]
    children: Vec<DocSymbol>,
}

#[derive(Deserialize)]
struct OutgoingCall {
    to: CallTarget,
    #[serde(rename = "fromRanges", default)]
    from_ranges: Vec<Range>,
}

#[derive(Deserialize)]
struct CallTarget {
    uri: String,
    #[serde(rename = "selectionRange")]
    selection_range: Range,
}

// SymbolKind values (LSP). Callables become definitions; container kinds
// contribute a qualifier (`Class.method`).
const KIND_METHOD: i64 = 6;
const KIND_FUNCTION: i64 = 12;
fn is_callable(kind: i64) -> bool {
    kind == KIND_FUNCTION || kind == KIND_METHOD
}
fn is_container(kind: i64) -> bool {
    matches!(kind, 5 | 23 | 11 | 10) // Class, Struct, Interface, Enum
}
/// Whether a symbol contributes a qualifier segment to its descendants. Both
/// containers (`Class.method`) and enclosing callables (`outer.inner` for a
/// nested function) do — matching the AST adapter's qualname stack, which pushed
/// every class *and* function name.
fn nests(kind: i64) -> bool {
    is_container(kind) || is_callable(kind)
}

/// A collected callable's location, for mapping a call-hierarchy callee back to
/// it by source location.
struct DefSpan {
    lo: u32,
    hi: u32,
    id: String,
}

/// The generic extractor's working state for one run.
pub struct Extractor<'c> {
    cfg: &'c LanguageConfig,
    root: PathBuf,
    files: Vec<PathBuf>,
    definitions: BTreeMap<String, Definition>,
    /// def id → (uri, name-position) for prepareCallHierarchy.
    anchors: Vec<(String, String, Position)>,
    /// relpath → collected callable spans (1-based line ranges).
    def_index: BTreeMap<String, Vec<DefSpan>>,
    edges: Vec<Edge>,
    edge_keys: BTreeSet<(String, String, String)>,
    roots: BTreeMap<String, Effect>,
    /// Cached symbol index of an external target file: (lo, hi, qual, kind).
    target_syms: BTreeMap<String, Vec<(u32, u32, String, i64)>>,
    opened_targets: BTreeSet<String>,
    // metrics — call edges
    pub n_call_edges: usize,
    pub n_local: usize,
    pub n_effect: usize,
    pub n_stdlib_pure: usize,
    pub n_unknown: usize,
    pub n_prepare_ok: usize,
    pub n_prepare_empty: usize,
    // metrics — reference edges (the tree-sitter syntactic rung)
    pub n_ref_sites: usize,
    pub n_ref_edges: usize,
    pub n_ref_local: usize,
    pub n_ref_effect: usize,
    pub n_ref_stdlib_pure: usize,
    pub n_ref_unknown: usize,
    pub n_module_defs: usize,
}

impl<'c> Extractor<'c> {
    pub fn new(cfg: &'c LanguageConfig, root: &Path) -> Self {
        Extractor {
            cfg,
            root: root.to_path_buf(),
            files: Vec::new(),
            definitions: BTreeMap::new(),
            anchors: Vec::new(),
            def_index: BTreeMap::new(),
            edges: Vec::new(),
            edge_keys: BTreeSet::new(),
            roots: BTreeMap::new(),
            target_syms: BTreeMap::new(),
            opened_targets: BTreeSet::new(),
            n_call_edges: 0,
            n_local: 0,
            n_effect: 0,
            n_stdlib_pure: 0,
            n_unknown: 0,
            n_prepare_ok: 0,
            n_prepare_empty: 0,
            n_ref_sites: 0,
            n_ref_edges: 0,
            n_ref_local: 0,
            n_ref_effect: 0,
            n_ref_stdlib_pure: 0,
            n_ref_unknown: 0,
            n_module_defs: 0,
        }
    }

    /// Run the whole pipeline and return the extracted facts.
    pub fn run(mut self) -> Result<FactSet> {
        self.discover();
        if self.files.is_empty() {
            anyhow::bail!(
                "no source files matched {:?} under {}",
                self.cfg.file_globs,
                self.root.display()
            );
        }

        let server_cmd = crate::resolved_server_cmd(self.cfg);
        let mut lsp = LspClient::spawn(&server_cmd, &self.root)
            .with_context(|| format!("starting the {} language server", self.cfg.language_id))?;
        let result = self.drive(&mut lsp);
        if std::env::var("HINZU_LSP_DEBUG").is_ok() {
            let lines = lsp.stderr_lines();
            let n = lines.len().min(20);
            if n > 0 {
                eprintln!("hinzu-lsp[debug]: server stderr (last {n}):");
                for l in &lines[lines.len() - n..] {
                    eprintln!("  {l}");
                }
            }
        }
        lsp.shutdown();
        result?;

        eprintln!("hinzu-lsp: {}", self.summary());
        Ok(self.into_facts())
    }

    /// The lifecycle over an initialized-then-torn-down server.
    fn drive(&mut self, lsp: &mut LspClient) -> Result<()> {
        self.initialize(lsp)?;
        for f in &self.files.clone() {
            let text = std::fs::read_to_string(f).unwrap_or_default();
            lsp.notify(
                "textDocument/didOpen",
                serde_json::json!({"textDocument": {
                    "uri": path_to_uri(f), "languageId": self.cfg.language_id,
                    "version": 1, "text": text,
                }}),
            )?;
        }
        let settled = lsp.wait_until_settled(Duration::from_secs(45));
        if std::env::var("HINZU_LSP_DEBUG").is_ok() {
            eprintln!("hinzu-lsp[debug]: settled={settled}");
        }
        self.await_ready(lsp);

        self.collect_definitions(lsp)?;
        self.collect_calls(lsp)?;
        self.collect_references(lsp)?;
        Ok(())
    }

    fn initialize(&self, lsp: &mut LspClient) -> Result<()> {
        crate::client::initialize(
            lsp,
            &self.root,
            &self.cfg.init_options,
            serde_json::json!({
                "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
                "callHierarchy": {"dynamicRegistration": false},
                "definition": {"linkSupport": true},
            }),
        )
    }

    /// Open a throwaway in-memory probe doc and poll `textDocument/definition`
    /// until the server resolves a known symbol (proof its workspace is warm),
    /// or the probe times out. Ported from the Python adapter's `_await_ready`;
    /// skipped when the config declares no probe.
    fn await_ready(&self, lsp: &mut LspClient) {
        let Some(probe) = &self.cfg.ready_probe else {
            return;
        };
        let path = self.root.join(&probe.filename);
        let uri = path_to_uri(&path);
        let _ = lsp.notify(
            "textDocument/didOpen",
            serde_json::json!({"textDocument": {
                "uri": uri, "languageId": self.cfg.language_id, "version": 1, "text": probe.text,
            }}),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(25);
        while std::time::Instant::now() < deadline {
            let resp = lsp.request(
                "textDocument/definition",
                serde_json::json!({"textDocument": {"uri": uri},
                    "position": {"line": probe.line, "character": probe.character}}),
                Duration::from_secs(15),
            );
            if let Ok(v) = resp {
                if let Some(turi) = first_target_uri(&v) {
                    if probe.expect.iter().any(|m| turi.contains(m)) {
                        break;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let _ = lsp.notify(
            "textDocument/didClose",
            serde_json::json!({"textDocument": {"uri": uri}}),
        );
    }

    // ---- file discovery ------------------------------------------------------
    fn discover(&mut self) {
        let mut seen = BTreeSet::new();
        for g in &self.cfg.file_globs {
            let pat = self.root.join(g);
            let Some(pat) = pat.to_str() else { continue };
            let Ok(paths) = glob::glob(pat) else { continue };
            for p in paths.flatten() {
                if !p.is_file() {
                    continue;
                }
                let Ok(rel) = p.strip_prefix(&self.root) else {
                    continue;
                };
                if rel.components().any(|c| {
                    self.cfg
                        .exclude_dirs
                        .iter()
                        .any(|d| c.as_os_str() == d.as_str())
                }) {
                    continue;
                }
                let name = p
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if self.cfg.exclude_suffixes.iter().any(|s| name.ends_with(s)) {
                    continue;
                }
                if seen.insert(p.clone()) {
                    self.files.push(p);
                }
            }
        }
        self.files.sort();
    }

    fn rel(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    }

    // ---- definitions ---------------------------------------------------------
    fn collect_definitions(&mut self, lsp: &mut LspClient) -> Result<()> {
        for f in &self.files.clone() {
            let uri = path_to_uri(f);
            let relpath = self.rel(f);
            let syms = self.document_symbols(lsp, &uri);
            self.walk_defs(&syms, &relpath, &uri, None);
        }
        Ok(())
    }

    fn walk_defs(&mut self, syms: &[DocSymbol], relpath: &str, uri: &str, container: Option<&str>) {
        for s in syms {
            let qual = match container {
                Some(c) => format!("{c}.{}", s.name),
                None => s.name.clone(),
            };
            if is_callable(s.kind) {
                let id = format!("{relpath}#{qual}");
                let lo = s.range.start.line + 1;
                let hi = s.range.end.line + 1;
                self.definitions.insert(
                    id.clone(),
                    Definition {
                        id: id.clone(),
                        display: qual.clone(),
                        language: self.cfg.language(),
                        file: relpath.to_string(),
                        line_start: lo,
                        line_end: hi,
                        is_component: false,
                    },
                );
                self.anchors
                    .push((id.clone(), uri.to_string(), s.selection_range.start));
                self.def_index
                    .entry(relpath.to_string())
                    .or_default()
                    .push(DefSpan { lo, hi, id });
            }
            let next = if nests(s.kind) {
                Some(qual.as_str())
            } else {
                container
            };
            // Reborrow-safe recursion: clone the qualifier so the mutable walk can
            // continue.
            let next_owned = next.map(|c| c.to_string());
            self.walk_defs(&s.children, relpath, uri, next_owned.as_deref());
        }
    }

    // ---- call edges ----------------------------------------------------------
    fn collect_calls(&mut self, lsp: &mut LspClient) -> Result<()> {
        let anchors = std::mem::take(&mut self.anchors);
        for (cid, uri, pos) in &anchors {
            let prep = self.prepare_call_hierarchy(lsp, uri, *pos);
            let Some(item) = prep else {
                self.n_prepare_empty += 1;
                continue;
            };
            self.n_prepare_ok += 1;
            let og = lsp
                .request(
                    "callHierarchy/outgoingCalls",
                    serde_json::json!({"item": item}),
                    Duration::from_secs(30),
                )
                .ok()
                .and_then(|v| serde_json::from_value::<Vec<OutgoingCall>>(v).ok())
                .unwrap_or_default();
            for call in og {
                self.emit_call(lsp, cid, &call);
            }
        }
        Ok(())
    }

    fn emit_call(&mut self, lsp: &mut LspClient, caller_id: &str, call: &OutgoingCall) {
        let callee_def_line = call.to.selection_range.start.line + 1;
        let caller_file = caller_id.split('#').next().unwrap_or("").to_string();
        let lines: Vec<u32> = if call.from_ranges.is_empty() {
            vec![call.to.selection_range.start.line + 1]
        } else {
            call.from_ranges.iter().map(|r| r.start.line + 1).collect()
        };
        for line in &lines {
            self.classify_and_emit(
                lsp,
                caller_id,
                &call.to.uri,
                callee_def_line,
                EdgeKind::Call,
                &caller_file,
                *line,
            );
        }
    }

    /// Resolve a used symbol (a call target, or a reference site's
    /// `textDocument/definition` target) to a hinzu edge and emit it. Shared by
    /// the call resolver and the tree-sitter reference resolver so both treat a
    /// resolved-vs-unknown target identically: a LOCAL owned callee threads to the
    /// collected definition (or its `__init__`); an EXTERNAL callee is classified
    /// by provenance into an effect root, a trusted-pure stdlib baseline (no
    /// edge), or a fail-closed `Unknown`. `kind` selects call vs reference (edge
    /// kind, resolution, and which metric counters advance). Returns whether an
    /// edge was actually added (false for a trusted-pure or dead-end target), so a
    /// reference caller can tell whether its `<module>` node earned a definition.
    #[allow(clippy::too_many_arguments)]
    fn classify_and_emit(
        &mut self,
        lsp: &mut LspClient,
        caller_id: &str,
        target_uri: &str,
        target_def_line: u32,
        kind: EdgeKind,
        evidence_file: &str,
        evidence_line: u32,
    ) -> bool {
        let is_call = matches!(kind, EdgeKind::Call);
        let resolution = EdgeResolution::for_kind(kind);
        let callee_path = uri_to_path(target_uri);
        if is_call {
            self.n_call_edges += 1;
        } else {
            self.n_ref_edges += 1;
        }

        // Local (owned) target?
        if let Some(rel) = self.owned_rel(&callee_path) {
            match self.def_at(&rel, target_def_line) {
                Some(id) => {
                    self.bump_local(is_call);
                    self.add_edge(
                        caller_id,
                        &id,
                        kind,
                        resolution,
                        evidence_file,
                        evidence_line,
                    );
                    return true;
                }
                None => {
                    // A local target that is not a collected callable: most often
                    // construction of a local class. Thread to its `__init__` so the
                    // constructor's own effects propagate; a class with no tracked
                    // `__init__` (e.g. a dataclass) is pure, so no edge.
                    if let Some(init) =
                        self.local_class_init(lsp, target_uri, &rel, target_def_line)
                    {
                        self.bump_local(is_call);
                        self.add_edge(
                            caller_id,
                            &init,
                            kind,
                            resolution,
                            evidence_file,
                            evidence_line,
                        );
                        return true;
                    }
                    return false;
                }
            }
        }

        // External target: reconstruct its class-qualified name, classify by
        // provenance, and map to an effect.
        let (pkg, origin) = match self.cfg.package_of(&callee_path) {
            Some((p, o)) => (Some(p), Some(o)),
            None => (None, None),
        };
        let qual = self
            .qualname_at(lsp, target_uri, target_def_line)
            .unwrap_or_else(|| {
                Path::new(&callee_path)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
        let base = pkg.clone().unwrap_or_else(|| {
            Path::new(&callee_path)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "external".into())
        });
        let symbol = format!("{base}::{qual}");
        let effect = pkg.as_deref().and_then(|p| self.cfg.effect_of(&symbol, p));

        match (effect, origin) {
            (Some(e), _) => {
                // An effect root: seed it directly by declaration provenance.
                self.add_edge(
                    caller_id,
                    &symbol,
                    kind,
                    resolution,
                    evidence_file,
                    evidence_line,
                );
                self.roots.entry(symbol.clone()).or_insert(e);
                self.bump_effect(is_call);
                true
            }
            (None, Some(Origin::Stdlib)) => {
                // A pure standard-library target: trusted-pure baseline, so no
                // edge — it must never become an Unknown (hinzu-core's pure
                // baseline is Rust's std::, which would not clear it).
                self.bump_stdlib_pure(is_call);
                false
            }
            (None, _) => {
                // A third-party package (or an unmapped foreign file) we cannot
                // see through: an edge with no root, so hinzu-core marks it Unknown
                // and fails closed until a `[trust]` line vouches for it.
                self.add_edge(
                    caller_id,
                    &symbol,
                    kind,
                    resolution,
                    evidence_file,
                    evidence_line,
                );
                self.bump_unknown(is_call);
                true
            }
        }
    }

    fn bump_local(&mut self, is_call: bool) {
        if is_call {
            self.n_local += 1;
        } else {
            self.n_ref_local += 1;
        }
    }
    fn bump_effect(&mut self, is_call: bool) {
        if is_call {
            self.n_effect += 1;
        } else {
            self.n_ref_effect += 1;
        }
    }
    fn bump_stdlib_pure(&mut self, is_call: bool) {
        if is_call {
            self.n_stdlib_pure += 1;
        } else {
            self.n_ref_stdlib_pure += 1;
        }
    }
    fn bump_unknown(&mut self, is_call: bool) {
        if is_call {
            self.n_unknown += 1;
        } else {
            self.n_ref_unknown += 1;
        }
    }

    // ---- reference edges (the tree-sitter syntactic rung) --------------------

    /// The syntactic reference pass: for each owned source file, parse it with
    /// tree-sitter and enumerate its non-call reference sites (a function/symbol
    /// used as a value, plus module-scope call callees call hierarchy never
    /// anchored), then resolve each through the SAME `textDocument/definition` →
    /// provenance → effect path the call resolver uses and emit a `reference`
    /// edge. It is SOUND-ADDITIVE: it only adds effects/edges, so no real
    /// violation the call-only pass found can vanish; what it adds is the
    /// higher-order and module-level (import-time) effects call hierarchy missed.
    ///
    /// Python-only for now — the enumeration in [`crate::treesitter`] is Python's
    /// grammar. Go and the other LSP-tier languages are the same shape and a
    /// documented follow-up.
    fn collect_references(&mut self, lsp: &mut LspClient) -> Result<()> {
        if self.cfg.language_id != "python" {
            return Ok(());
        }
        for f in &self.files.clone() {
            let uri = path_to_uri(f);
            let relpath = self.rel(f);
            let Ok(source) = std::fs::read_to_string(f) else {
                continue;
            };
            let sites = crate::treesitter::python_reference_sites(&source);
            let module_id = format!("<module>@{relpath}");
            let mut module_used = false;
            for site in sites {
                self.n_ref_sites += 1;
                // Attribute the site to its nearest enclosing collected function
                // by source position (the reference's "caller"); a site with no
                // enclosing function is import-time / class-body code, attributed
                // to the file's synthetic `<module>` node.
                let enclosing = self.def_at(&relpath, site.site_line);
                // A call callee INSIDE a function is already a call-hierarchy
                // `call` edge — skip it here (the dedupe, by position). At module
                // scope there is no such edge, so it is emitted.
                if site.is_call_callee && enclosing.is_some() {
                    continue;
                }
                let caller_id = enclosing.unwrap_or_else(|| module_id.clone());
                let is_module = caller_id == module_id;
                let Some((turi, def_line)) =
                    self.resolve_definition(lsp, &uri, site.query_line, site.query_char)
                else {
                    // Unresolved: the call path likewise never enumerates a target
                    // it cannot resolve, so this adds nothing rather than guessing.
                    continue;
                };
                let added = self.classify_and_emit(
                    lsp,
                    &caller_id,
                    &turi,
                    def_line,
                    EdgeKind::Reference,
                    &relpath,
                    site.site_line,
                );
                if added && is_module {
                    module_used = true;
                }
            }
            // Emit the synthetic `<module>` definition only when an import-time
            // effect/edge actually attached to it, so import-time effects become
            // visible and policeable without spawning empty nodes everywhere.
            if module_used {
                let line_end = source.lines().count().max(1) as u32;
                self.definitions.insert(
                    module_id.clone(),
                    Definition {
                        id: module_id,
                        display: "<module>".to_string(),
                        language: self.cfg.language(),
                        file: relpath.clone(),
                        line_start: 1,
                        line_end,
                        is_component: false,
                    },
                );
                self.n_module_defs += 1;
            }
        }
        Ok(())
    }

    /// Resolve a reference site's `textDocument/definition`, returning the target
    /// `(uri, 1-based def line)` — the same pair `emit_call` feeds
    /// [`Self::classify_and_emit`] from a call-hierarchy callee. Handles the three
    /// LSP shapes (`Location`, `Location[]`, `LocationLink[]`).
    fn resolve_definition(
        &mut self,
        lsp: &mut LspClient,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Option<(String, u32)> {
        let resp = lsp
            .request(
                "textDocument/definition",
                serde_json::json!({"textDocument": {"uri": uri},
                    "position": {"line": line, "character": character}}),
                Duration::from_secs(15),
            )
            .ok()?;
        parse_definition_target(&resp)
    }

    /// If a local, non-callable call target is a class, the id of its tracked
    /// `__init__`, else `None` (pure construction).
    fn local_class_init(
        &mut self,
        lsp: &mut LspClient,
        uri: &str,
        rel: &str,
        def_line: u32,
    ) -> Option<String> {
        // Reconstruct the qualname at the class def line from the OWNED file's
        // own symbols, then look for `<rel>#<Class>.__init__`.
        let qual = self.qualname_at(lsp, uri, def_line)?;
        let init = format!("{rel}#{qual}.__init__");
        self.definitions.contains_key(&init).then_some(init)
    }

    // ---- resolution helpers --------------------------------------------------

    /// The collected callable whose 1-based line range encloses `line` in
    /// `relpath` — smallest enclosing wins (a nested function over its parent).
    fn def_at(&self, relpath: &str, line: u32) -> Option<String> {
        self.def_index.get(relpath).and_then(|spans| {
            spans
                .iter()
                .filter(|s| s.lo <= line && line <= s.hi)
                .min_by_key(|s| s.hi - s.lo)
                .map(|s| s.id.clone())
        })
    }

    /// The relpath of a callee path owned by the project (under the root, not in
    /// an excluded dir), or `None` for an external file.
    fn owned_rel(&self, path: &str) -> Option<String> {
        let p = Path::new(path);
        let rel = p.strip_prefix(&self.root).ok()?;
        if rel.components().any(|c| {
            self.cfg
                .exclude_dirs
                .iter()
                .any(|d| c.as_os_str() == d.as_str())
        }) {
            return None;
        }
        Some(rel.to_string_lossy().replace('\\', "/"))
    }

    /// The class-qualified qualname of the definition at `def_line` (1-based) in
    /// the target file `uri`, reconstructed from that file's own
    /// `documentSymbol`. This is how an external method call recovers its
    /// receiver (`Path.is_file`) that call hierarchy dropped. `None` when the
    /// target file has no enclosing callable symbol at that line.
    fn qualname_at(&mut self, lsp: &mut LspClient, uri: &str, def_line: u32) -> Option<String> {
        if !self.target_syms.contains_key(uri) {
            let index = self.build_target_index(lsp, uri);
            self.target_syms.insert(uri.to_string(), index);
        }
        let table = self.target_syms.get(uri)?;
        table
            .iter()
            .filter(|(lo, hi, _, k)| {
                *lo <= def_line && def_line <= *hi && (is_callable(*k) || is_container(*k))
            })
            .min_by_key(|(lo, hi, _, _)| hi - lo)
            .map(|(_, _, q, _)| q.clone())
    }

    /// Build (and cache) the flattened symbol index of an external target file:
    /// `(lo, hi, qual, kind)` per symbol, 1-based lines. The file is opened
    /// (read from disk — vendored typeshed and installed packages are readable)
    /// so the server answers `documentSymbol` for it.
    fn build_target_index(
        &mut self,
        lsp: &mut LspClient,
        uri: &str,
    ) -> Vec<(u32, u32, String, i64)> {
        if self.opened_targets.insert(uri.to_string()) {
            let path = uri_to_path(uri);
            if let Ok(text) = std::fs::read_to_string(&path) {
                let _ = lsp.notify(
                    "textDocument/didOpen",
                    serde_json::json!({"textDocument": {
                        "uri": uri, "languageId": self.cfg.language_id, "version": 1, "text": text,
                    }}),
                );
            }
        }
        let syms = self.document_symbols(lsp, uri);
        let mut out = Vec::new();
        flatten(&syms, None, &mut out);
        out
    }

    /// `documentSymbol` for one uri, retried a few times because a cold server
    /// can answer `null` before it has indexed the document.
    fn document_symbols(&self, lsp: &mut LspClient, uri: &str) -> Vec<DocSymbol> {
        for attempt in 0..4 {
            let resp = lsp.request(
                "textDocument/documentSymbol",
                serde_json::json!({"textDocument": {"uri": uri}}),
                Duration::from_secs(20),
            );
            if let Ok(v) = resp {
                if std::env::var("HINZU_LSP_DEBUG").is_ok() && attempt == 0 {
                    eprintln!(
                        "hinzu-lsp[debug]: documentSymbol {uri} -> {}",
                        serde_json::to_string(&v)
                            .unwrap_or_default()
                            .chars()
                            .take(300)
                            .collect::<String>()
                    );
                }
                if let Ok(syms) = serde_json::from_value::<Vec<DocSymbol>>(v.clone()) {
                    if !syms.is_empty() {
                        return syms;
                    }
                }
                if (v.is_null() || v.as_array().map(|a| a.is_empty()).unwrap_or(false))
                    && attempt < 3
                {
                    std::thread::sleep(Duration::from_millis(400));
                    continue;
                }
            }
            break;
        }
        Vec::new()
    }

    /// `prepareCallHierarchy` at a name position, retried on a cold empty result.
    /// Returns the raw item to echo back to `outgoingCalls`.
    fn prepare_call_hierarchy(
        &self,
        lsp: &mut LspClient,
        uri: &str,
        pos: Position,
    ) -> Option<serde_json::Value> {
        for attempt in 0..3 {
            let resp = lsp
                .request(
                    "textDocument/prepareCallHierarchy",
                    serde_json::json!({"textDocument": {"uri": uri},
                        "position": {"line": pos.line, "character": pos.character}}),
                    Duration::from_secs(20),
                )
                .ok()?;
            if let Some(arr) = resp.as_array() {
                if let Some(first) = arr.first() {
                    return Some(first.clone());
                }
            }
            if attempt < 2 {
                std::thread::sleep(Duration::from_millis(400));
            }
        }
        None
    }

    fn add_edge(
        &mut self,
        caller: &str,
        callee: &str,
        kind: EdgeKind,
        resolution: EdgeResolution,
        evidence_file: &str,
        evidence_line: u32,
    ) {
        if caller.is_empty() || callee.is_empty() || caller == callee {
            return;
        }
        let key = (
            caller.to_string(),
            callee.to_string(),
            kind.as_str().to_string(),
        );
        if !self.edge_keys.insert(key) {
            return;
        }
        self.edges.push(Edge {
            caller: caller.to_string(),
            callee: callee.to_string(),
            kind,
            resolution,
            evidence_file: evidence_file.to_string(),
            evidence_line,
            seam: false,
        });
    }

    fn into_facts(self) -> FactSet {
        let mut facts = FactSet::default();
        for def in self.definitions.into_values() {
            facts.add_def(def);
        }
        facts.edges = self.edges;
        let mut roots: Vec<EffectRoot> = self
            .roots
            .into_iter()
            .map(|(symbol, effect)| EffectRoot { symbol, effect })
            .collect();
        roots.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        facts.roots = roots;
        facts
    }

    /// A one-line diagnostics summary for stderr, mirroring the script adapter's.
    pub fn summary(&self) -> String {
        format!(
            "files {} | definitions {} | prepareOK {} prepareEmpty {} | call-edges {} \
             (local {}, effect {}, stdlib-pure {}, unknown {}) | ref-sites {} ref-edges {} \
             (local {}, effect {}, stdlib-pure {}, unknown {}) module-defs {} | effect roots {}",
            self.files.len(),
            self.definitions.len(),
            self.n_prepare_ok,
            self.n_prepare_empty,
            self.n_call_edges,
            self.n_local,
            self.n_effect,
            self.n_stdlib_pure,
            self.n_unknown,
            self.n_ref_sites,
            self.n_ref_edges,
            self.n_ref_local,
            self.n_ref_effect,
            self.n_ref_stdlib_pure,
            self.n_ref_unknown,
            self.n_module_defs,
            self.roots.len(),
        )
    }
}

/// Flatten a document-symbol tree into `(lo, hi, qual, kind)` rows, 1-based.
fn flatten(syms: &[DocSymbol], container: Option<&str>, out: &mut Vec<(u32, u32, String, i64)>) {
    for s in syms {
        let qual = match container {
            Some(c) => format!("{c}.{}", s.name),
            None => s.name.clone(),
        };
        out.push((
            s.range.start.line + 1,
            s.range.end.line + 1,
            qual.clone(),
            s.kind,
        ));
        let next = if nests(s.kind) {
            Some(qual.as_str())
        } else {
            container
        };
        let next_owned = next.map(|c| c.to_string());
        flatten(&s.children, next_owned.as_deref(), out);
    }
}

/// The first `targetUri`/`uri` in a `textDocument/definition` response.
fn first_target_uri(v: &serde_json::Value) -> Option<String> {
    let first = v.as_array()?.first()?;
    first
        .get("targetUri")
        .or_else(|| first.get("uri"))
        .and_then(|u| u.as_str())
        .map(str::to_string)
}

/// The first target of a `textDocument/definition` response as `(uri, 1-based
/// def line)`, across the three LSP shapes: a bare `Location` (`uri` + `range`),
/// a `Location[]`, or a `LocationLink[]` (`targetUri` + `targetSelectionRange` /
/// `targetRange`). The def line is where the resolved symbol's name sits, which
/// [`Extractor::qualname_at`] reconstructs the class-qualified name from — exactly
/// as a call-hierarchy callee's `selectionRange` feeds it.
fn parse_definition_target(v: &serde_json::Value) -> Option<(String, u32)> {
    let first = if v.is_array() {
        v.as_array()?.first()?
    } else if v.is_null() {
        return None;
    } else {
        v
    };
    let uri = first
        .get("targetUri")
        .or_else(|| first.get("uri"))
        .and_then(|u| u.as_str())?;
    let range = first
        .get("targetSelectionRange")
        .or_else(|| first.get("targetRange"))
        .or_else(|| first.get("range"))?;
    let line = range.pointer("/start/line").and_then(|l| l.as_u64())? as u32 + 1;
    Some((uri.to_string(), line))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn python_cfg() -> crate::config::LanguageConfig {
        let mut subst = BTreeMap::new();
        subst.insert("python_version".to_string(), "3.11".to_string());
        subst.insert("python_platform".to_string(), "linux".to_string());
        crate::config::LanguageConfig::from_parts(
            crate::PYTHON_CONFIG,
            &[crate::PYTHON_ANNOTATIONS, crate::PYTHON_LIB_ANNOTATIONS],
            &subst,
        )
        .expect("python config parses")
    }

    /// A reference site inside a collected function attributes to that function;
    /// one at module scope (no enclosing collected callable) attributes to the
    /// file's synthetic `<module>` node — the SQLAlchemy / class-body case.
    #[test]
    fn module_scope_reference_attributes_to_module_node() {
        let cfg = python_cfg();
        let mut ex = Extractor::new(&cfg, Path::new("/proj"));
        // A collected function `f` spanning lines 10..=20 of `m.py`.
        ex.def_index
            .entry("m.py".to_string())
            .or_default()
            .push(DefSpan {
                lo: 10,
                hi: 20,
                id: "m.py#f".to_string(),
            });
        // Inside the function → attributed to `f`.
        assert_eq!(ex.def_at("m.py", 15).as_deref(), Some("m.py#f"));
        // At module scope (line 3, outside every def span) → no enclosing
        // callable, so the emitter falls back to the `<module>` id.
        assert_eq!(ex.def_at("m.py", 3), None);
        let module_id = format!("<module>@{}", "m.py");
        assert_eq!(module_id, "<module>@m.py");
    }

    #[test]
    fn parse_definition_target_across_lsp_shapes() {
        // LocationLink[] (linkSupport): targetUri + targetSelectionRange.
        let link = serde_json::json!([{
            "targetUri": "file:///x/subprocess.pyi",
            "targetSelectionRange": {"start": {"line": 41, "character": 4},
                                     "end": {"line": 41, "character": 7}},
        }]);
        assert_eq!(
            parse_definition_target(&link),
            Some(("file:///x/subprocess.pyi".to_string(), 42))
        );
        // Bare Location: uri + range.
        let loc = serde_json::json!({
            "uri": "file:///x/mod.py",
            "range": {"start": {"line": 0, "character": 0},
                      "end": {"line": 0, "character": 3}},
        });
        assert_eq!(
            parse_definition_target(&loc),
            Some(("file:///x/mod.py".to_string(), 1))
        );
        // Location[]: array of bare Locations.
        let arr = serde_json::json!([{
            "uri": "file:///x/mod.py",
            "range": {"start": {"line": 9, "character": 0},
                      "end": {"line": 9, "character": 3}},
        }]);
        assert_eq!(
            parse_definition_target(&arr),
            Some(("file:///x/mod.py".to_string(), 10))
        );
        // An unresolved reference: null / empty → nothing to classify.
        assert_eq!(parse_definition_target(&serde_json::Value::Null), None);
        assert_eq!(parse_definition_target(&serde_json::json!([])), None);
    }
}
