//! The out-of-process script-adapter harness for the TypeScript path. The
//! TypeScript adapter is an external Node script (`node analyze.mjs`) that reads a
//! project and writes hinzu's `FactSet` JSON to stdout, logging to stderr. This
//! module holds the shell-out-capture-parse body it uses.
//!
//! The Rust (StableMIR driver) and Python (in-process `hinzu-lsp` generic LSP
//! extractor) paths do not use this harness — Rust is a linked driver, and Python
//! is now driven all-in-Rust over ty's LSP, no script subprocess.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use hinzu_core::facts::FactSet;

/// Locate an adapter's entry script: an explicit environment override (which
/// must point at a real file), else the in-tree copy at `adapters/<subdir>/
/// <script>` relative to this crate's manifest. `missing_hint` completes the
/// honest error when neither is present. Used by the TypeScript harness (the
/// only script adapter left).
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
    /// The language name, for error messages ("TypeScript").
    pub language: &'static str,
    /// The interpreter binary (`node`).
    pub binary: String,
    /// The adapter entry script (`analyze.mjs`).
    pub script: PathBuf,
    /// Run with the project as the working directory, so `tsc` resolves the
    /// project's own `node_modules`.
    pub cwd_is_project: bool,
}

impl ScriptAdapter {
    /// Run the adapter over `project` with `extra_args`, capture its stdout, and
    /// return it as a string. Fails with an honest message — surfacing the
    /// adapter's own stderr — rather than faking an analysis when the tool or a
    /// dependency is missing. The adapter logs progress to stderr and writes only
    /// JSON to stdout, so a failure surfaces the adapter's own diagnostics.
    /// Shared by the fact ([`Self::extract`]) and API modes.
    pub fn run_capture(&self, project: &Path, extra_args: &[&str]) -> Result<String> {
        let project = project
            .canonicalize()
            .with_context(|| format!("resolving project path {}", project.display()))?;

        let mut cmd = Command::new(&self.binary);
        cmd.arg(&self.script).arg(&project).args(extra_args);
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

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "the {} adapter failed for {}:\n{}",
                self.language,
                project.display(),
                stderr.trim()
            );
        }

        String::from_utf8(output.stdout).context("the adapter's output was not utf-8")
    }

    /// Run the adapter over `project` in fact mode and parse the `FactSet` JSON.
    pub fn extract(&self, project: &Path) -> Result<FactSet> {
        let json = self.run_capture(project, &[])?;
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
