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

`hinzu graph` emits a dependencies-first (leaves-first) port order over the
call/use graph — the same facts the effect engine consumes, reused to answer
"in what order should an agent move this code?" The graph may contain cycles
(mutual recursion); the acyclic SCC-condensation is what makes the order
well-defined. See [notes/graph.md](notes/graph.md) for the JSON schema, the
ordering semantics, and how a porting agent walks it.

`hinzu port-diff` closes the loop: given a source package's graph + plan and a
target port's graph, it reports — file by file, symbol by symbol — how much has
actually been ported, in a way that survives file decomposition and relocation.
It is config-driven (one toml describes several packages) and emits a JSON report
plus an optional self-contained HTML dashboard. See
[notes/port-diff.md](notes/port-diff.md) for the config schema, the input modes,
`--from` closure scoping, and the band definitions.

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
