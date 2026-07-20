//! The TypeScript extraction harness: locate the compiler-API adapter and Node,
//! then drive it over a target TypeScript project through the shared
//! script-adapter runner ([`crate::adapter_harness`]).
//!
//! Unlike the Rust path (a rustc driver linked to a pinned nightly), the
//! TypeScript adapter is a plain Node program under `adapters/typescript/`. The
//! shared runner shells out to `node analyze.mjs <project>`, captures the
//! `FactSet` JSON it writes to stdout, and parses it. When Node or the adapter is
//! unavailable it fails with an honest message rather than faking an analysis —
//! the same honest-capability-edge discipline as the Rust harness.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use hinzu_core::facts::FactSet;
use hinzu_core::similarity::SignatureDoc;

use crate::adapter_harness::{locate_script, ScriptAdapter};

/// Whether `path` looks like a TypeScript project: it has a `tsconfig.json` or a
/// `package.json` the adapter can build a program from.
pub fn is_ts_project(path: &Path) -> bool {
    path.join("tsconfig.json").is_file() || path.join("package.json").is_file()
}

/// Locate the TypeScript adapter and build the [`ScriptAdapter`] that drives it.
/// `HINZU_NODE` overrides the interpreter (default `node`); `HINZU_TS_ADAPTER`
/// overrides the script. Shared by the fact path ([`extract_facts`]) and the
/// public-API path (`api_ts`), so both spawn the same Node adapter identically.
pub fn ts_script_adapter() -> Result<ScriptAdapter> {
    let script = locate_script(
        "HINZU_TS_ADAPTER",
        "typescript",
        "analyze.mjs",
        "set HINZU_TS_ADAPTER to analyze.mjs, and run `npm install` in adapters/typescript so its \
         `typescript` dependency is present",
    )?;
    Ok(ScriptAdapter {
        language: "TypeScript",
        binary: std::env::var("HINZU_NODE").unwrap_or_else(|_| "node".to_string()),
        script,
        // Run with the project as the working directory so TypeScript resolves
        // the project's own `node_modules` (its `@types/node` and dependencies)
        // the way its own `tsc` would, independent of where `hinzu` ran from.
        cwd_is_project: true,
    })
}

/// The TypeScript project roots to run the structural extractor over for a
/// target. If `path` is itself a TypeScript project, that is the single root.
/// Otherwise — a mixed repo whose top level is (say) a cargo workspace — scan a
/// bounded depth for nested `tsconfig.json` directories, so a TypeScript
/// sub-project inside a larger repo is still analyzed. Dependency, build, and VCS
/// directories are skipped.
pub fn find_ts_projects(path: &Path) -> Vec<PathBuf> {
    if is_ts_project(path) {
        return vec![path.to_path_buf()];
    }
    let mut out = Vec::new();
    scan_for_tsconfig(path, 0, &mut out);
    out.sort();
    out.dedup();
    out
}

/// A bounded recursive scan for directories containing a `tsconfig.json`,
/// skipping the usual non-source directories. Depth is capped so the scan stays
/// cheap on a large repo.
fn scan_for_tsconfig(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 3 {
        return;
    }
    if dir.join("tsconfig.json").is_file() {
        out.push(dir.to_path_buf());
        // Do not descend into a project's own nested configs; one program per
        // project root is enough.
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if matches!(
            name.as_str(),
            "node_modules" | "target" | "dist" | "build" | "out" | "coverage" | ".git"
        ) || name.starts_with('.')
        {
            continue;
        }
        scan_for_tsconfig(&p, depth + 1, out);
    }
}

/// Extract structural signatures from a TypeScript project by running the
/// compiler-API structural extractor (`structural.mjs`) over it. Returns the
/// parsed [`SignatureDoc`] (stamped `typescript` / `tsc-checker`), or an honest
/// error when Node or the adapter is missing. `HINZU_NODE` overrides the
/// interpreter (default `node`); `HINZU_TS_STRUCTURAL` overrides the script.
pub fn extract_structural(project: &Path) -> Result<SignatureDoc> {
    let script = locate_script(
        "HINZU_TS_STRUCTURAL",
        "typescript",
        "structural.mjs",
        "set HINZU_TS_STRUCTURAL to structural.mjs, and run `npm install` in adapters/typescript \
         so its `typescript` dependency is present",
    )?;
    let binary = std::env::var("HINZU_NODE").unwrap_or_else(|_| "node".to_string());
    let project = project
        .canonicalize()
        .with_context(|| format!("resolving project path {}", project.display()))?;

    // Run with the project as the working directory, so `tsc` resolves the
    // project's own `node_modules` the way its own `tsc` would.
    let output = Command::new(&binary)
        .arg(&script)
        .arg(&project)
        .current_dir(&project)
        .output()
        .with_context(|| {
            format!(
                "running the TypeScript structural extractor: {} {}",
                binary,
                script.display()
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "the TypeScript structural extractor failed for {}:\n{}",
            project.display(),
            stderr.trim()
        );
    }

    let json = String::from_utf8(output.stdout)
        .context("the TypeScript structural extractor's output was not utf-8")?;
    serde_json::from_str(&json).with_context(|| {
        format!(
            "parsing the TypeScript structural extractor's SignatureDoc JSON for {}",
            project.display()
        )
    })
}

/// Extract effect facts from a TypeScript project by running the compiler-API
/// adapter over it. Returns the parsed `FactSet`, or an honest error when Node
/// or the adapter is missing.
pub fn extract_facts(project: &Path) -> Result<FactSet> {
    ts_script_adapter()?.extract(project)
}
