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

## The resolution backend: ty over LSP, with Jedi as the fallback

The adapter resolves each call site with one of two interchangeable backends
behind the `FactSet` seam. The AST walk, the caller attribution, and the whole
owned/effect/stdlib/third-party classification are identical whichever backend
ran; the only swappable part is how a call-site position becomes a declaration.

- **ty** (Astral's Rust type checker) is the default, driven over its LSP. The
  adapter spawns `ty server`, opens every source file, waits for the first check
  pass to settle, then pipelines a `textDocument/definition` request at each
  callee token. The definition's target — a file in ty's vendored typeshed, or an
  owned or third-party module — plus the enclosing qualname at the target give the
  provenance the effect roots key on. Because ty is a real type system it resolves
  the un-typed `pathlib` receivers and much of the duck-typed surface Jedi cannot.
- **Jedi** is the zero-extra-dependency fallback: name-resolution-grade
  `script.goto(follow_imports=True)`, used automatically when the `ty` binary is
  absent so the adapter always runs. `HINZU_PY_BACKEND` forces `ty` or `jedi`;
  `HINZU_TY` overrides the ty binary path.

### What ty buys, measured on housekeeping

Running the full pipeline over `housekeeping` (a pure-Python fleet auditor, 82
files, 486 definitions, 2,449 call sites) with an illustrative functional-core
policy, ty against the Jedi baseline:

| | Jedi (fallback) | ty (default) |
| --- | --- | --- |
| call sites resolved | 83.0% (2,032) | **89.5% (2,192)** |
| unresolved (Unknown) call sites | 417 | **257** |
| `fs`-effect call edges | 28 | **117** (≈4×) |
| effect roots | 19 | 22 |
| forbidden-effect violations | 15 (1 fs, 14 process) | **20 (6 fs, 14 process)** |
| `cannot certify` (Unknown) findings | 136 | **86** |

The `fs` coverage roughly quadruples and the Unknown pile shrinks by a third. The
gain is almost entirely the un-typed `pathlib` gap ty closes: chains like
`(ctx.workdir / "src-tauri").is_dir()` and `p.parent.mkdir()`, which Jedi emitted
as unresolved Unknowns, ty resolves to real `pathlib::Path.is_dir` /
`pathlib::Path.mkdir` `fs` roots — so they become precise `forbids fs` findings in
the core rather than "cannot certify." The total finding count *drops* (151 → 106)
precisely because honest gaps turn into precise edges (some pure, some real
violations). The whole run stays well under two seconds with request pipelining.

### Sound whichever backend runs

No backend resolves everything — Python's dynamism (duck-typed receivers,
`getattr`, decorators, dynamic import) leaves a residue, 10.5% even for ty. This
is where Unknown-by-default earns its keep: every unresolved call site is emitted
as an unknown-target edge, so hinzu-core turns it into an `Unknown` that
propagates up the call graph and fails closed under `on_unknown = fail`, with an
evidence path down to the exact site. A weak-resolution language would be unsound
if unresolved calls read as pure; here they read as "cannot certify." Soundness
does not depend on resolution strength — only precision does, and that is exactly
what ty improves.

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

### ty in CI: default locally, non-blocking on the runner

ty is the default backend everywhere the `ty` binary is present — local
development and real use. On the headless GitHub Actions runner, though, ty's LSP
resolves the fixture inconsistently: the same cold run that resolves
`builtins.open` returns null for imported-stdlib symbols such as `subprocess.run`,
deterministically, in a way that does not reproduce on a normal workstation even
with an empty ty cache (cold, the server resolves `subprocess.run` on the first
poll in about 50 ms). The adapter already settles on a readiness probe and retries
nulls with backoff, but this specific runner behavior persists. Rather than gate
the pull request on that ty-in-CI flakiness, the `py-check` job runs its live
assertion on the deterministic Jedi fallback; the ty numbers above are from a real
local run of the full pipeline. A non-blocking ty smoke step is deliberately *not*
added — `continue-on-error` on a `cargo test` step is exactly what this repo's own
`ci-continue-on-error` self-guard forbids, and dodging that guard to show a green
ty run would be dishonest. When ty's headless-CI behavior stabilizes, the live
assertion moves to ty. The stable Rust jobs stay backend-free regardless: their
Python coverage is the committed sample-facts test, which runs from JSON with no ty
or Jedi.

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
