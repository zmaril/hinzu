//! A tiny diagnostic driver for the generic LSP extractor: drive the right
//! backend over `<project>` and print the extracted `FactSet` JSON to stdout
//! (the extractor logs a one-line summary to stderr). It routes by project
//! marker — a `go.mod` drives gopls, otherwise ty (Python). Used by CI's
//! `py-check` diagnostics as an all-Rust dry run, and handy locally, e.g. to
//! regenerate a committed `sample-facts.json`:
//!
//! ```sh
//! cargo run -p hinzu-lsp --example extract -- path/to/project
//! ```

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: extract <project>"))?;
    let project = PathBuf::from(path);
    let facts = if project.join("go.mod").is_file() {
        hinzu_lsp::extract_go(&project)?
    } else {
        hinzu_lsp::extract_python(&project)?
    };
    println!("{}", serde_json::to_string_pretty(&facts)?);
    Ok(())
}
