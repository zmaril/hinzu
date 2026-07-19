# The Python effect catalog

hinzu has one flat, shared effect vocabulary. The category names mean the same
thing in every language: `fs` is `fs` for Rust, TypeScript, and Python, `net` is
`net`, and so on. A language does not get its own namespace and does not rename a
shared category. What a language chooses is which categories it *seeds* — the
subset of the shared vocabulary its runtime actually exposes as a certifiable
effect. A category that does not apply to a language simply does not appear for
it.

Python seeds this subset: `fs`, `net`, `process`, `env`, `clock`, `random`. The
sections below list what each one is seeded from, keyed on the callee's
declaration provenance — the module and name the resolution backend resolves a
call to, which is what the adapter reads.

## What Python seeds

- **`fs`** — the filesystem surface. The ambient builtin `open` (like
  TypeScript's `fetch`, it is not an import); the `io` module; `shutil`,
  `tempfile`, `glob`, `fileinput`, and `linecache`; the file operations of `os`
  (`os.remove`, `os.mkdir`, `os.listdir`, `os.stat`, and the rest) and the
  `os.path` predicates that stat the disk (`exists`, `isfile`, `getsize`); and
  the I/O *methods* of `pathlib.Path` (`read_text`, `write_text`, `mkdir`,
  `open`, `exists`, `glob`, and so on).
- **`net`** — `socket`, `ssl`, `urllib`, `http`, `ftplib`, `smtplib`, `poplib`,
  `imaplib`, `telnetlib`, and `xmlrpc`; and the well-known network packages
  `requests`, `httpx`, `urllib3`, and `aiohttp`.
- **`process`** — `subprocess` and `multiprocessing`; and the `os` process
  primitives `os.system`, `os.popen`, the `os.spawn*` and `os.exec*` families,
  `os.fork`, and `os.kill`.
- **`env`** — reads of the ambient process environment: `os.getenv` /
  `os.putenv` / `os.unsetenv` (calls, which the call graph sees), and the
  `os.environ` / `os.environb` mapping. Note that a bare `os.environ[...]` /
  `os.environ.get(...)` read is an *attribute access*, not a call, so the
  call-only generic extractor does not see it (see "Honest fidelity" below); the
  `os.getenv(...)` call form is seeded normally.
- **`clock`** — the `time` module, and the wall-clock reads of `datetime`
  (`datetime.now`, `datetime.utcnow`, `date.today`). The rest of `datetime` —
  date arithmetic, formatting — stays pure, because it is not a clock read.
- **`random`** — nondeterminism: the `random` and `secrets` modules.

`db` is a shared category, but Python reaches a database through a package
(`psycopg`, `mysqlclient`, `sqlalchemy`; `sqlite3` is the one standard-library
exception), so `db` is declared per project with a `[trust]` line rather than
shipped as a built-in — for example `[trust] "psycopg" = ["db"]` in `hinzu.toml`.

## Why the bare `pathlib.Path(...)` constructor is pure

`pathlib` is where the constructor-versus-method distinction matters. Building a
path — `pathlib.Path("a/b")`, `p.parent`, `p / "c"`, `p.with_suffix(".txt")` — is
pure path algebra; nothing touches the disk. Only the *methods* that perform I/O
are `fs`: `read_text`, `write_text`, `read_bytes`, `write_bytes`, `open`,
`mkdir`, `unlink`, `exists`, `glob`, `iterdir`, `stat`, and the rest. So the
adapter seeds `fs` on `pathlib.Path.mkdir` but not on `pathlib.Path` itself. A
spike that marked the whole `pathlib` module `fs` over-approximated every path
construction; the shipped adapter does not.

## Why there is no `alloc` for Python

Rust seeds an `alloc` effect: heap allocation is a real, certifiable cost a
performance-sensitive Rust region can forbid, and the standard library marks the
APIs that allocate. Python runs on a garbage-collected runtime where an
allocation is not an observable effect a functional-core policy can meaningfully
forbid — every value construction may allocate, and the collector, not the
caller, governs it. So `alloc` is absent for Python, exactly as it is for
TypeScript. It is absent, not renamed: there is no `py/alloc` and no substitute
category. Python seeds the subset above and nothing more.

## The extraction mechanism: the generic Rust LSP adapter, over ty

Python is analyzed by hinzu's **generic Rust LSP extractor** (`crates/hinzu-lsp`),
all in Rust — not a Python script. The retired `analyze.py` / `lspclient.py` pair
is replaced by a synchronous Rust LSP client plus a language-agnostic extractor
parameterized by a per-language config
(`crates/hinzu-lsp/configs/python.toml`): the ty server command, file globs, ty's
`initializationOptions`, the provenance rules, and (loaded from this same
`python.toml`) the effect map. **ty** (Astral's Rust type checker) is the sole
resolution backend; the only non-Rust artifact on the whole path is the external
`ty` binary the client spawns.

The extractor spawns `ty server`, opens every source file, settles the first check
pass (plus a ready-probe on `subprocess.run` so resolution does not race cold
start), then: `documentSymbol` per file → definitions; `prepareCallHierarchy` +
`callHierarchy/outgoingCalls` per definition → a real, type-resolved call graph.
An external callee's defining-file uri gives the provenance the effect roots key
on, and its class-qualified name (`pathlib::Path.is_file`) is reconstructed from
the target file's own `documentSymbol`. Because ty is a real type system it
resolves the un-typed `pathlib` receivers and the `self.api()` method dispatch a
name-resolver cannot.

There is **no fallback resolver**. If the `ty` binary is absent the run exits
nonzero with an honest message; it never silently degrades and never fakes a
resolution. `HINZU_TY` overrides the ty binary path; `HINZU_PY_VERSION` pins ty's
target Python version (default `3.11`).

### Fidelity: call edges plus a tree-sitter reference rung

`callHierarchy/outgoingCalls` reports only the calls ty resolved, so on its own it
is **call-only**: it does not see higher-order `reference` uses (a function passed
as a value/callback/decorator) nor any use at **module scope** (which call
hierarchy never anchors, since it is not inside a function definition). A second,
syntactic rung now closes that gap for Python. `crates/hinzu-lsp/src/treesitter.rs`
parses each source file with **tree-sitter** and enumerates its non-call reference
sites; `extract.rs` resolves each through the *same* `textDocument/definition` →
provenance → effect path the call resolver uses and emits a `reference` edge (see
"The reference rung" below). The rung is **sound-additive** — it only adds
edges/effects — so no violation the call pass found can vanish; what it adds is the
higher-order and import-time effects call hierarchy missed. What remains uncovered
is an ambient *attribute read* such as `os.environ[...]`, and a call site ty could
not resolve at all. Unknown-by-default over what it *does* resolve keeps the result
sound — a resolved use into an unvouched third-party package is an `Unknown` that
fails closed, never a silent pure.

Go and the other LSP-tier languages reuse the identical reference rung once their
grammar's node/field table is added (a tree-sitter grammar plus the same
value-position query) — a documented follow-up; the tree-sitter query is Python's
grammar for now.

### Measured on housekeeping (before → after)

Running the shared pipeline over `housekeeping` (a pure-Python fleet auditor, 82
files) with the same illustrative functional-core policy, old script vs new
extractor:

| | old `analyze.py` (AST + ty-definition) | new Rust LSP adapter (callHierarchy) |
| --- | --- | --- |
| definitions | 486 | 471 (`__init__`, which ty's documentSymbol omits — mostly ignored `tests/`) |
| forbidden-effect violations | **20 (6 fs, 14 process)** | **20 (6 fs, 14 process)** — exact, identical evidence paths |
| `fs`-effect call edges | 117 | 114 |
| effect roots | 22 (fs 11, clock 3, net 6, env 1, process 1) | 21 — identical but for `env 1` (`os::environ`, an ambient read) |
| `cannot certify` (Unknown) findings | 86 | 41 |

The **20 forbidden-effect violations match exactly**, with identical evidence
paths — the process/fs leaks flow through resolved calls, which call hierarchy
captures well (ty resolves the `self.api()` → `run` → `subprocess.run` chain). Un-typed
`pathlib` chains like `(ctx.workdir / "src-tauri").is_dir()` and
`p.parent.mkdir()` resolve to precise `pathlib::Path.is_dir` / `pathlib::Path.mkdir`
`fs` roots, so `fs` coverage stays at 114 edges. The `Unknown` count drops (86 →
41) because the old adapter emitted an `Unknown` for every one of its ~257
unresolved call sites, which the call-only driver does not enumerate; it flags an
`Unknown` only for a *resolved* call into an unvouched package. The whole run stays
around five seconds.

### Recognizing every stdlib target (the headless-runner fix)

ty resolves an imported stdlib symbol to whichever declaration it finds. Usually
that is a stub in ty's **vendored typeshed** (`.../typeshed/<hash>/stdlib/
subprocess.pyi`). But on a host whose interpreter ships a full standard library —
notably a headless GitHub Actions runner — ty resolves `import subprocess` to the
interpreter's **real stdlib source** (`.../lib/python3.11/subprocess.py`) instead.
Both are the standard library; ty picks one per host.

The extractor reconstructs a callee's provenance from that definition *target
file* via the config's provenance rules (`crates/hinzu-lsp/configs/python.toml`,
`[[provenance]]`). Those rules recognize four external shapes — ty's vendored
typeshed stdlib, its vendored third-party stubs, installed `site-packages`, and a
real CPython stdlib path — the last being the headless-runner fix, ported from the
old adapter's `module_of_target`:

- a real CPython stdlib path — a `.../pythonX.Y/<module>.pyi?` file that is not
  under `site-packages` / `dist-packages` — classifies as **stdlib**
  (`.../python3.11/subprocess.py` → `subprocess`), alongside the vendored-typeshed
  and site-packages shapes.

Without it, on the runner `subprocess.run` resolves to
`.../lib/python3.11/subprocess.py`, which — if unrecognized — would fall through to
`Unknown` while `builtins.open` (a C builtin always resolved to the vendored
`builtins.pyi`) classified fine. The rule closes that asymmetry, and it did not
reproduce on a workstation because there ty resolves the same import to its
vendored typeshed.

The extractor also **pins ty's target** `python-version` and `python-platform` in
the LSP `initialize`, via the config's `[init_options]`
(`configuration.environment`, with `diagnosticMode = workspace` so the whole
project is indexed), so the stdlib typeshed is selected deterministically rather
than by ty's environment inference. These options are passed at the top level of
`initializationOptions`, not nested under a `settings` key — ty rejects the latter
as "unknown options." The fixture also carries a `[tool.ty.environment]` section in
its `pyproject.toml` so a plain `ty check` of it is deterministic too.

### The reference rung: tree-sitter syntax resolved through the LSP

Call hierarchy is call-only, so a function used as a *value* and any use at module
scope are invisible to it. The reference rung is a second, syntactic fact source
layered under the same LSP resolution. It is two layers:

1. **Syntax (tree-sitter).** `crates/hinzu-lsp/src/treesitter.rs` parses each
   Python file with `tree-sitter` + `tree-sitter-python` and enumerates its
   **non-call reference sites** — a name (identifier or `a.b` attribute) used as a
   value: a call argument (`f(g)`), an assignment right-hand side (`x = g`), a
   default parameter (`def h(cb=g)`), a `return`, a collection element, a `pair`
   value, or a bare decorator (`@deco`). It resolves an `a.b.c` attribute at its
   trailing member (`c`), so the member — not the receiver — drives resolution.
   `import` / `from … import` statements are skipped wholesale: importing a name is
   not using it.
2. **Resolution (LSP).** For each site, `extract.rs` calls
   `textDocument/definition` and feeds the target through the *same*
   `classify_and_emit` the call resolver uses — an owned target threads to the
   collected definition, an external one is classified by provenance into an effect
   root, a trusted-pure stdlib baseline (no edge), or a fail-closed `Unknown`. It
   emits an `Edge { kind: reference, resolution: reference }`.

**Attribution.** Each site is attributed to the collected function whose line span
encloses it (its "caller"). A site with **no** enclosing collected function — code
in a class body or at module top level, which runs at *import time* — is attributed
to a synthetic per-file `<module>` definition (id `<module>@<relpath>`, display
`<module>`, spanning the whole file), emitted only when an import-time edge actually
attaches to it. That is what makes SQLAlchemy's declarative models, module-level
`create_engine(...)`, and decorators visible and policeable.

**Deduped against calls.** A call's own callee (`g` in `g(x)`) is *not* re-emitted
as a reference **inside a function** — call hierarchy already emits that `call`
edge — the dedupe done by source position. At **module scope** there is no such
edge, so a call callee there (`Column(...)`, `create_engine(...)`) *is* emitted, on
the `<module>` node. Unresolved reference sites are skipped, exactly as the call
path never enumerates a call it could not resolve.

**Public-API annotation resolution.** A type checker resolves a re-exported public
symbol to its *internal* defining module — ty resolves `create_engine` to
`sqlalchemy.engine.create`, so the reconstructed symbol is
`sqlalchemy.engine.create::create_engine`, but the annotation is the public
`sqlalchemy::create_engine`. `LanguageConfig::effect_of` therefore walks the dotted
package prefixes for an inheriting language (Python), collapsing the qualname onto
each ancestor package until the authored row matches. This is why the SQLAlchemy
`db` rows — latent under call-only because they never matched a deep resolution —
fire the moment the rung lands.

### Sound over what it resolves

Over the calls it sees, Unknown-by-default holds: a resolved third-party call (or
reference) becomes an edge to a `<package>::<member>` symbol with no effect root, so
hinzu-core turns it into an `Unknown` that propagates up the call graph and fails
closed under `on_unknown = fail`, with an evidence path down to the exact package. A
resolved use is never read as false-pure — it reads as "cannot certify" until a
`[trust]` line vouches for the package. A call site ty failed to resolve at all is
still not enumerated (that would need a fuller body walk); the rung adds the
higher-order and module-level uses, not unresolved sites.

### Measured on entl-python (the flagship)

The fleet sweep flagged that entl's Python read-plane (`entl.models`) uses
SQLAlchemy **entirely at module scope** — `declarative_base()`, `Column(...)`, and
`event.listen(...)` in class bodies — so call-only walked none of it, and the
`db` annotation rows were authored but "latent behind call-only until the reference
rung lands." Extracting entl-python before (main, call-only) and after (this rung):

| | main (call-only) | this rung (reference edges) |
| --- | --- | --- |
| effect roots | 4 | 8 |
| `db` roots | **0** | **3** — `create_engine`, `Session.scalar`, `Session.scalars` |
| reference edges | 0 (not emitted) | 507 |
| `<module>` nodes | 0 | 3 |

The `db` effect now **surfaces** (0 → 3 roots) where main saw none: entl's
`create_engine` / `Session.scalar` / `.scalars` uses resolved to their internal
`sqlalchemy.engine.create` / `sqlalchemy.orm.session` modules, which main classified
as fail-closed `Unknown` (the deep path never matched the public `sqlalchemy::` row)
— the public-API annotation resolution above flips them to `db`. Separately, the
reference rung makes `entl.models`' module-level construction (`declarative_base`,
`Column`, `event.listen`) **visible** for the first time, attributed to
`<module>@python/entl/models.py`; because that surface is deliberately *not* vouched
`db` (it is metadata assembly, per the library pack's never-fake-pure rule), it
surfaces as fail-closed `Unknown` — visible and policeable rather than silently
absent. The change is additive: main's four `fs`/`process` roots and their evidence
paths are unchanged, so no real violation vanished.

### Why ty, and toward a native backend

The fact source is deliberately swappable behind the `FactSet` seam. Today ty runs
as an LSP server / subprocess, not an embeddable library — its crates are not a
published stable Rust library API yet. **The intent is to move to a native,
in-process ty backend behind this same seam once ty ships a stable Rust library
API**, dropping the subprocess and the LSP round-trip while emitting the identical
`FactSet`. That is the "ty only via LSP, with intent to go native later" call.

[**pyrefly**](https://github.com/facebook/pyrefly) (Meta; Rust; MIT-licensed;
production-stable — Instagram's default checker at roughly 20 million lines) was
evaluated as the alternative and came out near-tied: comparable resolution and the
same LSP-today / library-later constraint (its crates.io entry is a `0.0.1`
placeholder over internal-only crates). ty was chosen for Astral's trajectory and
the native-later intent — the same team and toolchain hinzu already leans on. Both
would resolve the `pathlib` receivers a real type system sees;
[**ty**](https://github.com/astral-sh/ty) (Astral; Rust; Apache-2.0) is the one
hinzu builds on. **zuban is excluded outright: it is AGPL-licensed, so it cannot be
embedded in MIT-licensed hinzu.**

The swap changes only how a call site resolves, not the schema the adapter emits
or the shared pipeline downstream — the same design that lets Rust and TypeScript
feed one engine.

### ty in CI

The `py-check` job installs the pinned `ty==0.0.61` and runs its live fixture
assertion by driving the **all-Rust** extractor over ty — the same backend used
locally and in real use, no fallback. Because the provenance rules recognize every
stdlib target shape and the extractor pins ty's target, stdlib resolution is
deterministic on the runner. The job also dumps diagnostics (ty version, a
`ty check -vvv` environment/module-resolution grep, and an all-Rust
`HINZU_LSP_DEBUG` dry run of the extractor over the fixture with its stderr
summary) so a future resolution regression yields a real log rather than a bare
failure. The stable Rust jobs stay backend-free regardless: their Python coverage
is the committed sample-facts test, which runs from JSON with no ty.

## How the extractor maps provenance to a category

The extractor (`crates/hinzu-lsp`) reconstructs each external callee's canonical
symbol from its definition target file and its enclosing (class-)qualname. A call
into an owned source file becomes a normal call edge; its effects propagate through
its own body.
A call into one of the built-ins above becomes an effect root, seeded by that
declaration provenance and emitted with a canonical `::`-segmented symbol
(`subprocess::run`, `builtins::open`, `pathlib::Path.mkdir`) — the same shape
Rust and TypeScript use, so a project's `[roots]` / `[trust]` overrides work
identically across all three languages. A pure standard-library or builtin call
draws no edge, so it never becomes an Unknown. A call into any other third-party
package becomes an edge to a `<package>::<member>` symbol with no effect root, so
it is `Unknown` until a `[trust]` line vouches for it.

hinzu-core carries the same table as a shipped annotation set,
`crates/hinzu-core/annotations/python.toml` — the Python counterpart to
`std.toml` and `node.toml` — so its `Unknown` classification agrees with what the
adapter seeds, and a project's `[roots]` / `[trust]` overrides apply identically
across all three languages.

## The shipped library pack: common third-party packages

`python.toml` maps only the standard library. A second shipped file,
`crates/hinzu-core/annotations/python-libs.toml`, carries the well-known
third-party packages a fleet sweep found most often as Unknown, so a project need
not repeat those `[trust]` lines in every `hinzu.toml`. Both files are loaded
as built-in Python defaults and merged — by hinzu-core's root seeding and by the
extractor's effect map alike, one source of truth — and a project's own `[trust]`
/ `[roots]` still overrides either. The pack lives by one rule: never vouch pure
anything that reaches an effect in the tracked vocabulary
(fs/net/db/process/env/clock/random); when a package's surface is mixed or
uncertain, it is left Unknown (fail-closed) rather than guessed.

The first pack ships three packages:

- **rich** — terminal presentation (`Console.print`, `Console.input`, `Table`,
  `Panel`, `markup.escape`, …). None of it reaches the tracked vocabulary, so it
  is vouched pure. Caveat: rich writes to the terminal, which is console I/O;
  hinzu has no `console` effect category today, so "pure" here is only with
  respect to the vocabulary we track, not a claim rich is side-effect-free in
  general.
- **PyYAML** (imported as `yaml`) — `safe_load` / `load` / `dump` operate on a
  string or stream the caller supplies; the library performs no fs or net of its
  own. Pure.
- **SQLAlchemy** — the engine / session / connection execution surface
  (`create_engine`, `Engine.connect`, `Connection.execute`, `Session.execute` /
  `.query` / `.commit`, `sessionmaker`, …) is a `db` effect. The declarative and
  expression *construction* surface (`declarative_base`, `Column`,
  `relationship`, the `select()` / `text()` builders) is pure metadata assembly
  and is deliberately left fail-closed Unknown rather than cleared with a
  package-wide pure vouch — a substring vouch would wrongly clear the execution
  rows. These rows now **fire**: the reference rung emits the module-level and
  higher-order SQLAlchemy uses call-only missed, and `effect_of`'s public-API
  prefix resolution matches the authored `sqlalchemy::` rows against ty's
  deep-resolved `sqlalchemy.engine.create::create_engine` symbols. On entl-python
  this surfaces the `db` effect where call-only saw none (0 → 3 `db` roots — see
  "Measured on entl-python" above); the construction surface stays visible-but-
  fail-closed `Unknown`, as intended.

On `housekeeping` the pack clears every Unknown "cannot-certify" finding (rich 37,
yaml 20 — 57 → 0) while the real forbidden-effect violation set is unchanged (126
fs/net/process reaches, an identical set with identical evidence paths): making a
pure package pure removes no genuine effect root, so no real leak can vanish.
