# The hinzu Python path — now all-Rust, over ty's LSP

Python is analyzed by hinzu's **generic Rust LSP extractor** (`crates/hinzu-lsp`),
not a Python script. The old `analyze.py` / `lspclient.py` pair has been retired:
its AST walk, caller attribution, and ty-over-LSP resolution are now the
language-agnostic Rust extractor, driven by data — `crates/hinzu-lsp/configs/python.toml`
(the ty server command, file globs, ty `initializationOptions`, and the
typeshed / real-stdlib / site-packages provenance rules) plus the shipped
[`python.toml`](../../crates/hinzu-core/annotations/python.toml) effect map. The
only artifact left in this directory is the test fixture under `tests/`.

The one non-Rust artifact on the whole Python path is now the external
[ty](https://github.com/astral-sh/ty) binary the Rust client spawns — which hinzu
does not write.

## How it works

`hinzu check <python-project>` detects a `pyproject.toml` / `setup.py` /
`setup.cfg` and calls `hinzu_lsp::extract_python`, which:

1. spawns `ty server` and drives it over a synchronous, in-process Rust LSP client
   (`crates/hinzu-lsp/src/client.rs`, the Rust port of the old `lspclient.py`);
2. `documentSymbol` per file → definitions (class- and function-qualified);
3. `prepareCallHierarchy` + `callHierarchy/outgoingCalls` per definition → call
   edges — a real type-resolved call graph with no per-language parser;
4. classifies each external callee by its defining-file uri (ty's vendored
   typeshed, the interpreter's real stdlib, or an installed package) and maps it to
   an effect via the shared `python.toml` table, reconstructing the callee's
   class-qualified name (`pathlib::Path.is_file`) from the target file's own
   `documentSymbol`.

The facts feed the same shared pipeline as Rust and TypeScript (SQLite store, DBSP
propagation, `hinzu.toml` policy). **ty is the sole resolution backend**; if the
`ty` binary is absent the run exits nonzero with an honest message — no fallback,
no faked analysis. `HINZU_TY` overrides the ty binary path; `HINZU_PY_VERSION`
pins ty's target Python version (default `3.11`).

## Stdlib resolution on any host

ty resolves an imported stdlib symbol to whichever declaration it finds — its
vendored typeshed stub on most hosts, or the interpreter's real stdlib source
(`.../lib/python3.11/subprocess.py`) on a host that ships a full stdlib, such as a
headless CI runner. The Python config's provenance rules recognize **all** of
these shapes — vendored typeshed, installed site-packages, and a real
`.../pythonX.Y/…` stdlib path — as stdlib, so `import subprocess` →
`subprocess.run` resolves the same way everywhere. The extractor pins ty's target
`python-version` / `python-platform` in the LSP `initialize` so the typeshed is
selected deterministically. See [`notes/python-catalog.md`](../../notes/python-catalog.md).

## Honest fidelity: call-only

The generic extractor is **call-only**: `callHierarchy/outgoingCalls` reports only
the calls ty resolved, so it does not see three things — higher-order `reference`
edges (a function passed as a value/callback/decorator), an ambient attribute read
such as `os.environ`, and a call site ty could not resolve at all. All three need a
language body walk, which hinzu defers to a future language-agnostic tree-sitter
rung (also Rust). Unknown-by-default over the calls it *does* resolve
keeps the result sound — a resolved call into an unvouched third-party package
becomes an `Unknown` that fails closed under `on_unknown = fail`, never a silent
pure. The native StableMIR driver remains hinzu's Rust-precision path. See
[`notes/python-catalog.md`](../../notes/python-catalog.md) for the measured
before/after on `housekeeping`.

## The test fixture

`tests/fixture/` is a two-file functional-core project (`core.py` pure,
`effects.py` the sanctioned adapter layer) with a `hinzu.toml` policy; the live
`py_check` integration test drives the Rust extractor over it. `tests/sample-facts.json`
is the committed, backend-free fact set the stable Rust job checks — regenerated
from the Rust extractor so it stays representative.
