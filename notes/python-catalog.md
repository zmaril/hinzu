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
declaration provenance — the module and name Jedi resolves a call to, which is
what the adapter reads.

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
  Jedi so a shadowed local `os` never misfires.
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

## Fidelity: the weakest of the three adapters, and sound anyway

The adapter is name-resolution-grade: Jedi resolves a call site to a declaration
by name and import following, not by a full type system. On a real codebase it
resolves about 78% of call sites — the weakest of hinzu's three adapters, well
below the TypeScript checker's 97% and the StableMIR driver's 99.95%. The gap is
Python's dynamism: duck-typed receivers, decorators, `getattr`, dynamic import,
and — the most common single gap — un-typed `pathlib` chains, where `p.parent`
loses Jedi's type so `p.parent.mkdir()` cannot be resolved to the `pathlib`
method.

This is where Unknown-by-default earns its keep. Every unresolved call site is
emitted as an unknown-target edge, so hinzu-core turns it into an `Unknown` that
propagates up the call graph and fails closed under `on_unknown = fail`. A
weak-resolution language would be unsound if unresolved calls read as pure; here
they read as "cannot certify," with an evidence path down to the exact site that
could not be resolved. The un-typed `pathlib.Path.mkdir` gap therefore shows up
as an honest Unknown, never as a false-pure. Soundness does not depend on
resolution strength — only precision does.

## The planned native backends: pyrefly and ty

The fact source is deliberately swappable behind the `FactSet` seam. Jedi is
today's backend because it is the mature, maintained name-resolution engine with
a stable Python library API. Two native-Rust type checkers are the candidates to
supersede it, because a real type system would resolve the `pathlib` receivers
and much of the duck-typed surface Jedi cannot, turning many of today's Unknowns
into precise effect edges:

- [**pyrefly**](https://github.com/facebook/pyrefly) (Meta; Rust; MIT-licensed;
  production-stable — it is Instagram's default checker at roughly 20 million
  lines, and its aggressive inference is what would close the un-typed `pathlib`
  gap). It ranks ahead of ty on maturity.
- [**ty**](https://github.com/astral-sh/ty) (Astral; Rust; Apache-2.0; preview).

The honest blocker is shared: neither exposes a stable Rust *library* API yet.
pyrefly's crates.io entry is a `0.0.1` placeholder over internal-only crates, and
both ship as an LSP server / subprocess today, not an embeddable library. So
whichever ships a stable library API first becomes the in-process native backend
behind this same `FactSet` seam; until then, either is an LSP-driven upgrade over
Jedi (a subprocess that resolves better), not a native embed. **zuban is excluded
outright: it is AGPL-licensed, so it cannot be embedded in MIT-licensed hinzu.**

Either way, the swap changes only how a call site resolves, not the schema the
adapter emits or the shared pipeline downstream — the same design that lets Rust
and TypeScript feed one engine. Until a native library API exists, hinzu keeps
building on Jedi.

## How the adapter maps provenance to a category

The adapter (`adapters/python/analyze.py`) resolves each call with Jedi and reads
the callee's `full_name` and module path. A call into an owned source file
becomes a normal call edge; its effects propagate through its own body. A call
into one of the built-ins above becomes an effect root, seeded by that
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
