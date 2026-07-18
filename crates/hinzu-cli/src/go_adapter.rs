//! The Go extraction path: drive the generic Rust LSP extractor
//! ([`hinzu_lsp`]) over a target Go module with gopls (the Go team's language
//! server) as the resolution backend, entirely in-process — no per-language
//! parser, no script subprocess, no JSON round-trip.
//!
//! Go rides the same language-agnostic extractor as Python; the only
//! Go-specific artifacts are the config (`crates/hinzu-lsp/configs/go.toml` —
//! gopls command, `**/*.go` globs, GOROOT + module-cache provenance) and the
//! shipped `go.toml` effect map. When the `gopls` binary is unavailable the run
//! fails with an honest message rather than faking an analysis — the same
//! honest-capability-edge discipline as the Rust, TypeScript, and Python
//! harnesses. `HINZU_GOPLS` overrides the `gopls` binary path.
//!
//! gopls typechecks the module to resolve calls into dependencies, so a module
//! with external dependencies needs them present in the module cache. This
//! adapter runs `go mod download` best-effort first (a stdlib-only module needs
//! nothing fetched, so a failure there is a note, not a hard error); the real
//! capability edge is gopls itself.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use hinzu_core::facts::FactSet;

/// Whether `path` looks like a Go module: it has a `go.mod` the adapter can
/// analyze. Go's module marker, the counterpart to `pyproject.toml` for Python
/// or `Cargo.toml` for Rust.
pub fn is_go_project(path: &Path) -> bool {
    path.join("go.mod").is_file()
}

/// Extract effect facts from a Go module by driving gopls over its LSP with the
/// in-process Rust extractor. Returns the parsed `FactSet`, or an honest error
/// when the `gopls` binary is missing.
pub fn extract_facts(project: &Path) -> Result<FactSet> {
    // gopls resolves calls into dependencies only when they are in the module
    // cache, so prime it. Best-effort: a stdlib-only module (the fixture) needs
    // nothing, and an offline run may still resolve from cache — so a failure
    // here is a note on stderr, not a hard stop. The honest capability edge is
    // gopls, checked inside `extract_go`.
    prime_module_cache(project);

    let facts = hinzu_lsp::extract_go(project)
        .with_context(|| format!("extracting Go facts from {}", project.display()))?;

    if facts.defs.is_empty() && facts.edges.is_empty() {
        anyhow::bail!(
            "the Go adapter produced no facts for {} — is it a Go module with source files \
             gopls can resolve? (a build error in the module can stop gopls from indexing it)",
            project.display()
        );
    }
    Ok(facts)
}

/// Run `go mod download` in the module directory, best-effort. Prints a note to
/// stderr when `go` is absent or the download fails, but does not fail the run —
/// a stdlib-only module resolves without it.
fn prime_module_cache(project: &Path) {
    match Command::new("go")
        .arg("mod")
        .arg("download")
        .current_dir(project)
        .output()
    {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr);
            eprintln!(
                "hinzu: `go mod download` reported an issue (continuing; stdlib still \
                 resolves):\n{}",
                err.trim()
            );
        }
        Err(e) => {
            eprintln!("hinzu: could not run `go mod download` (continuing): {e}");
        }
    }
}
