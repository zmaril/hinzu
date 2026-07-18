//! The Python extraction path: drive the generic Rust LSP extractor
//! ([`hinzu_lsp`]) over a target Python project with ty (Astral's type checker)
//! as the resolution backend, entirely in-process — no Python, no script
//! subprocess, no JSON round-trip.
//!
//! This replaces the old out-of-process `python3 analyze.py` plumbing: the AST
//! walk, the caller attribution, and the ty-over-LSP resolution the script did
//! are now the language-agnostic Rust extractor driven by
//! `crates/hinzu-lsp/configs/python.toml` (server command, file globs, ty init
//! options, provenance rules) plus the shipped `python.toml` effect map. When the
//! `ty` binary is unavailable the run fails with an honest message rather than
//! faking an analysis — the same honest-capability-edge discipline as the Rust
//! and TypeScript harnesses. `HINZU_TY` overrides the `ty` binary path;
//! `HINZU_PY_VERSION` pins ty's target Python version (default `3.11`).

use std::path::Path;

use anyhow::{Context, Result};
use hinzu_core::facts::FactSet;

/// Whether `path` looks like a Python project: it has a `pyproject.toml`,
/// `setup.py`, or `setup.cfg` the adapter can analyze.
pub fn is_python_project(path: &Path) -> bool {
    path.join("pyproject.toml").is_file()
        || path.join("setup.py").is_file()
        || path.join("setup.cfg").is_file()
}

/// Extract effect facts from a Python project by driving ty over its LSP with the
/// in-process Rust extractor. Returns the parsed `FactSet`, or an honest error
/// when the `ty` binary is missing.
pub fn extract_facts(project: &Path) -> Result<FactSet> {
    let facts = hinzu_lsp::extract_python(project)
        .with_context(|| format!("extracting Python facts from {}", project.display()))?;

    if facts.defs.is_empty() && facts.edges.is_empty() {
        anyhow::bail!(
            "the Python adapter produced no facts for {} — is it a Python project with source \
             files ty can resolve?",
            project.display()
        );
    }
    Ok(facts)
}
