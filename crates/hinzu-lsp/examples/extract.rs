//! A tiny diagnostic driver for the Python path of the generic LSP extractor:
//! drive ty over `<project>` and print the extracted `FactSet` JSON to stdout
//! (the extractor logs a one-line summary to stderr). Used by CI's `py-check`
//! diagnostics as an all-Rust dry run, and handy locally:
//!
//! ```sh
//! cargo run -p hinzu-lsp --example extract -- path/to/python/project
//! ```

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: extract <python-project>"))?;
    let facts = hinzu_lsp::extract_python(&PathBuf::from(path))?;
    println!("{}", serde_json::to_string_pretty(&facts)?);
    Ok(())
}
