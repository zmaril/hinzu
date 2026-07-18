# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
