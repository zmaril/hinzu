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

use std::path::Path;

use anyhow::Result;
use hinzu_core::facts::FactSet;

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

/// Extract effect facts from a TypeScript project by running the compiler-API
/// adapter over it. Returns the parsed `FactSet`, or an honest error when Node
/// or the adapter is missing.
pub fn extract_facts(project: &Path) -> Result<FactSet> {
    ts_script_adapter()?.extract(project)
}
