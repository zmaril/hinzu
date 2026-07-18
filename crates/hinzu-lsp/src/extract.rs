//! The generic, language-agnostic fact extractor. Nothing here knows any
//! specific language: it is parameterized entirely by a [`LanguageConfig`]. It
//! drives any LSP server implementing `textDocument/documentSymbol` +
//! `textDocument/prepareCallHierarchy` + `callHierarchy/outgoingCalls` and emits
//! hinzu's [`FactSet`] directly, in-process — no JSON round-trip through a
//! script.
//!
//! Pipeline (the Go/gopls spike, ported to Rust and generalized):
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
//! Fidelity, stated honestly: this is **call-only**. `callHierarchy/outgoingCalls`
//! reports only the calls the server resolved, so it does not see higher-order
//! `reference` edges — a function passed as a value/callback/decorator — an
//! ambient attribute read, nor a call site the server could not resolve at all.
//! These need a language body walk; hinzu defers them to a future
//! language-agnostic tree-sitter rung (also Rust). Unknown-by-default over the
//! calls it *does* resolve keeps the result sound, never silently pure. See
//! notes/python-catalog.md.

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
    // metrics
    pub n_call_edges: usize,
    pub n_local: usize,
    pub n_effect: usize,
    pub n_stdlib_pure: usize,
    pub n_unknown: usize,
    pub n_prepare_ok: usize,
    pub n_prepare_empty: usize,
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
        Ok(())
    }

    fn initialize(&self, lsp: &mut LspClient) -> Result<()> {
        let root_uri = path_to_uri(&self.root);
        let params = serde_json::json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "initializationOptions": self.cfg.init_options,
            "capabilities": {
                "textDocument": {
                    "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
                    "callHierarchy": {"dynamicRegistration": false},
                    "definition": {"linkSupport": true},
                },
                "window": {"workDoneProgress": true},
            },
            "workspaceFolders": [{"uri": root_uri, "name": self.root.file_name()
                .map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into())}],
        });
        lsp.request("initialize", params, Duration::from_secs(30))
            .context("LSP initialize")?;
        lsp.notify("initialized", serde_json::json!({}))?;
        Ok(())
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
                        language: hinzu_core::facts::Language::Python,
                        file: relpath.to_string(),
                        line_start: lo,
                        line_end: hi,
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
        let callee_path = uri_to_path(&call.to.uri);
        let callee_def_line = call.to.selection_range.start.line + 1;
        let caller_file = caller_id.split('#').next().unwrap_or("").to_string();

        // Local (owned) callee?
        let local_rel = self.owned_rel(&callee_path);
        let lines: Vec<u32> = if call.from_ranges.is_empty() {
            vec![call.to.selection_range.start.line + 1]
        } else {
            call.from_ranges.iter().map(|r| r.start.line + 1).collect()
        };

        if let Some(rel) = local_rel {
            let local_id = self.def_at(&rel, callee_def_line);
            for line in &lines {
                self.n_call_edges += 1;
                match &local_id {
                    Some(id) => {
                        self.n_local += 1;
                        let id = id.clone();
                        self.add_edge(
                            caller_id,
                            &id,
                            EdgeKind::Call,
                            EdgeResolution::Call,
                            &caller_file,
                            *line,
                        );
                    }
                    None => {
                        // A local target that is not a collected callable: most
                        // often construction of a local class. Thread to its
                        // `__init__` so the constructor's own effects propagate;
                        // a class with no tracked `__init__` (e.g. a dataclass)
                        // is pure, so no edge — matching the native adapter.
                        if let Some(init) =
                            self.local_class_init(lsp, &call.to.uri, &rel, callee_def_line)
                        {
                            self.n_local += 1;
                            self.add_edge(
                                caller_id,
                                &init,
                                EdgeKind::Call,
                                EdgeResolution::Call,
                                &caller_file,
                                *line,
                            );
                        }
                    }
                }
            }
            return;
        }

        // External callee: reconstruct its class-qualified name, classify by
        // provenance, and map to an effect.
        let (pkg, origin) = match self.cfg.package_of(&callee_path) {
            Some((p, o)) => (Some(p), Some(o)),
            None => (None, None),
        };
        let qual = self
            .qualname_at(lsp, &call.to.uri, callee_def_line)
            .unwrap_or_else(|| {
                // Fall back to the file stem tail if the target has no symbol.
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

        for line in &lines {
            self.n_call_edges += 1;
            match (effect, origin) {
                (Some(e), _) => {
                    // An effect root: seed it directly by declaration provenance.
                    self.add_edge(
                        caller_id,
                        &symbol,
                        EdgeKind::Call,
                        EdgeResolution::Call,
                        &caller_file,
                        *line,
                    );
                    self.roots.entry(symbol.clone()).or_insert(e);
                    self.n_effect += 1;
                }
                (None, Some(Origin::Stdlib)) => {
                    // A pure standard-library call: trusted-pure baseline, so no
                    // edge — it must never become an Unknown (hinzu-core's pure
                    // baseline is Rust's std::, which would not clear it).
                    self.n_stdlib_pure += 1;
                }
                (None, _) => {
                    // A third-party package (or an unmapped foreign file) we
                    // cannot see through: an edge with no root, so hinzu-core
                    // marks it Unknown and fails closed until a `[trust]` line
                    // vouches for it.
                    self.add_edge(
                        caller_id,
                        &symbol,
                        EdgeKind::Call,
                        EdgeResolution::Call,
                        &caller_file,
                        *line,
                    );
                    self.n_unknown += 1;
                }
            }
        }
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
             (local {}, effect {}, stdlib-pure {}, unknown {}) | effect roots {}",
            self.files.len(),
            self.definitions.len(),
            self.n_prepare_ok,
            self.n_prepare_empty,
            self.n_call_edges,
            self.n_local,
            self.n_effect,
            self.n_stdlib_pure,
            self.n_unknown,
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
