# The hinzu Python adapter

A name-resolution-grade extractor. It reads a Python project and emits hinzu's
language-independent `FactSet` JSON — the same schema the Rust StableMIR driver
and the TypeScript compiler-API adapter emit, and the schema
`hinzu_core::FactSet::from_json` ingests. The work splits between two tools: the
standard-library `ast` module walks each file with a stack of enclosing functions
(the caller), and [Jedi](https://github.com/davidhalter/jedi) resolves each call
site's callee (`script.goto(follow_imports=True)`), which supplies the callee's
`full_name` and module path — the declaration provenance the effect roots key on.

## Install and run

```sh
pip install -r requirements.txt   # jedi, nothing else (ast is stdlib)
python3 analyze.py <project-dir>  # writes FactSet JSON to stdout, logs to stderr
```

`hinzu check <python-project>` runs this for you: it detects a `pyproject.toml` /
`setup.py` / `setup.cfg`, shells out to `python3 analyze.py`, and feeds the facts
through the shared pipeline (SQLite store, DBSP propagation, `hinzu.toml` policy).
Point `HINZU_PY_ADAPTER` at this `analyze.py` to override the location, and
`HINZU_PYTHON` at a specific interpreter.

## What it emits

- **definitions** — one per function-like node in an owned source file (not a
  virtualenv, not build output, not `__pycache__`), with a stable id, display
  name, `language: "python"`, file, and line range.
- **edges** — `call` edges from Jedi's resolution, `reference` edges where a
  function value is used without being called (a decorator, a callback), and —
  crucially — an edge with `resolution: "unresolved"` for every call site Jedi
  could not resolve. Both call and reference edges carry effects.
- **effect_roots** — the standard-library, ambient-builtin, and well-known
  third-party effects the walk resolved by declaration provenance, mapped to
  hinzu's one flat, shared effect vocabulary (`fs`, `net`, `process`, `env`,
  `clock`, `random` — the same names Rust and TypeScript use, no `alloc`). See
  [`notes/python-catalog.md`](../../notes/python-catalog.md).

An **unresolved** call site — a duck-typed receiver, an un-typed `pathlib` chain
like `target.parent.mkdir`, `getattr`, a dynamic import — is emitted as an
unknown-target edge, so hinzu-core turns it into an `Unknown` that fails closed
under the default `on_unknown = fail`. It is never silently dropped as pure; that
is what keeps a weak-resolution language sound. A call into a third-party package
the analyzer cannot see through becomes an edge to a `<package>::<member>` symbol
with no effect root, so it is `Unknown` until a `[trust]` line in `hinzu.toml`
vouches for it.

## Honest limits

Python resolves only about 78% of call sites — it is the weakest-resolution of
the three adapters. Un-typed `pathlib` receivers are a known gap: `p.parent`
loses Jedi's type, so `p.parent.mkdir()` is unresolved and surfaces as an
`Unknown` ("cannot certify") rather than a false-pure. That is the sound outcome,
not a defect. The fact source is deliberately swappable behind the `FactSet`
seam: a native-Rust type checker — [pyrefly](https://github.com/facebook/pyrefly)
(Meta, MIT, production-stable) ahead of [ty](https://github.com/astral-sh/ty)
(Astral, Apache-2.0, preview) — is the planned future backend once one ships a
stable library API, at higher fidelity through the same contract. See
[`notes/python-catalog.md`](../../notes/python-catalog.md).
