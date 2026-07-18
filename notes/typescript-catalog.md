# The TypeScript effect catalog

hinzu has one flat, shared effect vocabulary. The category names mean the same
thing in every language: `fs` is `fs` for Rust and for TypeScript, `net` is
`net`, and so on. A language does not get its own namespace and does not rename a
shared category. What a language chooses is which categories it *seeds* — the
subset of the shared vocabulary its runtime actually exposes as a certifiable
effect. A category that does not apply to a language simply does not appear for
it.

TypeScript seeds this subset: `fs`, `net`, `process`, `env`, `clock`, `random`.
The sections below list what each one is seeded from, keyed on the callee's
declaration provenance — the file and name the TypeScript checker resolves a call
to, which is what the adapter reads.

## What TypeScript seeds

- **`fs`** — the Node filesystem modules: `node:fs`, `fs`, and `node:fs/promises`
  (`fs/promises`). Any call whose callee resolves into these declarations is an
  `fs` root.
- **`net`** — the Node network modules `node:net`, `node:http`, `node:https`,
  `node:http2`, `node:tls`, `node:dgram`, and `node:dns`; the ambient globals
  `fetch` and `WebSocket` (resolved against `lib.dom.d.ts` / `@types/node`, not a
  same-named user symbol); and the network npm package `undici`.
- **`process`** — `node:child_process` (`spawn`, `exec`, `fork`, and the rest);
  the subprocess npm packages `cross-spawn` and `execa`.
- **`env`** — ambient reads of the process environment: `process.env`,
  `process.argv`, `process.argv0`, `process.cwd`, and `process.exit`, each
  confirmed against the Node/lib declaration so a user object named `process`
  never misfires.
- **`clock`** — the wall-clock and monotonic-time reads `Date.now` and
  `performance.now`.
- **`random`** — nondeterminism: `Math.random`, `crypto.getRandomValues`, and the
  randomness surface of `node:crypto` (`randomBytes`, `randomUUID`, `randomInt`,
  `randomFillSync`, `randomFill`, `generateKeyPair`, `generateKeyPairSync`). The
  rest of `node:crypto` — hashes, ciphers — is left pure, because it is not a
  source of nondeterminism.

`db` is a shared category, but TypeScript has no built-in database primitive to
seed it from. A project reaches its database through a package (`pg`, `mysql`,
`better-sqlite3`), so `db` is declared per project with a `[trust]` line rather
than shipped as a built-in — for example `[trust] "pg" = ["db"]` in `hinzu.toml`.

## Why there is no `alloc` for TypeScript

Rust seeds an `alloc` effect: heap allocation is a real, certifiable cost a
performance-sensitive Rust region can forbid, and the standard library marks the
APIs that allocate. TypeScript runs on a garbage-collected runtime where an
allocation is not an observable effect a functional-core policy can meaningfully
forbid — every value construction may allocate, and the collector, not the
caller, governs it. So `alloc` is absent for TypeScript. It is absent, not
renamed: no `ts/alloc`, no substitute category. TypeScript seeds the subset above
and nothing more.

## Future candidates

Three effects may earn a place later. If they do, each is a shared-vocabulary
name usable by any language whose runtime exposes it — never a TypeScript-only
category:

- **`async`** — a function that suspends (returns a promise, `await`s). Useful for
  a region that must stay synchronous.
- **`throws`** — a function that can raise. Useful for a total-function boundary.
- **`dom`** — reads or writes to the document / browser environment.

None of these is in the v1 vocabulary. They are recorded here so that if the
vocabulary grows, it grows with shared names.

## How the adapter maps provenance to a category

The adapter (`adapters/typescript/analyze.mjs`) resolves each call with the
TypeScript checker (`getResolvedSignature`, symbol aliasing) and reads the
callee's declaration file. A call into an owned source file becomes a normal call
edge; its effects propagate through its own body. A call into one of the
built-ins above becomes an effect root, seeded by that declaration file — the
sound way to catch a re-exported or aliased `readFile`, and the only way to catch
an ambient global like `fetch`, which is not an import. A call into any other
third-party package becomes `Unknown` until a `[trust]` line vouches for it, so
an unseen dependency can never be read as pure by omission.

hinzu-core carries the same table as a shipped annotation set,
`crates/hinzu-core/annotations/node.toml` — the TypeScript counterpart to
`std.toml` — so its `Unknown` classification agrees with what the adapter seeds,
and a project's `[roots]` / `[trust]` overrides apply identically across both
languages.
