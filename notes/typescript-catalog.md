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

## Reference-level extraction

Call edges are only half the picture. A function reaches an effect two other ways
a call-only view never sees, and the adapter emits a `reference` edge for each —
resolved through the *same* declaration → provenance → effect path as a call, so
it is sound-additive: it only ever adds edges and roots, never removes one, so no
violation the call view found can vanish.

- **Higher-order references.** A value-position use of an effectful symbol — a
  function passed as a callback (`register(readFile)`), stored, returned, put in
  an array/object literal, or used as a default parameter — taints its enclosing
  function, because that value reaches its effect when something later invokes it.
  Both an owned function used as a value and an external effectful symbol
  (`fetch`, a node built-in like `fs.readFile`, an effectful npm import) draw the
  edge. The callee of a call is *not* re-emitted as a reference — the dedupe is by
  position, so nothing is counted as both a call and a reference.
- **Module-level (import-time) effects.** Code that runs when a module is
  imported — a top-level `fetch(...)`, an effectful call outside any function, a
  module-scope initializer like `export const api = treaty(origin)` — has no
  enclosing function to attribute to. The adapter gives each file a synthetic
  `<module>` definition (`<module>@<relpath>`, whole-file span) and attributes
  import-time effects to it, so they are visible and policeable rather than
  silently dropped. The node is emitted only for a file whose import-time code
  actually reaches an effect, so pure files spawn no empty node. This mirrors the
  `<module>` node the Python tree-sitter rung introduced.

Because a bare reference to an effectful value taints even where the value is only
read (for example reading `WebSocket.OPEN` references the `WebSocket` network
primitive), the reference rung is a deliberate over-approximation: it never misses
a real effect, at the cost of occasionally flagging a symbol that is referenced
but not invoked — the same soundness-over-precision trade the Python rung makes.

hinzu-core carries the same table as a shipped annotation set,
`crates/hinzu-core/annotations/node.toml` — the TypeScript counterpart to
`std.toml` — so its `Unknown` classification agrees with what the adapter seeds,
and a project's `[roots]` / `[trust]` overrides apply identically across both
languages.

## The shipped library pack

`node.toml` covers the Node runtime's own surface. A second shipped set,
`crates/hinzu-core/annotations/node-libs.toml`, covers the npm packages the fleet
reaches most often, so a plain functional-core check stops reporting them as
`Unknown` without a project having to write a `[trust]` line for each one. It is
merged onto `node.toml` for the TypeScript language base, and a project's own
`hinzu.toml` still overrides anything in it.

The pack follows one hard rule: a package, or a call within it, that performs I/O
is never marked pure. A mixed package is annotated at its effectful entry points,
and only its genuinely-pure remainder is vouched pure. Two packages show why the
granularity matters:

- **drizzle-orm** is split at the seam between building a query and running it.
  The query builders — `eq`, `and`, `or`, `sql`, `asc`, `desc`, `relations`, and
  the comparison and aggregate helpers — build SQL fragments in memory and are
  pure. Only the execution surface reaches the database and is `db`: `.select`,
  `.from`, `.where`, `.insert`, `.values`, `.update`, `.set`, `.delete`,
  `.returning`, `.transaction`, `.execute`, and the `.all` / `.run` / `.get`
  drivers. Keeping `eq(users.id, 1)` out of `db` is the accuracy win — it is a
  pure value, not a read.
- **bun-types** is the Bun runtime's ambient types. Its `bun:test` API — `expect`,
  `describe`, `test`, and the `to*` matcher families — is pure and is the largest
  single source of `Unknown` in a test-heavy repo. Bun's actual I/O is graded:
  `Bun.spawn` and `Bun.spawnSync` are `process`, `Bun.file` and `Bun.write` are
  `fs`, and `Bun.serve` is `net`.

The rest of the pack is whole-package: `@electric-sql/pglite` is `db`; `elysia`,
`@elysiajs/eden`, and `@modelcontextprotocol/sdk` are `net`; `@disponent/node` is
`process`. The UI and utility packages — react, react-dom, zustand, the xterm and
CodeMirror widgets, `@mantine/core`, `@dnd-kit/core`, ts-pattern, and
`@sinclair/typebox` — carry nothing in hinzu's vocabulary and are vouched pure,
with one caveat: the vocabulary is `fs` / `net` / `db` / `process` / `env` /
`clock` / `random`, so a package whose only side effect is on the DOM or a
rendered view is "pure" only in that vocabulary. DOM and render effects are
outside it and are not modeled.
