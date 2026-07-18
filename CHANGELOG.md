# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
