# The hinzu TypeScript adapter

A native TypeScript compiler-API extractor. It reads a TypeScript project and
emits hinzu's language-independent `FactSet` JSON — the same schema the Rust
StableMIR driver emits, and the schema `hinzu_core::FactSet::from_json` ingests.
The heavy lifting is the TypeScript checker's: `ts.createProgram` from the
project's own `tsconfig.json`, then `checker.getResolvedSignature()` at each call
site with an enclosing-function walk to attribute the call to its caller.

## Install and run

```sh
npm install                       # typescript + @types/node, nothing else
node analyze.mjs <project-dir>    # writes FactSet JSON to stdout, logs to stderr
```

`hinzu check <ts-project>` runs this for you: it detects a `tsconfig.json` /
`package.json`, shells out to `node analyze.mjs`, and feeds the facts through the
shared pipeline (SQLite store, DBSP propagation, `hinzu.toml` policy). Point
`HINZU_TS_ADAPTER` at this `analyze.mjs` to override the location, and `HINZU_NODE`
at a specific `node`.

## What it emits

- **definitions** — one per function-like node in an owned source file (not
  `node_modules`, not `.d.ts`, not build output), with a stable id, display name,
  `language: "typescript"`, file, and line range.
- **edges** — `call` edges from `getResolvedSignature`, and `reference` edges
  where a function value is used without being called (a callback, a default
  parameter). Both carry effects, so higher-order flow is not lost.
- **effect_roots** — the Node built-ins and ambient globals the walk resolved by
  declaration provenance, mapped to hinzu's shared effect vocabulary. See
  [`notes/typescript-catalog.md`](../../notes/typescript-catalog.md).

An external call that is neither an owned function nor a known built-in becomes an
edge to a `<package>::<member>` symbol with no effect root, so hinzu-core marks it
`Unknown` and a policy can refuse to certify code that reaches it — until a
`[trust]` line in `hinzu.toml` vouches for the package.

## Honest limits

`any`-typed and dynamically dispatched calls do not resolve and are left out
rather than invented (an under-approximation). A project without its
`node_modules` installed resolves fewer third-party calls; the adapter falls back
to the import specifier to still name the package. `require(variable)` and
`import(expr)` are not followed.
