<!-- housekeeper:description A Rust CLI, laid out as a Cargo workspace with a core engine crate and a thin command-line shell -->
<!-- housekeeper:topics cargo-workspace, cli, command-line-tool, rust -->
# hinzu

hinzu is a language-independent analysis layer that sits above existing
compilers and type checkers. It doesn't parse source or add a new language —
it consumes the semantic information a compiler has already computed (which
function a call resolves to, what type an expression has, which symbols are
mutated, which values escape) and normalizes it into a common set of facts.
Whole-program analyses then run over those facts instead of over syntax.

The first analysis is **effect analysis**: finding where a program observes or
changes the outside world — filesystem, network, databases, clocks, randomness,
processes — and enforcing architectural boundaries such as a pure functional
core wrapped in an imperative shell. Because the reasoning works on normalized
facts rather than any one language's syntax, the same engine can serve many
languages, and further analyses (purity, capabilities, taint, dependency and
ownership analysis) reuse the same foundation. Every conclusion keeps its
evidence, so a reported effect can be traced back through the call path to the
operation that caused it.

The guiding idea: compilers already hold the deepest understanding of their
languages, so hinzu preserves that knowledge after compilation and makes it
reusable, rather than reconstructing approximations from source. See
[notes/design.md](notes/design.md) for the full design and philosophy.

> **Status: early scaffolding.** The Cargo workspace, CLI, and CI are in place;
> the CLI exposes a single `run` placeholder while the analysis surface
> described in the design doc is built out. New functionality slots into an
> established shape rather than a blank repo.

## Layout

The workspace splits into two crates:

- **`hinzu-core`** — the library: fact extraction, the normalized fact
  database, and the analysis engines. All the real work lives here so it stays
  testable without going through argv.
- **`hinzu-cli`** — a thin shell that parses arguments (with
  [clap](https://docs.rs/clap)) and hands off to `hinzu-core`. It builds the
  `hinzu` binary.

## Install

Build from a checkout with a recent stable Rust toolchain:

```sh
git clone https://github.com/zmaril/hinzu
cd hinzu
cargo build --release
```

The binary lands at `target/release/hinzu`. To install it onto your `PATH`:

```sh
cargo install --path crates/hinzu-cli
```

## Usage

```sh
hinzu run        # run the engine (placeholder for now)
hinzu check <p>  # check a project's effects against a hinzu.toml policy
hinzu graph <p>  # emit a JSON dependency graph for AI-assisted porting
hinzu plan <p>   # emit a grouped, wave-ordered porting plan
hinzu port-diff  # cross-language port-progress diff (source graph vs target port)
hinzu --help     # list commands
hinzu --version  # print the version
```

## Porting a codebase, in dependency order

`graph`, `plan`, and `port-diff` form a pipeline for porting a codebase to
another language or framework with AI agents. The point is to work in dependency
order — **leaves first** — so that whenever an agent ports a symbol everything
that symbol depends on already exists and is testable, instead of shotgunning
files and stitching them back together afterward. All three reuse the exact facts
the effect engine consumes.

`hinzu graph <dir>` emits the dependency graph: **call** edges at the symbol
level and **module-dependency** edges at the file level. Real code has cycles
(mutual recursion, back-and-forth calls between modules), so the graph is **not**
acyclic in general — calling it a "DAG" would be a lie the moment the code has a
cycle. What *is* acyclic is its **condensation**: collapse each
strongly-connected component to a single node and a dependencies-first
topological order becomes well-defined. That acyclic view, and the port-order
utilities built on it, live in the `condensation` field.

`hinzu plan <dir>` turns that graph into an operational schedule: **waves**
(topological layers with no dependency between the groups in a wave, so an
orchestrator can port a whole wave in parallel) over **groups** (a PR / an agent
thread per group — a dependency cycle is collapsed into one mandatory unit, and
small tightly-coupled files are coalesced so there isn't a thread per one-liner).
`--from <entry>` scopes the plan to the transitive dependency closure of an entry
point — exactly what that one entry needs, in port order, and nothing else.

`hinzu port-diff --config <toml> --package <p>` measures how far a port has
actually gotten. It matches a source package's graph + plan against the target
port's graph by **symbol-graph structure** (so it survives file rename and
decomposition), bands every source file **DONE / PORTED / STARTED / NOT-STARTED**,
and emits a ready-frontier — unported files whose source-dependencies are all
ported — plus, with `--html`, a self-contained dashboard. It is config-driven:
one toml describes several packages under a shared naming ruleset. `--all` sweeps
**every** package into one combined rollup JSON + dashboard (with `--cache-dir`
to make the repeated extraction reusable); `--from` scopes a single `--package`
to one entry point's closure (a rooted view is single-package, so it is not
combined with `--all`).

**Fidelity, honestly.** The graph is **call-only** — it misses higher-order and
dynamic dispatch, and file edges are *inferred* from call edges (there is no
imports / implementation table). port-diff's STARTED and PORTED bands are
structural too: a name-and-structure match, which call-edge overlap annotates for
confidence but never fabricates. Only **DONE** is cross-checked against a
conformance oracle — and on the `pi` → `atilla` port it holds exactly: the DONE
band equals the target's per-package conformance-native count.

See [notes/graph.md](notes/graph.md), [notes/plan.md](notes/plan.md), and
[notes/port-diff.md](notes/port-diff.md) for the JSON schemas, the ordering and
wave semantics, `--from` closure scoping, and the band definitions.

## Development

```sh
scripts/dev.sh            # format-check + lint + test, the way CI does
```

Or run the gates individually:

```sh
cargo fmt --all           # format
cargo clippy --all-targets -- -D warnings  # lint
cargo test                # run the tests
```

CI runs the same three on every push and pull request, alongside the fleet
housekeeping, Straitjacket, codespell, and vale checks.

## Contributing

Pull request titles follow
[Conventional Commits](https://www.conventionalcommits.org)
(`type(scope): summary`) — CI enforces it. Keep `cargo fmt`, `cargo clippy`,
and `cargo test` green before opening a PR.

## License

[MIT](LICENSE) © Zack Maril
