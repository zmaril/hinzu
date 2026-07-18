//! The shared out-of-process adapter harness. The TypeScript and Python adapters
//! are both external scripts (`node analyze.mjs`, `python3 analyze.py`) that read
//! a project and write hinzu's `FactSet` JSON to stdout, logging to stderr. Only
//! the Rust path differs (a linked StableMIR driver). This module holds the one
//! shell-out-capture-parse body they share, so `ts_adapter` and `py_adapter` keep
//! just their own project detection and binary/script location — the parts that
//! genuinely differ per language.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use hinzu_core::facts::FactSet;

/// Locate an adapter's entry script: an explicit environment override (which
/// must point at a real file), else the in-tree copy at `adapters/<subdir>/
/// <script>` relative to this crate's manifest. `missing_hint` completes the
/// honest error when neither is present. Shared by the TypeScript and Python
/// harnesses, whose only differences here are the names.
pub fn locate_script(
    env_var: &str,
    subdir: &str,
    script: &str,
    missing_hint: &str,
) -> Result<PathBuf> {
    if let Ok(path) = std::env::var(env_var) {
        let path = PathBuf::from(path);
        if !path.is_file() {
            bail!("{env_var}={} is not a file", path.display());
        }
        return Ok(path);
    }
    let in_tree = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("hinzu-cli sits under crates/ in the repo root")
        .join("adapters")
        .join(subdir)
        .join(script);
    if in_tree.is_file() {
        return Ok(in_tree);
    }
    bail!(
        "the adapter was not found at {} — {missing_hint}",
        in_tree.display()
    )
}

/// How to run one external script adapter over a project.
pub struct ScriptAdapter {
    /// The language name, for error messages ("TypeScript", "Python").
    pub language: &'static str,
    /// The interpreter binary (`node`, `python3`).
    pub binary: String,
    /// The adapter entry script (`analyze.mjs`, `analyze.py`).
    pub script: PathBuf,
    /// Run with the project as the working directory. TypeScript needs this so
    /// `tsc` resolves the project's own `node_modules`; Python does not.
    pub cwd_is_project: bool,
}

impl ScriptAdapter {
    /// Run the adapter over `project`, capture its stdout, and parse the
    /// `FactSet` JSON. Fails with an honest message — surfacing the adapter's own
    /// stderr — rather than faking an analysis when the tool or a dependency is
    /// missing.
    pub fn extract(&self, project: &Path) -> Result<FactSet> {
        let project = project
            .canonicalize()
            .with_context(|| format!("resolving project path {}", project.display()))?;

        let mut cmd = Command::new(&self.binary);
        cmd.arg(&self.script).arg(&project);
        if self.cwd_is_project {
            cmd.current_dir(&project);
        }
        let output = cmd.output().with_context(|| {
            format!(
                "running the {} adapter: {} {}",
                self.language,
                self.binary,
                self.script.display()
            )
        })?;

        // The adapter logs progress to stderr and writes only JSON to stdout, so
        // a failure surfaces the adapter's own diagnostics.
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "the {} adapter failed for {}:\n{}",
                self.language,
                project.display(),
                stderr.trim()
            );
        }

        let json =
            String::from_utf8(output.stdout).context("the adapter's output was not utf-8")?;
        let facts = FactSet::from_json(&json).with_context(|| {
            format!(
                "parsing the {} adapter's FactSet JSON for {}",
                self.language,
                project.display()
            )
        })?;

        if facts.defs.is_empty() && facts.edges.is_empty() {
            bail!(
                "the {} adapter produced no facts for {} — is it a {} project with source files?",
                self.language,
                project.display(),
                self.language,
            );
        }
        Ok(facts)
    }
}
