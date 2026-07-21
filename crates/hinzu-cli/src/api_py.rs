//! The Python public-API extraction path: drive ty over its LSP (via the
//! in-process `hinzu-lsp` extractor) and lower the result into hinzu's
//! language-agnostic [`ApiReport`].
//!
//! Thin by design, exactly like the TypeScript seam: `hinzu_lsp::
//! extract_python_api` produces the `{package, fidelity, modules}` pieces, and
//! this path hands them to the pure [`hinzu_core::api::build_api`] for
//! normalization. All process/fs effects (the ty subprocess, file reads, LSP
//! round-trips) live in `hinzu-lsp` — never in hinzu-core. Python fidelity is
//! the weakest of the three languages; the honest limits ride in the report's
//! [`hinzu_core::api::Fidelity`] notes rather than being papered over here.

use std::path::Path;

use anyhow::{Context, Result};
use hinzu_core::api::{build_api, ApiReport};

/// Extract the public API of a Python package by driving ty over its LSP.
/// `root_label` overrides the report's `package.root` with the target as the
/// operator named it (matching the Rust and TypeScript paths).
pub fn extract(project: &Path, root_label: &str) -> Result<ApiReport> {
    let py = hinzu_lsp::extract_python_api(project, root_label)
        .with_context(|| format!("extracting the Python API of {}", project.display()))?;
    Ok(build_api(py.package, py.fidelity, py.modules))
}
