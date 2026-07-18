# The hinzu Python adapter

A type-directed extractor. It reads a Python project and emits hinzu's
language-independent `FactSet` JSON — the same schema the Rust StableMIR driver
and the TypeScript compiler-API adapter emit, and the schema
`hinzu_core::FactSet::from_json` ingests. The work splits between two tools: the
standard-library `ast` module walks each file with a stack of enclosing functions
(the caller), and [ty](https://github.com/astral-sh/ty) (Astral's Rust type
checker), driven over its LSP (`ty server`), resolves each call site's callee via
`textDocument/definition` — the definition's target file (ty's vendored typeshed,
or an owned/third-party module) plus the enclosing qualname supply the callee's
`full_name` and module path, the declaration provenance the effect roots key on.

**ty is the sole resolution backend.** There is no fallback resolver: if the `ty`
binary is absent the adapter exits nonzero with an honest message. ty is kept
behind the `FactSet` seam so a native in-process ty backend can replace the LSP
subprocess later without changing anything downstream.

## Install and run

```sh
pip install -r requirements.txt   # ty==0.0.61 (ast is stdlib; the adapter has
                                  # no Python-package dependency of its own)
python3 analyze.py <project-dir>  # writes FactSet JSON to stdout, logs to stderr
```

`hinzu check <python-project>` runs this for you: it detects a `pyproject.toml` /
`setup.py` / `setup.cfg`, shells out to `python3 analyze.py`, and feeds the facts
through the shared pipeline (SQLite store, DBSP propagation, `hinzu.toml` policy).
Point `HINZU_PY_ADAPTER` at this `analyze.py` to override the location,
`HINZU_PYTHON` at a specific interpreter, and `HINZU_TY` at a specific `ty` binary.

## Deterministic stdlib resolution

The adapter pins ty's target `python-version` and `python-platform` (to the
interpreter running the adapter) in the LSP `initialize`, and warms ty's vendored
typeshed with a synchronous `ty check --project` before the definition batch. This
makes imported-stdlib resolution (`import subprocess` → `subprocess.run`) resolve
from vendored typeshed **deterministically on any host** — including headless CI
runners, where ty's own environment auto-discovery is unreliable and an un-pinned
`ty server` resolves `builtins` but returns null for imported-stdlib symbols. See
[`notes/python-catalog.md`](../../notes/python-catalog.md).

## What it emits

- **definitions** — one per function-like node in an owned source file (not a
  virtualenv, not build output, not `__pycache__`), with a stable id, display
  name, `language: "python"`, file, and line range.
- **edges** — `call` edges from ty's resolution, `reference` edges where a
  function value is used without being called (a decorator, a callback), and —
  crucially — an edge with `resolution: "unresolved"` for every call site ty
  could not resolve. Both call and reference edges carry effects.
- **effect_roots** — the standard-library, ambient-builtin, and well-known
  third-party effects the walk resolved by declaration provenance, mapped to
  hinzu's one flat, shared effect vocabulary (`fs`, `net`, `process`, `env`,
  `clock`, `random` — the same names Rust and TypeScript use, no `alloc`). See
  [`notes/python-catalog.md`](../../notes/python-catalog.md).

An **unresolved** call site — a duck-typed receiver, `getattr`, a dynamic import —
is emitted as an unknown-target edge, so hinzu-core turns it into an `Unknown` that
fails closed under the default `on_unknown = fail`. It is never silently dropped as
pure; that is what keeps a weak-resolution language sound. A call into a third-party
package the analyzer cannot see through becomes an edge to a `<package>::<member>`
symbol with no effect root, so it is `Unknown` until a `[trust]` line in
`hinzu.toml` vouches for it.

## Honest limits

Python is still the weakest-resolution of the three adapters — its dynamism
(duck-typed receivers, `getattr`, decorators, dynamic import) leaves a residue no
resolver closes. On `housekeeping`, ty resolves about 89.5% of call sites; the
remainder stay `Unknown` ("cannot certify") rather than being read as false-pure —
the sound outcome, not a defect. Because ty is a real type system it closes the
un-typed `pathlib` gap a name-resolver cannot: `p.parent.mkdir()` resolves to a
precise `pathlib::Path.mkdir` `fs` root instead of an `Unknown`. The fact source is
deliberately swappable behind the `FactSet` seam: a native in-process ty backend is
the planned future resolution primitive once ty ships a stable Rust library API, at
the same fidelity through the same contract. See
[`notes/python-catalog.md`](../../notes/python-catalog.md).
