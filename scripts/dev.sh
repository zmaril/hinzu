#!/usr/bin/env bash
# Stand up the hinzu dev environment: format-check, lint, and test the Rust
# workspace — the same three gates CI runs. Safe to run from anywhere.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "checking formatting…"
cargo fmt --all --check

echo
echo "linting…"
cargo clippy --all-targets -- -D warnings

echo
echo "testing…"
cargo test

echo
echo "dev environment ready:"
echo "  cargo test                     # the workspace tests"
echo "  cargo run -p hinzu-cli -- run # the hinzu CLI"
