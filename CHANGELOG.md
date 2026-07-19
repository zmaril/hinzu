# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **`hinzu ranges` — freerange-style numeric range / abstract-interpretation
  analysis, catching integer divide-by-zero at compile time on Rust via MIR
  intervals.** A new subcommand infers, per function, the interval each value can
  hold (interval + integer flag + may-be-NaN + one excluded point, with
  IEEE-754-exact float arithmetic) and flags arithmetic hazards — integer
  divide-by-zero and remainder-by-zero today — as evidence-carrying facts: which
  function, which statement location, and the divisor range that proves it. The
  architecture keeps the analysis **language-agnostic**: the interval domain, the
  worklist abstract-interpretation engine (with widening and branch refinement),
  and the hazard detection live in a pure `hinzu_core::absint` module (passes the
  functional-core self-check — no I/O, only `alloc`), consuming a
  language-neutral body-fact IR. Only the fact **extraction** is Rust-specific:
  the StableMIR driver, under `HINZU_EMIT_BODIES`, lowers each function's MIR
  basic blocks, assignment statements, and terminators into that IR — so a new
  language later is a new extractor feeding the same engine. rustc inserts a
  divide-by-zero assert before integer division; the engine deliberately does not
  refine on that assert (which would prove every division safe), so what it proves
  is that the inserted panic is *reachable* — while a user guard (`if c != 0 { ..
  / c }`, compiled to a `SwitchInt`) is refined and correctly discharges the
  divisor (the excluded-point trick). Analysis is intraprocedural; the `Call`
  terminator carries the seam for interprocedural range propagation (freerange's
  requirement peeling) to ride hinzu's call-graph fixpoint later. `hinzu ranges
  <path>` emits deterministic JSON (per-function parameter/return ranges + hazards
  with evidence) and exits non-zero when a hazard is found, so it is usable as a
  CI gate; `--bodies <json>` analyzes pre-extracted body facts with no nightly
  toolchain. A committed `crates/hinzu-cli/tests/fixtures/ranges-demo` (bodies
  extracted from the real driver) gives stable-CI coverage — an unguarded `width
  / count` and `x % n` are flagged, a guarded divide and a divide by a nonzero
  constant are not — plus an `#[ignore]`d live end-to-end test over the demo
  crate.
- **A rule-engine core: the effect-region check now runs as the first rule
  behind a pluggable `Rule` trait.** hinzu's effect analysis is lifted into a
  shared shape — a rule is a query over a `RuleContext` (the fact set, the
  per-symbol effect summaries, the policy, and the forward adjacency that
  reconstructs evidence paths) that emits `Finding`s, and a small engine folds
  every enabled rule over one shared context and concatenates the results. A
  `Finding` generalizes the effect check's `Violation`: it keeps the rule id,
  the flagged definition and its location, the human-facing message, the
  evidence chain, and the severity, without hard-coding an effect or a region,
  so a future structure-shaped rule fills the same fields. The `hinzu.toml`
  policy gains a `[rules]` section — `enable` turns on named rules and each
  `[rules.<id>]` subtable carries that rule's own config — while the
  effect-region policy keeps its `[region.*]` surface and still runs on the
  presence of a region. A file with no `[rules]` section parses and behaves
  exactly as before, and `hinzu check` produces the same violations, exit codes,
  and report as it did, now routed through the engine. This is scaffolding: the
  seam is design-agnostic and leaves room for the language-understanding rules
  to attach later, none of which ship here.
- **Full reference-level parity for the native TypeScript adapter — higher-order
  and module-level (import-time) effects in the tsc compiler API.** The adapter
  already drew `reference` edges for a bare identifier resolving to an owned
  function used as a value; two flows call-only still missed are now covered,
  each resolved through the *same* declaration → provenance → effect path as a
  call. **Higher-order:** a value-position use of an *external* effectful symbol —
  an ambient global (`register(fetch)`), a node built-in (`register(fs.readFile)`,
  `register(readFile)` from a named `node:fs` import), or an effectful npm import —
  now taints its enclosing function, not only owned functions. **Module-level:**
  code that runs at import time (a top-level `fetch(...)`, an effectful call
  outside any function, a module-scope initializer like
  `export const api = treaty(origin)`) has no enclosing function, so call-only
  dropped it entirely; the adapter now gives each file a synthetic `<module>`
  definition (`<module>@<relpath>`, whole-file span) and attributes import-time
  effects to it as `reference` edges, emitting the node only for a file whose
  import-time code actually reaches an effect — exactly the `<module>` model the
  Python tree-sitter rung introduced. A call callee is never re-emitted as a
  reference (deduped by position, now including tagged-template tags), so the rung
  is **sound-additive**: it only adds the higher-order and import-time effects the
  call view could not see. On **powdermonkey** (236-file Bun/React app) the rung
  lifted reference edges 214 → 239, seeded three roots call-only missed
  (`WebSocket`, `process.argv`, `fs.readdirSync`), gave 101 files a `<module>`
  node, and — under an illustrative browser-must-not-touch-network policy — added
  six findings atop the 58 the call view already had (every one preserved): an
  import-time `treaty(...)` client and five higher-order `WebSocket` references. A
  committed `adapters/typescript/tests/reference-fixture` (higher-order
  `node:fs::readFile` reach + a module-level `fetch` on the file's `<module>`
  node) gives stable-CI coverage with no Node required, plus an `#[ignore]`d live
  end-to-end test.
- **Reference-level edges for the native Rust adapter — higher-order and
  import-time effects natively from MIR.** The StableMIR driver
  (`crates/hinzu-rustc-driver`) was call-only: it walked each monomorphized body's
  `Call` terminators, so a function used as a *value* — passed as a callback
  (`register(foo)`), assigned, returned, reified to a fn-pointer, stored in a
  struct field (straitjacket's `RegexRule { judge: judge_font }`), or captured in a
  closure handed elsewhere — produced no edge, and a closure's own body was
  recorded as a bare definition but never walked, so both flows were invisible. The
  driver now also walks each body's statements/operands and emits `Edge{kind:
  reference, resolution: reference}` for those non-call *uses*: a `FnDef`/closure
  constant in a call argument, an assignment RHS, a returned operand or a
  fn-pointer reification (`visit_operand`); a closure `Rvalue::Aggregate`
  construction (`visit_rvalue`); and the initializer body of a `static`/`const` —
  including a `LazyLock`/lazy static, the Rust analogue of a module-level
  import-time effect, attributed to the static's own id. Referenced closures are
  walked under their own def id so a reference edge into one transitively carries
  its body's effects. Resolution rides the *same* `Instance::resolve` → provenance
  → effect path as calls, and effect roots seed identically, so a referenced
  effectful item taints its user exactly as a direct call would. A `Call`'s own
  callee is emitted once, as a `call`, never re-emitted as a reference (the two
  paths are disjoint by construction). This is the rung the Python tree-sitter
  driver and the TypeScript checker already reached, done natively from
  monomorphized MIR — **the call-only caveat is lifted for Rust**. The rung is
  **sound-additive**: it only adds edges/effects, so no violation call-only found
  can vanish. On **straitjacket** it added 126 reference edges and surfaced 3
  higher-order `fs` findings call-only missed — three closure bodies that read
  files (`main`, `Projects::discover`, `walk::collect_files`), previously
  un-walked — while leaving the pre-existing 9 forbidden-`fs` violators unchanged
  (12 = 9 + 3, none removed); the stored fn-pointer `judge` case
  (`pattern_rules → judge_font`) draws its reference edge and correctly does not
  taint, since those judges are pure. A committed fixture
  (`adapters/rust/tests/reference-fixture/`, with its driver-produced
  `reference-fixture-facts.json`) gives stable-CI coverage of the higher-order
  callback, the handed-off closure, and the lazy import-time initializer with no
  nightly toolchain required (`crates/hinzu-cli/tests/rust_check.rs`). Walking
  closure and `static`/`const` bodies also refined the driver's edge resolution:
  a **named** `FnDef` call target the driver could not *monomorphize* (a
  generic/trait call in a polymorphic body — `<P as AsRef<Path>>::as_ref`) is now
  labeled `resolution: call`, keeping its trait-method name for the engine's
  name-based resolution order (a std name clears to trusted-pure; an unresolvable
  user name still degrades to `Unknown`) rather than `resolution: unresolved`,
  which is reserved for a genuinely *anonymous* fn-pointer / `dyn` target
  (`<indirect>`). This keeps the `self-check` functional-core guard green — its
  dogfood run over hinzu-core surfaced `Store::open`'s `impl AsRef<Path>`
  `with_context` closure, whose `path.as_ref()` was a benign, pure, unmonomorphized
  std trait call, not a real effect.
- **Design note: a language-understanding rule layer — a "straitjacket for
  semantics."** `notes/rules-design.md` designs the surface that turns hinzu's
  effect analysis into a general rule engine with straitjacket's ergonomics: a
  rule is a query over the fact database (plus derived facts) that emits findings
  with evidence paths, gates CI on a non-zero exit, and reads its config from a
  `[rules]` section of `hinzu.toml`. Effects become the first rule; three React
  and TypeScript rules are the first peers — `effect-in-component` (a
  component-aware view over the existing effect propagation, superseding
  straitjacket's token-based version by printing the real evidence path to the
  effect), `prop-drilling`, and `one-component-per-file`. The note specifies the
  new adapter facts the rules need (a semantic `is_component` flag, `Render`
  edges for the component tree, and prop-forwarding / prop-usage facts), gives an
  honest per-rule fidelity read — including prop-drilling's real limits (`{...}`
  spread, HOC wrappers, context, computed props) — and sets the rollout after PRs
  #21/#22. Design only; the implementation follows in later pull requests.
- **The reference-level rung of the precision ladder, for Python: a tree-sitter
  syntactic layer resolved through the LSP.** The generic LSP driver builds its
  graph from `callHierarchy/outgoingCalls`, which is call-only — it misses a
  function used as a *value* (`register(fs.read_file)`) and any use at **module
  scope** (SQLAlchemy `Column(...)` / `declarative_base()` in a class body, a
  decorator), which call hierarchy never anchors. A new second fact source
  (`crates/hinzu-lsp/src/treesitter.rs`, over `tree-sitter` +
  `tree-sitter-python`) parses each Python file and enumerates its non-call
  **reference sites** — a name in a value position (call argument, assignment RHS,
  default parameter, `return`, collection element, bare decorator) plus
  module-scope call callees. `extract.rs` resolves each through the *same*
  `textDocument/definition` → provenance → effect path as calls, attributes it to
  the enclosing function (or a synthetic per-file `<module>` definition,
  `<module>@<relpath>`, for import-time / class-body code), and emits a
  `reference` edge. A call's own callee inside a function is not re-emitted (call
  hierarchy already covers it — deduped by position). The rung is
  **sound-additive**: it only adds edges/effects, so no real violation call-only
  found can vanish; what it adds is the higher-order and import-time effects call
  hierarchy missed. `effect_of` now also resolves annotations at the public-API
  package prefix, so an authored `sqlalchemy::create_engine` row matches the
  symbol a type checker resolves to its internal defining module
  (`sqlalchemy.engine.create::create_engine`). On **entl-python** — whose
  SQLAlchemy read-plane is used entirely at module scope — the `db` effect now
  **surfaces** (0 → 3 roots: `create_engine`, `Session.scalar`, `Session.scalars`)
  where call-only saw none, and `entl.models`' module-level construction becomes
  visible/policeable for the first time, closing exactly the loop the SQLAlchemy
  annotation pack flagged as latent. A committed `reference-fixture` (higher-order
  callback reach + module-level SQLAlchemy `db`) gives stable-CI coverage with no
  ty/SQLAlchemy required. Go and the other LSP-tier languages reuse the identical
  rung once their grammar's node/field table is added — a documented follow-up.

- **Shipped Python library annotation pack — the highest-leverage third-party
  packages a fleet sweep surfaced as Unknown, vouched as built-in Python
  defaults.** A new file `crates/hinzu-core/annotations/python-libs.toml` sits
  beside the stdlib set `python.toml` and is merged into the built-in Python
  defaults by both hinzu-core's root seeding and the LSP extractor's effect map —
  one source of truth, no drift — while a project's own `hinzu.toml` `[trust]` /
  `[roots]` still overrides it. It vouches **rich** (terminal presentation) and
  **PyYAML** (`yaml`) pure — an honest caveat records that rich's console output
  is outside hinzu's tracked effect vocabulary (fs/net/db/process/env/clock/
  random), not a claim it is side-effect-free in general — and maps
  **SQLAlchemy**'s engine/session/connection execution surface to `db`, leaving
  the pure declarative/expression construction surface fail-closed rather than
  clearing it with a package-wide vouch. On `housekeeping` the pack clears every
  Unknown "cannot-certify" finding (57 → 0: rich 37, yaml 20) while the set of
  real forbidden-effect violations is unchanged (126 fs/net/process reaches, an
  identical set with identical evidence paths) — no real leak vanished. The
  SQLAlchemy rows are authored correctly now but will not reduce Unknowns until
  the reference-level rung lands, because the call-only extractor emits no edges
  for SQLAlchemy's largely module-level (class-scope) usage.

- **A shipped Rust library annotation pack — the common crates stop coming back
  "cannot certify."** `crates/hinzu-core/annotations/rust-libs.toml` is a built-in
  default, merged onto `std.toml` for the Rust base the same way the Python and
  Node packs work, so a project inherits honest effect labels for the crates the
  fleet reaches most often without writing a `[trust]` line for each one. It
  follows the resolution order already in `roots.rs`: a pure crate is vouched pure
  (`serde`, `serde_json`, `serde_yaml`, `toml`, `regex`, `sha2`, `sha1`, `digest`,
  `anyhow`, `thiserror`, `genco`, `crossbeam_channel`, `itertools`, and the `oxc`
  parser/AST/semantic crates — in-memory compute with no I/O), while a mixed or
  effectful crate is graded at its effect roots and never blanket-pured: `gix` is
  `fs` (its transport and protocol crates `net`), `ignore` is `fs`, `duckdb` and
  `postgres` / `tokio-postgres` are `db` (a `postgres` connect additionally opens
  a socket, so the connect entry points are `net`), the `arrow` file and stream
  codecs (`arrow-ipc`, CSV, JSON) are `fs` while the in-memory columnar surface
  stays pure, `clap`'s argv/env readers (`Parser::parse`, `Command::get_matches`)
  are `env` while its builders and match accessors stay pure, `chrono`'s
  `Utc::now` / `Local::now` are `clock` while its date types stay pure, `uuid`'s
  entropy constructors (`new_v4`, `new_v7`) are `random` while parsing stays pure,
  `tokio`'s `fs` / `net` / `process` / `time` submodules carry their effects while
  the runtime, task, and sync primitives stay pure, and the TLS crates
  (`native_tls`, `rustls`) are `net`. The hard rule throughout: a crate that does
  I/O is never marked pure, because a false "pure" is a silent hole in a
  functional-core gate. A project's own `hinzu.toml` still overrides any of it. On
  the [Straitjacket](https://github.com/zmaril/straitjacket) reference crate under
  a functional-core policy the pack drops the "cannot certify" count sharply while
  leaving the forbidden-effect count unchanged — no real leak appears or vanishes.
  See [`notes/rust-libs-catalog.md`](./notes/rust-libs-catalog.md).

- **A shipped TypeScript / npm library annotation pack
  (`crates/hinzu-core/annotations/node-libs.toml`) — common packages now grade to
  a real effect instead of a bare `Unknown`.** The counterpart to the `node.toml`
  runtime pack, merged onto it as a built-in TypeScript default (a project's own
  `hinzu.toml` still overrides it). It covers the packages the fleet reaches most
  often, graded granularly against the hard rule that nothing doing I/O is ever
  marked pure. drizzle-orm is split at the seam that matters: its query builders
  (`eq`, `and`, `or`, `sql`, `asc`, `relations`, the comparison and aggregate
  helpers) are pure in-memory values, while only its execution surface
  (`.select`, `.from`, `.insert`, `.update`, `.delete`, `.transaction`, `.execute`,
  `.all` / `.run` / `.get`) is `db` — so `eq(users.id, 1)` stops being read as a
  database read. bun-types is split the same way: the `bun:test` API (`expect`,
  `describe`, the `to*` matchers) is pure while `Bun.spawn` / `Bun.spawnSync` is
  `process`, `Bun.file` / `Bun.write` is `fs`, and `Bun.serve` is `net`.
  @electric-sql/pglite is `db`; elysia, @elysiajs/eden, and
  @modelcontextprotocol/sdk are `net`; @disponent/node is `process`. The UI and
  utility packages (react, react-dom, zustand, the xterm and CodeMirror widgets,
  @mantine/core, @dnd-kit/core, ts-pattern, @sinclair/typebox) are vouched pure,
  with one honest caveat: hinzu's vocabulary is fs/net/db/process/env/clock/random,
  so a DOM or render side effect is outside it and is not modeled. See
  [`notes/typescript-catalog.md`](./notes/typescript-catalog.md).

- **Go is a first-class language, over gopls — the proof that a new language is a
  new config, not new extractor code.** `hinzu check` routes a `go.mod` module to
  the same generic Rust LSP extractor Python uses, driving gopls (the Go team's
  language server) as the sole resolution backend. Everything Go lives as data:
  the config `crates/hinzu-lsp/configs/go.toml` (gopls command, `**/*.go` globs,
  and GOROOT + module-cache + downloaded-toolchain provenance rules, robust to
  the plain, versioned, and `setup-go` toolcache GOROOT layouts) plus the shipped
  effect map `crates/hinzu-core/annotations/go.toml`, which the config and
  hinzu-core's own root seeding both read — one source of truth. Go seeds the
  shared vocabulary minus `alloc`: `fs`, `net`, `process`, `env`, `clock`,
  `random`. Provenance is package-granular by import path and does **not** inherit
  to a nested import path (`net/url` is pure, independent of `net` — the opposite
  of Python's dotted-module inheritance); the effect-mixed `os` splits into `fs`
  file operations and `env` accessors, while `io` / `bufio` / `path/filepath` /
  `time` take honest whole-package over-approximations a project can clear with a
  `[trust]` line. `_test.go` files are analyzed; `vendor/` and `testdata/` are
  excluded. Go interface dispatch rides the extractor's existing
  `textDocument/implementation` follow-up (a CHA over-approximation). `HINZU_GOPLS`
  overrides the gopls binary; a missing gopls is an honest nonzero failure, never
  a faked analysis. On [`rs/curlie`](https://github.com/rs/curlie) the extractor
  surfaces the `exec.Command("curl", …)` subprocess spawn with its evidence path
  (`main.go#main -> os/exec::Command`) and fails closed on the third-party
  `golang.org/x/term` / `golang.org/x/sys` console calls it cannot see through. A
  stable-CI test runs Go facts from committed JSON with no toolchain; the isolated
  `go-check` job runs the live gopls path. See
  [`notes/go-catalog.md`](./notes/go-catalog.md). The Go config `stub` that
  shipped with the generic extractor is now the complete, wired config.

- **A generic, all-Rust LSP-driven fact extractor (`crates/hinzu-lsp`) — hinzu's
  new baseline extraction mechanism.** A synchronous Rust LSP client (the port of
  the retired `lspclient.py`) plus a language-agnostic extractor parameterized
  entirely by a per-language config (server command, file globs, the server's
  `initializationOptions`, provenance rules, and the effect map). It drives any
  server that speaks `documentSymbol` + `callHierarchy` and emits hinzu's
  `FactSet` in-process — no per-language parser, no script subprocess, no JSON
  round-trip. The pipeline (ported from the Go/gopls spike): `documentSymbol` →
  definitions; `prepareCallHierarchy` + `callHierarchy/outgoingCalls` →
  caller→callee `call` edges (a local callee mapped by source location); each
  external callee's defining-file uri → provenance → effect, its class-qualified
  name reconstructed from the target file's own `documentSymbol`. Adding a
  language is a new config file plus its provenance/effect rows, not new code — a
  Go config stub ships beside the Python one to keep that seam honest.

### Changed

- **Python is now analyzed all-in-Rust, over ty's LSP** — the out-of-process
  `analyze.py` / `lspclient.py` script adapter is **retired and deleted** (along
  with its `requirements.txt` / `pyproject.toml`). Its AST walk, caller
  attribution, and ty-over-LSP resolution are now the generic Rust extractor
  above, driven by `crates/hinzu-lsp/configs/python.toml` plus the shipped
  `python.toml` effect map (one source of truth). ty (Astral's Rust type checker)
  remains the sole resolution backend — spawned by the Rust client, the only
  non-Rust artifact on the path; a missing `ty` is still an honest nonzero
  failure. `HINZU_TY` overrides the binary, `HINZU_PY_VERSION` pins ty's target
  version (default `3.11`). The real-CPython-stdlib provenance fix and the
  class-qualified symbol reconstruction (`pathlib::Path.is_file`) are ported into
  the config/extractor. On `housekeeping` the new extractor reproduces the **20
  forbidden-effect violations (6 fs, 14 process) exactly, with identical evidence
  paths**; effect roots match but for `os::environ` (an ambient read), and `fs`
  coverage holds at 114 edges. **Honest fidelity note:** the generic extractor is
  **call-only** — `callHierarchy` drops higher-order `reference` edges, ambient
  attribute reads (`os.environ`), and call sites the server could not resolve
  (so `Unknown` findings fall 86 → 41). Those need a body walk, deferred to a
  future language-agnostic tree-sitter rung (also Rust); unknown-by-default over
  resolved calls keeps it sound. The native StableMIR driver stays hinzu's
  Rust-precision path.

- Python adapter — **ty as the sole resolution backend** (no fallback). The
  adapter resolves call sites with **ty** (Astral's Rust type checker), driven
  over its **LSP** (`ty server`, stdio JSON-RPC): it opens every source file,
  settles the first check pass, then pipelines a `textDocument/definition` at each
  callee token and maps the definition target (ty's vendored typeshed, or an
  owned/third-party module) plus the enclosing qualname to a symbol and effect.
  The earlier Jedi fallback is **removed**: ty is the only backend, kept behind
  the `FactSet` seam for a future native in-process ty. If the `ty` binary is
  absent the adapter exits nonzero with an honest message — never a faked or
  weaker resolution. `HINZU_TY` overrides the ty binary path. The AST walk, caller
  attribution, reference edges, and the whole owned/effect/stdlib/third-party
  classification are backend-independent. On `housekeeping`, ty resolves 89.5% of
  call sites, drives `fs`-effect coverage to 117 edges by resolving the un-typed
  `pathlib` chains a name-resolver misses, and keeps the `Unknown` finding pile at
  86 — un-typed `.is_dir()` / `.mkdir()` gaps become precise `forbids fs` findings
  instead of "cannot certify." Unresolved sites still fail closed as `Unknown`
  under `on_unknown = fail`, so precision rises without weakening soundness.
- Python adapter — **recognize the interpreter's real stdlib as a ty definition
  target**, fixing imported-stdlib resolution on headless CI runners. ty resolves
  an imported stdlib symbol to whichever declaration it finds: its VENDORED
  typeshed stub on most hosts, but the interpreter's REAL stdlib source
  (`.../lib/python3.11/subprocess.py`) on a headless GitHub Actions runner, whose
  interpreter ships a full stdlib. The adapter's target-provenance mapping only
  recognized the vendored-typeshed and site-packages paths, so it dropped a
  real-stdlib target as an unknown `OTHER` — turning `subprocess.run` into an
  unresolved `Unknown` while `builtins.open` (a C builtin, always vendored)
  resolved. This looked like ty "returning null for imported-stdlib" but was a
  classification gap: `module_of_target` now recognizes a `.../pythonX.Y/…` stdlib
  path (source or stub, excluding site-packages) as STDLIB. The adapter also pins
  ty's target `python-version`/`python-platform` in the LSP `initialize`
  (`initializationOptions`, `diagnosticMode: workspace`) so the typeshed is
  selected deterministically. This lets the `py-check` CI job run its live fixture
  assertion on **ty** (pinned `ty==0.0.61`), the same backend used locally and in
  real use, and dump ty resolution diagnostics (a `textDocument/definition` probe
  + ty server logs) each run. The stable Rust jobs stay backend-free — their Python
  coverage is the committed sample-facts test, which runs from JSON with no ty. The
  intent remains a native in-process ty backend behind the same `FactSet` seam once
  ty ships a stable Rust library API; pyrefly was evaluated and near-tied but ty was
  chosen (Astral trajectory + native-later intent), and zuban is excluded (AGPL).
  See [`notes/python-catalog.md`](./notes/python-catalog.md).
- Python adapter (slice 3) — `hinzu check <python-project>` now works, through
  the same pipeline as Rust and TypeScript: adapter, SQLite fact store, DBSP
  propagation, `hinzu.toml` policy, violations. The adapter
  (`adapters/python/`) is a name-resolution extractor: the standard-library
  `ast` module walks each file with an enclosing-function stack, and ty (over its
  LSP) resolves each call site's callee, emitting hinzu's `FactSet` JSON — `call`
  and `reference` edges, effect roots seeded by declaration provenance, and, for
  every call site ty cannot resolve, an edge with `resolution: "unresolved"`.
  `hinzu check` routes by project type: a
  `Cargo.toml` takes the Rust StableMIR path, a `tsconfig.json` / `package.json`
  the TypeScript adapter, a `pyproject.toml` / `setup.py` / `setup.cfg` the
  Python adapter (set `HINZU_PY_ADAPTER` / `HINZU_PYTHON` to override). Python
  seeds the shared vocabulary subset `fs`, `net`, `process`, `env`, `clock`,
  `random` — the same names Rust and TypeScript use, no `alloc` for a
  garbage-collected runtime; the bare `pathlib.Path(...)` constructor is pure,
  only its I/O methods are `fs`. Python is still the weakest-resolution adapter —
  an unresolved site becomes an `Unknown` that fails closed under the default
  `on_unknown = fail`, which is what keeps it sound. hinzu ships a built-in Python
  annotation set, `crates/hinzu-core/annotations/python.toml` (the counterpart to
  `std.toml` / `node.toml`). A native in-process ty backend is the planned future
  resolution primitive behind the same `FactSet` seam, once ty ships a stable
  Rust library API. See [`notes/python-catalog.md`](./notes/python-catalog.md).
- TypeScript adapter (slice 2) — `hinzu check <ts-project>` now works, through
  the same pipeline as Rust: adapter, SQLite fact store, DBSP propagation,
  `hinzu.toml` policy, violations. The adapter (`adapters/typescript/`) is a
  native TypeScript compiler-API extractor: it builds a program from the
  project's `tsconfig`, walks each file with an enclosing-function stack, and
  resolves callees with `getResolvedSignature`, emitting hinzu's `FactSet` JSON —
  both `call` and `reference` edges, with effect roots seeded by declaration
  provenance. `hinzu check` routes by project type: a `Cargo.toml` takes the Rust
  StableMIR path, a `tsconfig.json` / `package.json` the TypeScript adapter (set
  `HINZU_TS_ADAPTER` to override its location). Node builtins map to hinzu's one
  flat, shared effect vocabulary — `fs`, `net`, `process`, `env`, `clock`,
  `random`, the same names Rust uses; TypeScript seeds that subset and there is
  deliberately no `alloc` for a garbage-collected runtime. A third-party npm
  package the checker cannot see through is `Unknown` and fails by default, until
  a `[trust]` line vouches for it, the same as Rust. hinzu ships a built-in
  TypeScript annotation set, `crates/hinzu-core/annotations/node.toml` (the
  counterpart to `std.toml`), so `Unknown` classification and `[roots]`/`[trust]`
  overrides work identically for both languages. See
  [`notes/typescript-catalog.md`](./notes/typescript-catalog.md).
- Honest treatment of unseen externals — the `Unknown` marker. A call the
  analyzer cannot see through — a foreign, no-body callee that no rule resolved,
  or an indirect call (function pointer / `dyn`) the driver marked unresolved —
  used to contribute nothing and read as pure. It now becomes `Unknown`, a
  first-class uncertainty that propagates up the call graph like an effect with
  an evidence path down to the offending callee. `hinzu check` fails on
  `Unknown` by default; `[analysis] on_unknown = "fail" | "warn" | "ignore"`
  tunes that (`ignore` restores the old effects-only behavior). The report
  distinguishes an unknown finding ("cannot certify: reaches unknown external
  `serde_json::from_str`") from a forbidden-effect violation.
- Effect-root classification at seed time (`RootSeeds::seed_unknowns`): each
  unseen callee resolves in a fixed order — explicit pure annotation, then an
  effect rule, then a built-in trusted-pure baseline (the standard library and
  calls through a standard-library trait), else `Unknown`. A callee in the
  analyzed workspace's own crates is never `Unknown`, even when a monomorphized
  turbofish makes it differ from its generic definition. Matching is
  segment-aligned (whole `::` path components), so a rule never matches a
  substring of an identifier.
- `[trust]` policy section — trusted external summaries stated outside the
  source. `"serde" = "pure"` vouches a crate effect-free (clearing its
  `Unknown`s); `"rusqlite" = ["db"]` declares the effects a crate carries.
  Merged over hinzu's built-in defaults; an explicit rule overrides.
- `Alloc` effect — heap allocation, tracked like any other effect so a
  performance-sensitive region can forbid it (`forbid = ["alloc"]`). hinzu ships
  its first library annotation set, `crates/hinzu-core/annotations/std.toml`,
  loaded as the built-in default and overridable by `hinzu.toml`: the standard
  library's I/O surface as effect roots, its allocating APIs (`Vec::push`,
  `Box::new`, `String` growth, `format!`, `.collect()`, `Rc`/`Arc::new`, map and
  set inserts) as `alloc` roots, and the genuinely-pure remainder (arithmetic,
  slices, comparisons, lazy iterator adapters) left to the trusted-pure
  baseline. The model is over-approximate: an API that may allocate is marked
  even when a given call does not.
- Self-check tightened: `hinzu-self.toml` now sets `on_unknown = "fail"`, allows
  `alloc` in every region while forbidding the real I/O effects, and carries an
  explicit `[trust]` list (`anyhow`, `toml`, `serde_json` → pure) for the three
  foreign crates hinzu-core reaches that the baseline does not already cover.
  The functional-core guard stays green because that trust list honestly
  accounts for every external, not because the boundary was weakened.
- Configurable effect-root seeding (`hinzu-core::roots`): a prefix table maps a
  callee's path to an effect category, so calls that leave the analyzed
  workspace into a registry dependency become effect roots. A built-in default
  covers the standard library (`std::fs`, `std::net`, `std::process`,
  `std::time`, `std::env`) plus a few common crates — `rand` for randomness and
  `rusqlite` / `libsqlite3_sys` for the database — and a `[roots]` section in
  the policy file extends or overrides it. The match strips generic arguments
  first, so a type such as `rusqlite::Error` inside a `Result` never seeds a
  spurious root. `hinzu check` seeds the fact set before propagation. This is
  what lets the tool see that a program whose I/O is all SQLite is effectful at
  all; a standard-library-only seed found nothing in it.
- Functional-core self-check: `hinzu check` now runs on hinzu-core itself in
  CI, as a regression guard. A dedicated policy (`hinzu-self.toml`) states the
  boundary — the fact schema, the propagation engine, and the policy check must
  reach no filesystem, network, database, subprocess, or environment effect,
  and effects are confined to the SQLite fact store (`store.rs`) and the seam
  that drives it (`check_facts` in `lib.rs`). A new `self-check` CI job builds
  the CLI on stable and the StableMIR driver on its nightly, extracts facts
  from hinzu-core, and fails on any leak. The job is isolated from the stable
  `rust` job: the nightly only ever builds the driver and hinzu-core-under-the-
  driver, never the workspace or its `dbsp` dependency. See
  `notes/self-check.md`.

- Toolchain pin (`rust-toolchain.toml`): the workspace is pinned to stable
  1.96.0. rustc 1.97.x hits an internal compiler error building the `dbsp`
  dependency (a `dyn_clone` vtable-slot panic in the new trait solver); 1.96.0
  compiles it cleanly. The pin can be lifted once the upstream regression is
  fixed. The StableMIR driver keeps its own nightly toolchain file.
- DBSP engine (`hinzu-dbsp`): the `DbspEngine` plugs into the `EffectEngine`
  seam and propagates effects with a recursive DBSP (Feldera) circuit —
  `effect(caller, e) :- edge(caller, callee), effect(callee, e)` over the union
  of call and reference edges, collapsed to set semantics with `.distinct()` so
  the fixed point terminates through call-graph cycles. Each `(function,
  effect)` pair gets an evidence path from a shared breadth-first helper in the
  engine core, so the path logic lives in one place. Unit tests cross-check the
  effect sets against the reference `NaiveEngine` pair for pair.
- StableMIR driver (`hinzu-rustc-driver`, excluded from the workspace default
  members): a `rustc_public` custom rustc driver that walks each monomorphized
  function's MIR, resolves call terminators with `Instance::resolve`, and emits
  JSON facts in hinzu's `FactSet` schema — definitions, call and reference
  edges tagged by resolution, and standard-library effect roots (`std::fs`,
  `std::net`, `std::process`, `std::time`, `std::env`, and random). Indirect
  function-pointer and `dyn` calls are recorded as unresolved rather than
  faked. The crate pins its own nightly and stays off the stable build and CI.
- Rust extraction harness in the CLI (`rust_adapter`): `hinzu check` on a cargo
  project with no `--facts` builds the driver, runs the target's compile with
  the driver set as `RUSTC_WORKSPACE_WRAPPER` (the clippy and miri trick),
  merges the per-crate facts, and ingests them — real extraction replacing the
  earlier stub. A missing nightly or driver fails with an honest message. The
  `--facts` JSON path stays as the offline route.
- `hinzu check --engine dbsp|naive` selects the propagation engine, defaulting
  to `dbsp`; both produce the same effect sets, so a run is reproducible either
  way.
- Slice 1 findings (`notes/slice-1-findings.md`): the first end-to-end run on
  real Rust code. On straitjacket the pipeline extracts 341 functions and 1171
  distinct edges (99.91% statically resolved, one honest unresolved
  function-pointer edge), finds four standard-library effect roots and eight
  transitively effectful functions, and confirms a functional-core policy that
  carves out the IO layer leaves the analysis core with no violations.
- Cargo workspace scaffold: `hinzu-core` (library) and `hinzu-cli` (the
  `hinzu` binary), with an `hinzu run` placeholder command.
- CI (fmt, clippy, test), Dependabot, CODEOWNERS, and the fleet housekeeping,
  Straitjacket, conventional-commits, codespell, and vale workflows.
- Design doc (`notes/design.md`): compiler-facts, a language-independent
  semantic analysis foundation with effect analysis as the first application.
- Getting-started plan (`notes/getting-started.md`): effect propagation as
  hinzu's first slice — per-language fact sources (Rust via `rust-analyzer
  scip`, TypeScript via the compiler API), the fact schema v0, the
  `hinzu.toml` policy shape, and a sliced implementation plan.
- Dataflow design-space survey (`notes/dataflow-survey.md`): the def-use /
  dataflow options across languages (stack-graphs, Semgrep, CodeQL, Glean,
  SCIP, Joern, tree-sitter), ported from the closed straitjacket exploration
  and reframed as provenance for hinzu's adapter layer.
- Getting-started plan update: decisions taken on the adapter forks — native
  compiler-API adapters for both languages (a StableMIR/`rustc_public` driver
  for Rust, the TypeScript compiler API for TypeScript, not SCIP), a SQLite
  fact database from day one, and reference + call edge granularity with a
  documented value-flow / effect-polymorphic precision ladder. Adds an
  empirical-validation section from a real run of the native TypeScript
  extractor against pi.dev (earendil-works/pi).
- Getting-started engine-stack decision: hinzu is all-in on DBSP (Feldera) as
  the single analysis engine, with SQLite as the durable fact store; `ascent`
  and Cozo were evaluated and dropped (DBSP covers batch and incremental in one
  engine, and Cozo is stale). Adds two validation spikes: a StableMIR
  (`rustc_public`) driver run over straitjacket (341 functions, 1,912 call
  edges, 99.95% statically resolved), and a DBSP incrementality benchmark on
  the pi facts (batch answer set-equal to a reference BFS, then
  diff-proportional recompute with exact retraction).
- Effect-propagation prototype behind `hinzu run`: a fact schema
  (`facts.rs`), a fixed-point propagation engine over the reverse call graph
  (`effects.rs`), and a region-based policy check (`policy.rs`), exercised on
  a synthetic functional-core violation with an evidence path.
- SQLite fact store (`store.rs`, bundled `rusqlite`): the durable source of
  truth for definitions, edges, effect roots, and derived effect summaries.
  Edges now carry a `resolution` provenance field (`call`, `reference`,
  `value-flow`, or `unresolved`) for the precision ladder. The fact types
  serialize to and from a JSON schema.
- Engine seam: an `EffectEngine` trait with the breadth-first `NaiveEngine` as
  its first implementation, so the incremental DBSP engine can plug in behind
  the same interface in a later phase.
- Policy parser: `hinzu.toml` is read into the region model with real glob
  matching for paths, an `ignore` list, and `allow`/`forbid` region rules. A
  file is governed by its most-specific matching region, so a nested adapters
  carve-out overrides the broader core. A worked `hinzu.toml` at the repo root
  states hinzu's own functional-core policy.
- `hinzu check <path>` command: ingests pre-extracted facts (`--facts <json>`)
  into the store, propagates effects, persists the summaries, checks them
  against a policy (`--policy`, default `hinzu.toml`), and reports every
  violation with its callable, forbidden effect, region, and evidence path,
  exiting non-zero when any are found. Without facts on a Rust project it
  reports that the StableMIR driver is not wired yet and exits non-zero rather
  than faking an analysis.
