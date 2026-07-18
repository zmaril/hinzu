//! hinzu-lsp — a generic, LSP-driven fact extractor, all in Rust.
//!
//! This crate is hinzu's new baseline extraction mechanism: a synchronous LSP
//! client ([`client`]) plus a language-agnostic extractor ([`extract`]) driven by
//! a per-language [`LanguageConfig`] ([`config`]). Point it at any language
//! server that speaks `documentSymbol` + `callHierarchy` and it emits hinzu's
//! [`FactSet`] in-process — no per-language parser, no script subprocess, no JSON
//! round-trip. The only non-Rust artifacts left on the path are the external
//! server binaries it invokes (ty for Python, gopls for Go), which hinzu does not
//! write.
//!
//! Python (over Astral's `ty`) is the shipped, tested language. A Go config stub
//! lives beside it to keep the "new language = new config, not new code" seam
//! honest.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use hinzu_core::facts::FactSet;

pub mod client;
pub mod config;
pub mod extract;

pub use config::LanguageConfig;
pub use extract::Extractor;

/// The shipped Python config (server command, file globs, ty init options, and
/// the typeshed/real-stdlib/site-packages provenance rules).
pub const PYTHON_CONFIG: &str = include_str!("../configs/python.toml");

/// The shipped Go config stub — proof the extractor generalizes; not wired into
/// `hinzu check` routing yet (a separate decision), but it parses and carries a
/// real gopls provenance/effect ruleset.
pub const GO_CONFIG: &str = include_str!("../configs/go.toml");

/// The Python effect map is the very same shipped annotation table hinzu-core
/// seeds from — one source of truth, no drift.
pub const PYTHON_ANNOTATIONS: &str = include_str!("../../hinzu-core/annotations/python.toml");

/// Build the Python [`LanguageConfig`], pinning ty's target `python-version` and
/// `python-platform` so its vendored typeshed stdlib resolves deterministically.
/// `HINZU_PY_VERSION` overrides the version (default `3.11`, matching CI and the
/// fixture's `requires-python`); the platform tracks the host.
pub fn python_config() -> Result<LanguageConfig> {
    let mut subst = BTreeMap::new();
    let version = std::env::var("HINZU_PY_VERSION").unwrap_or_else(|_| "3.11".to_string());
    subst.insert("python_version".to_string(), version);
    subst.insert(
        "python_platform".to_string(),
        host_python_platform().to_string(),
    );
    LanguageConfig::from_parts(PYTHON_CONFIG, PYTHON_ANNOTATIONS, &subst)
}

/// ty's `python-platform` spelling for the host OS.
fn host_python_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "win32"
    } else {
        "linux"
    }
}

/// Extract effect facts from a Python project by driving ty over its LSP, all in
/// Rust. Returns the parsed [`FactSet`]; the extractor logs a one-line
/// diagnostics summary to stderr. The `ty` binary must be present (`HINZU_TY`
/// overrides its path) — an honest nonzero error otherwise; there is no fallback
/// resolver and no faked analysis.
pub fn extract_python(project: &Path) -> Result<FactSet> {
    let cfg = python_config()?;
    ensure_server_available(&cfg)?;
    // LSP servers want an absolute `file://` root, so canonicalize first.
    let project = project
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("resolving project path {}: {e}", project.display()))?;
    Extractor::new(&cfg, &project).run()
}

/// Verify the configured language server's binary is on `PATH` (or is an
/// absolute, existing path), so a missing tool is an honest capability edge
/// rather than a spawn error mid-run. `HINZU_TY` overrides ty's binary.
fn ensure_server_available(cfg: &LanguageConfig) -> Result<()> {
    let cmd = resolved_server_cmd(cfg);
    let Some(bin) = cmd.first() else {
        anyhow::bail!("the {} config has an empty server command", cfg.language_id);
    };
    if which(bin).is_none() {
        anyhow::bail!(
            "the `{bin}` language server was not found — it is the {} adapter's resolution \
             backend. Install it (for Python: `uv tool install ty` or `pip install ty`) or set \
             HINZU_TY to its path.",
            cfg.language_id,
        );
    }
    Ok(())
}

/// Resolve the language server's argv, applying the `HINZU_TY` override for the
/// Python backend so the extractor spawns the overridden binary, not the default.
/// The single place the override lives — [`Extractor::run`] and
/// [`ensure_server_available`] both go through here.
pub fn resolved_server_cmd(cfg: &LanguageConfig) -> Vec<String> {
    let mut cmd = cfg.server_cmd.clone();
    if cfg.language_id == "python" {
        if let Ok(ty) = std::env::var("HINZU_TY") {
            if let Some(first) = cmd.first_mut() {
                *first = ty;
            }
        }
    }
    cmd
}

/// A minimal `which`: an absolute/relative existing file, or a bare name found on
/// `PATH`.
fn which(bin: &str) -> Option<std::path::PathBuf> {
    let p = Path::new(bin);
    if p.is_absolute() || bin.contains('/') {
        return p.is_file().then(|| p.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(bin))
        .find(|c| c.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_configs_parse() {
        python_config().expect("python config parses");
        let mut subst = BTreeMap::new();
        subst.insert("go_version".to_string(), "1.24".to_string());
        LanguageConfig::from_parts(GO_CONFIG, GO_ANNOTATIONS_FOR_TEST, &subst)
            .expect("go stub parses");
    }

    /// A tiny inline effect map so the Go-stub parse test needs no shipped file.
    const GO_ANNOTATIONS_FOR_TEST: &str =
        "[roots]\n\"os/exec\" = \"process\"\n\"os::Getenv\" = \"env\"\n";
}
