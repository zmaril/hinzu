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
- **`env`** — reads of the ambient process environment: `os.environ` (and
  `os.environb`), and `os.getenv` / `os.putenv` / `os.unsetenv`. The common
  idiom is `os.environ.get(...)`, where the `.get` itself is a pure dict method,
  so the adapter seeds the effect on the `os.environ` receiver, confirmed against
  ty so a shadowed local `os` never misfires.
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

## The resolution backend: ty over LSP, the sole backend

The adapter resolves each call site with **ty** (Astral's Rust type checker), its
sole resolution backend, driven over its LSP behind the `FactSet` seam. The AST
walk, the caller attribution, and the whole owned/effect/stdlib/third-party
classification are backend-independent; the only thing the seam abstracts is how a
call-site position becomes a declaration.

The adapter spawns `ty server`, opens every source file, settles the first check
pass, then pipelines a `textDocument/definition` request at each callee token. The
definition's target — a file in ty's vendored typeshed, or an owned or third-party
module — plus the enclosing qualname at the target give the provenance the effect
roots key on. Because ty is a real type system it resolves the un-typed `pathlib`
receivers and much of the duck-typed surface a name-resolver cannot.

There is **no fallback resolver**. If the `ty` binary is absent the adapter exits
nonzero with an honest message; it never silently degrades to a weaker resolver
and never fakes a resolution. `HINZU_TY` overrides the ty binary path.

### What ty buys, measured on housekeeping

Running the full pipeline over `housekeeping` (a pure-Python fleet auditor, 82
files, 486 definitions, 2,449 call sites) with an illustrative functional-core
policy:

| | ty |
| --- | --- |
| call sites resolved | **89.5% (2,192)** |
| unresolved (Unknown) call sites | **257** |
| `fs`-effect call edges | **117** |
| effect roots | 22 |
| forbidden-effect violations | **20 (6 fs, 14 process)** |
| `cannot certify` (Unknown) findings | **86** |

Because ty is a real type system, un-typed `pathlib` chains like
`(ctx.workdir / "src-tauri").is_dir()` and `p.parent.mkdir()` — which a
name-resolver leaves unresolved — resolve to real `pathlib::Path.is_dir` /
`pathlib::Path.mkdir` `fs` roots, so they become precise `forbids fs` findings in
the core rather than "cannot certify." That is why `fs` coverage reaches 117 call
edges. The whole run stays well under two seconds with request pipelining.

### Recognizing every stdlib target (the headless-runner fix)

ty resolves an imported stdlib symbol to whichever declaration it finds. Usually
that is a stub in ty's **vendored typeshed** (`.../typeshed/<hash>/stdlib/
subprocess.pyi`). But on a host whose interpreter ships a full standard library —
notably a headless GitHub Actions runner — ty resolves `import subprocess` to the
interpreter's **real stdlib source** (`.../lib/python3.11/subprocess.py`) instead.
Both are the standard library; ty picks one per host.

The adapter reconstructs a callee's provenance from that definition *target file*.
Its `module_of_target` originally recognized only two external shapes — ty's
vendored typeshed and installed `site-packages` — and classified anything else,
including a real-stdlib source path, as an unknown `OTHER`. So on the runner
`subprocess.run` resolved to `.../lib/python3.11/subprocess.py`, fell into `OTHER`,
and was emitted as an **unresolved** edge that fails closed as `Unknown` — while
`builtins.open` (a C builtin with no real source module, always resolved to the
vendored `builtins.pyi`) classified fine. That asymmetry — `builtins.open` resolves
but `subprocess.run` does not — read like ty "returning null for imported-stdlib,"
but it was a provenance-classification gap in the adapter, not a ty failure; ty
resolved the symbol correctly, just to a path the adapter did not recognize. It did
not reproduce on a workstation because there ty resolved the same import to its
vendored typeshed, which the adapter already recognized.

The fix classifies **any** stdlib target as STDLIB:

- `module_of_target` now recognizes a real CPython stdlib path — a
  `.../pythonX.Y/<module>.pyi?` file that is not under `site-packages` /
  `dist-packages` — as STDLIB (`.../python3.11/subprocess.py` → `subprocess`),
  alongside the vendored-typeshed and site-packages shapes it already handled.

The adapter also **pins ty's target** `python-version` and `python-platform` (to
the interpreter running the adapter) in the LSP `initialize`, via ty's
`initializationOptions` (`configuration.environment`, with
`diagnosticMode: workspace` so the whole project is indexed), so the stdlib
typeshed is selected deterministically rather than by ty's environment inference.
These options are passed at the top level of `initializationOptions`, not nested
under a `settings` key — ty rejects the latter as "unknown options." The fixture
also carries a `[tool.ty.environment]` section in its `pyproject.toml` so a plain
`ty check` of it is deterministic too.

### Sound whichever call resolves

ty does not resolve everything — Python's dynamism (duck-typed receivers,
`getattr`, decorators, dynamic import) leaves a residue, about 10.5%. This is where
Unknown-by-default earns its keep: every unresolved call site is emitted as an
unknown-target edge, so hinzu-core turns it into an `Unknown` that propagates up
the call graph and fails closed under `on_unknown = fail`, with an evidence path
down to the exact site. A weak-resolution language would be unsound if unresolved
calls read as pure; here they read as "cannot certify." Soundness does not depend
on resolution strength — only precision does, and that is exactly what a real type
system improves.

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
assertion on **ty** — the same backend used locally and in real use, no fallback.
Because the adapter recognizes every stdlib target shape and pins ty's target,
stdlib resolution is deterministic on the runner. The job also dumps ty resolution
diagnostics (version, `ty check -vvv` environment/module-resolution, and a
`textDocument/definition` probe of `subprocess.run` vs `open` with the ty server's
own logs) so a future resolution regression yields a real log rather than a bare
failure. The stable Rust jobs stay backend-free regardless: their Python coverage
is the committed sample-facts test, which runs from JSON with no ty.

## How the adapter maps provenance to a category

The adapter (`adapters/python/analyze.py`) resolves each call with the chosen
backend and reads the callee's `full_name` and module path (for ty, reconstructed
from the definition target file and its enclosing qualname). A call into an owned
source file becomes a normal call edge; its effects propagate through its own body.
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
