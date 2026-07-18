# hinzu

A Rust command-line tool, laid out as a Cargo workspace so the engine and the
shell stay separate.

The workspace splits into two crates:

- **`hinzu-core`** — the library. All the real work lives here so it stays
  testable without going through argv.
- **`hinzu-cli`** — a thin shell that parses arguments (with
  [clap](https://docs.rs/clap)) and hands off to `hinzu-core`. It builds the
  `hinzu` binary.

This is early scaffolding: the CLI exposes a single `run` placeholder command
while the actual surface is designed. Everything below already works, so new
functionality slots into an established shape rather than a blank repo.

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
hinzu run       # run the engine (placeholder for now)
hinzu --help    # list commands
hinzu --version # print the version
```

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
