//! The Python extraction harness: locate the Jedi adapter and its interpreter,
//! then drive it over a target Python project through the shared script-adapter
//! runner ([`crate::adapter_harness`]). When Python or Jedi is unavailable the
//! run fails with an honest message rather than faking an analysis — the same
//! honest-capability-edge discipline as the Rust and TypeScript harnesses.

use std::path::Path;

use anyhow::Result;
use hinzu_core::facts::FactSet;

use crate::adapter_harness::{locate_script, ScriptAdapter};

/// Whether `path` looks like a Python project: it has a `pyproject.toml`,
/// `setup.py`, or `setup.cfg` the adapter can analyze.
pub fn is_python_project(path: &Path) -> bool {
    path.join("pyproject.toml").is_file()
        || path.join("setup.py").is_file()
        || path.join("setup.cfg").is_file()
}

/// Extract effect facts from a Python project by running the Jedi adapter over
/// it. Returns the parsed `FactSet`, or an honest error when Python or the
/// adapter (or its `jedi` dependency) is missing. `HINZU_PYTHON` overrides the
/// interpreter (default `python3`); `HINZU_PY_ADAPTER` overrides the script.
pub fn extract_facts(project: &Path) -> Result<FactSet> {
    let script = locate_script(
        "HINZU_PY_ADAPTER",
        "python",
        "analyze.py",
        "set HINZU_PY_ADAPTER to analyze.py, and run `pip install jedi` so its dependency is \
         present",
    )?;
    ScriptAdapter {
        language: "Python",
        binary: std::env::var("HINZU_PYTHON").unwrap_or_else(|_| "python3".to_string()),
        script,
        // The adapter takes an absolute project path, so it needs no working
        // directory change (unlike the TypeScript adapter's node_modules lookup).
        cwd_is_project: false,
    }
    .extract(project)
}
