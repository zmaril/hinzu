# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
