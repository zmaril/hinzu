//! The TypeScript extraction harness: drive the compiler-API adapter over a
//! target TypeScript project and read back the effect facts it emits.
//!
//! Unlike the Rust path (a rustc driver linked to a pinned nightly), the
//! TypeScript adapter is a plain Node program under `adapters/typescript/`. This
//! module shells out to `node analyze.mjs <project>`, captures the `FactSet`
//! JSON it writes to stdout, and parses it. When Node or the adapter is
//! unavailable it fails with an honest message rather than faking an analysis —
//! the same honest-capability-edge discipline as the Rust harness.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use hinzu_core::facts::FactSet;

/// Whether `path` looks like a TypeScript project: it has a `tsconfig.json` or a
/// `package.json` the adapter can build a program from.
pub fn is_ts_project(path: &Path) -> bool {
    path.join("tsconfig.json").is_file() || path.join("package.json").is_file()
}

/// Extract effect facts from a TypeScript project by running the compiler-API
/// adapter over it. Returns the parsed `FactSet`, or an honest error when Node
/// or the adapter is missing.
pub fn extract_facts(project: &Path) -> Result<FactSet> {
    let node = node_binary()?;
    let adapter = adapter_entry()?;

    // Run with the project as the working directory so TypeScript resolves the
    // project's own `node_modules` (its `@types/node` and dependencies) the way
    // its own `tsc` would, independent of where `hinzu` was invoked from.
    let project = project
        .canonicalize()
        .with_context(|| format!("resolving project path {}", project.display()))?;
    let output = Command::new(&node)
        .arg(&adapter)
        .arg(&project)
        .current_dir(&project)
        .output()
        .with_context(|| {
            format!(
                "running the TypeScript adapter: {} {}",
                node,
                adapter.display()
            )
        })?;

    // The adapter logs progress to stderr and writes only JSON to stdout, so a
    // failure surfaces the adapter's own diagnostics.
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "the TypeScript adapter failed for {}:\n{}",
            project.display(),
            stderr.trim()
        );
    }

    let json = String::from_utf8(output.stdout).context("the adapter's output was not utf-8")?;
    let facts = FactSet::from_json(&json).with_context(|| {
        format!(
            "parsing the TypeScript adapter's FactSet JSON for {}",
            project.display()
        )
    })?;

    if facts.defs.is_empty() && facts.edges.is_empty() {
        bail!(
            "the TypeScript adapter produced no facts for {} — is it a TypeScript project with a \
             tsconfig?",
            project.display()
        );
    }
    Ok(facts)
}

/// Locate the `node` executable. Honors a `HINZU_NODE` override; otherwise trusts
/// `node` on `PATH`. The actual availability check is the extraction run itself,
/// which reports Node's own error if it is missing.
fn node_binary() -> Result<String> {
    Ok(std::env::var("HINZU_NODE").unwrap_or_else(|_| "node".to_string()))
}

/// Locate the adapter entry script (`analyze.mjs`): an explicit
/// `HINZU_TS_ADAPTER` override, else the in-tree copy under
/// `adapters/typescript/`. Fails honestly when neither is present.
fn adapter_entry() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("HINZU_TS_ADAPTER") {
        let path = PathBuf::from(path);
        if !path.is_file() {
            bail!("HINZU_TS_ADAPTER={} is not a file", path.display());
        }
        return Ok(path);
    }
    let in_tree = adapter_dir().join("analyze.mjs");
    if in_tree.is_file() {
        return Ok(in_tree);
    }
    bail!(
        "the TypeScript adapter was not found at {} — set HINZU_TS_ADAPTER to analyze.mjs, and run \
         `npm install` in adapters/typescript so its `typescript` dependency is present",
        in_tree.display()
    )
}

/// The in-tree adapter directory (`adapters/typescript/`), relative to this
/// crate's manifest.
fn adapter_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("hinzu-cli sits under crates/ in the repo root")
        .join("adapters/typescript")
}
