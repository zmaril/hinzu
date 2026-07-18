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

### Honest fidelity: call-only

`callHierarchy/outgoingCalls` reports only the calls ty resolved, so the generic
extractor is **call-only**: unlike the old AST walk it does not see three things —
higher-order `reference` edges (a function passed as a value/callback/decorator),
an ambient attribute read such as `os.environ`, and a call site ty could not
resolve at all. All three need a language body walk, deferred to a future
language-agnostic tree-sitter rung (also Rust). Unknown-by-default over the calls
it *does* resolve keeps the result sound — a resolved call into an unvouched
third-party package is an `Unknown` that fails closed, never a silent pure.

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

### Sound over what it resolves

Because the extractor is call-only, its `Unknown` set is narrower than the old AST
adapter's: it flags an `Unknown` for a *resolved* call into a third-party package
the analyzer cannot see through, but it cannot enumerate a call site ty failed to
resolve (that needs a body walk — the deferred tree-sitter rung). Over the calls it
does see, Unknown-by-default holds: a resolved third-party call becomes an edge to a
`<package>::<member>` symbol with no effect root, so hinzu-core turns it into an
`Unknown` that propagates up the call graph and fails closed under
`on_unknown = fail`, with an evidence path down to the exact package. A resolved
call is never read as false-pure — it reads as "cannot certify" until a `[trust]`
line vouches for the package.

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
  rows. Honest caveat: the extractor is call-only, and SQLAlchemy usage is largely
  module-level (declarative models at class scope) which emits no call edges, so
  these rows will not reduce Unknowns until the reference-level rung lands; they
  are authored correctly now so they fire the moment it does.

On `housekeeping` the pack clears every Unknown "cannot-certify" finding (rich 37,
yaml 20 — 57 → 0) while the real forbidden-effect violation set is unchanged (126
fs/net/process reaches, an identical set with identical evidence paths): making a
pure package pure removes no genuine effect root, so no real leak can vanish.
