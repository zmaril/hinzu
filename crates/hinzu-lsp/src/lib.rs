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
//! Python (over Astral's `ty`) and Go (over `gopls`) are the shipped, tested
//! languages — each a config file and its provenance/effect rows, sharing this
//! one extractor with no per-language code.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use hinzu_core::facts::FactSet;

pub mod api;
pub mod client;
pub mod config;
pub mod extract;
pub mod treesitter;

pub use api::{extract_python_api, PythonApi};
pub use config::LanguageConfig;
pub use extract::Extractor;

/// The shipped Python config (server command, file globs, ty init options, and
/// the typeshed/real-stdlib/site-packages provenance rules).
pub const PYTHON_CONFIG: &str = include_str!("../configs/python.toml");

/// The shipped Go config (gopls server command, `**/*.go` globs, GOROOT +
/// module-cache provenance rules). Wired into `hinzu check` routing for a
/// `go.mod` project, driving the same generic extractor Python uses.
pub const GO_CONFIG: &str = include_str!("../configs/go.toml");

/// The Python effect map is the very same shipped annotation table hinzu-core
/// seeds from — one source of truth, no drift.
pub const PYTHON_ANNOTATIONS: &str = include_str!("../../hinzu-core/annotations/python.toml");

/// The Python third-party library pack — well-known packages the fleet sweep
/// surfaced as Unknown (SQLAlchemy's execution surface → db; rich / PyYAML are
/// pure and carry no `[roots]` effect, so only hinzu-core's pure vouch reads
/// them). Merged onto `PYTHON_ANNOTATIONS` as a built-in Python default, the very
/// same pair hinzu-core's root seeding merges — one source of truth, no drift.
pub const PYTHON_LIB_ANNOTATIONS: &str =
    include_str!("../../hinzu-core/annotations/python-libs.toml");

/// The Go effect map, the very same shipped annotation table hinzu-core seeds
/// from (`os/exec` → process, `os::ReadFile` → fs, `net/http` → net) — one
/// source of truth, no drift.
pub const GO_ANNOTATIONS: &str = include_str!("../../hinzu-core/annotations/go.toml");

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
    LanguageConfig::from_parts(
        PYTHON_CONFIG,
        &[PYTHON_ANNOTATIONS, PYTHON_LIB_ANNOTATIONS],
        &subst,
    )
}

/// Build the Go [`LanguageConfig`]. Go needs no host-specific substitution
/// (gopls infers GOROOT / the module cache itself), so the substitution map is
/// empty; the effect map is the shipped `go.toml` annotation table.
pub fn go_config() -> Result<LanguageConfig> {
    let subst = BTreeMap::new();
    LanguageConfig::from_parts(GO_CONFIG, &[GO_ANNOTATIONS], &subst)
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
    run_extraction(python_config()?, project)
}

/// Extract effect facts from a Go module by driving gopls over its LSP, all in
/// Rust. Returns the parsed [`FactSet`]; the extractor logs a one-line
/// diagnostics summary to stderr. The `gopls` binary must be present
/// (`HINZU_GOPLS` overrides its path) — an honest nonzero error otherwise; there
/// is no fallback resolver and no faked analysis. gopls typechecks the module
/// itself, so the module should build (its dependencies fetched) for calls into
/// dependencies to resolve.
pub fn extract_go(project: &Path) -> Result<FactSet> {
    run_extraction(go_config()?, project)
}

/// Confirm the config's language server is available, canonicalize the project
/// root (LSP servers want an absolute `file://` root), and run the generic
/// extractor. The shared tail of every per-language entry point.
fn run_extraction(cfg: LanguageConfig, project: &Path) -> Result<FactSet> {
    ensure_server_available(&cfg)?;
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
             backend. Install it (for Python: `uv tool install ty` or `pip install ty`; for Go: \
             `go install golang.org/x/tools/gopls@latest`) or set the override env var \
             (HINZU_TY / HINZU_GOPLS) to its path.",
            cfg.language_id,
        );
    }
    Ok(())
}

/// Resolve the language server's argv, applying the per-backend binary override
/// (`HINZU_TY` for Python, `HINZU_GOPLS` for Go) so the extractor spawns the
/// overridden binary, not the default. The single place the overrides live —
/// [`Extractor::run`] and [`ensure_server_available`] both go through here.
pub fn resolved_server_cmd(cfg: &LanguageConfig) -> Vec<String> {
    let mut cmd = cfg.server_cmd.clone();
    let override_var = match cfg.language_id.as_str() {
        "python" => Some("HINZU_TY"),
        "go" => Some("HINZU_GOPLS"),
        _ => None,
    };
    if let Some(var) = override_var {
        if let Ok(bin) = std::env::var(var) {
            if let Some(first) = cmd.first_mut() {
                *first = bin;
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
        go_config().expect("go config parses");
    }

    #[test]
    fn go_config_shape_and_effect_map() {
        let cfg = go_config().expect("go config parses");
        assert_eq!(cfg.language_id, "go");
        assert_eq!(cfg.language(), hinzu_core::facts::Language::Go);
        // gopls is spawned as an stdio LSP server.
        assert_eq!(cfg.server_cmd.first().map(String::as_str), Some("gopls"));
        // Go effects are package-granular and do NOT inherit to a nested import
        // path — `net/url` stays pure even though `net` is net.
        assert!(!cfg.package_effects_inherit);
        assert_eq!(
            cfg.effect_of("net/url::Parse", "net/url"),
            None,
            "net/url is pure URL algebra, independent of net"
        );

        // Whole-package process rule.
        assert_eq!(
            cfg.effect_of("os/exec::Command", "os/exec"),
            Some(hinzu_core::facts::Effect::Process)
        );
        // The effect-mixed `os`: a file operation is fs, an accessor is env, and
        // the pure remainder (os.Exit) is neither.
        assert_eq!(
            cfg.effect_of("os::ReadFile", "os"),
            Some(hinzu_core::facts::Effect::Fs)
        );
        assert_eq!(
            cfg.effect_of("os::Getenv", "os"),
            Some(hinzu_core::facts::Effect::Env)
        );
        assert_eq!(cfg.effect_of("os::Exit", "os"), None);
        // net whole-package, and a protocol package built on it.
        assert_eq!(
            cfg.effect_of("net/http::Get", "net/http"),
            Some(hinzu_core::facts::Effect::Net)
        );
    }

    #[test]
    fn go_provenance_classifies_goroot_and_module_cache() {
        use crate::config::Origin;
        let cfg = go_config().expect("go config parses");
        // A plain GOROOT layout: `.../go/src/<import path>/<file>.go`.
        assert_eq!(
            cfg.package_of("/usr/local/go/src/os/exec/exec.go"),
            Some(("os/exec".to_string(), Origin::Stdlib))
        );
        // A top-level stdlib package.
        assert_eq!(
            cfg.package_of("/usr/local/go/src/os/file.go"),
            Some(("os".to_string(), Origin::Stdlib))
        );
        // A VERSIONED GOROOT dir (`go1.24.7`), what a GOTOOLCHAIN switch leaves
        // on PATH — the observed layout on this dev host.
        assert_eq!(
            cfg.package_of("/usr/local/go1.24.7/src/os/exec/exec.go"),
            Some(("os/exec".to_string(), Origin::Stdlib))
        );
        assert_eq!(
            cfg.package_of("/usr/local/go1.24.7/src/fmt/print.go"),
            Some(("fmt".to_string(), Origin::Stdlib))
        );
        // The GitHub `setup-go` toolcache layout, with version + arch dirs
        // between `go` and `src` — the robust GOROOT rule still resolves it.
        assert_eq!(
            cfg.package_of("/opt/hostedtoolcache/go/1.24.7/x64/src/net/http/client.go"),
            Some(("net/http".to_string(), Origin::Stdlib))
        );
        // A DOWNLOADED toolchain's stdlib, shipped under the module cache — still
        // stdlib, matched before the general module rule. (The `@` is spliced in
        // via `concat!` so the literal is not mistaken for an email address by
        // tooling that rewrites those.)
        assert_eq!(
            cfg.package_of(concat!(
                "/root/go/pkg/mod/golang.org/toolchain",
                "@",
                "v0.0.1-go1.26.5.linux-amd64/src/os/exec/exec.go"
            )),
            Some(("os/exec".to_string(), Origin::Stdlib))
        );
        // A module dependency in the module cache.
        assert_eq!(
            cfg.package_of(concat!(
                "/root/go/pkg/mod/github.com/mattn/go-colorable",
                "@",
                "v0.1.13/color.go"
            )),
            Some(("github.com/mattn/go-colorable".to_string(), Origin::Module))
        );
        // A subpackage of a module keeps its full import path.
        assert_eq!(
            cfg.package_of(concat!(
                "/root/go/pkg/mod/golang.org/x/net",
                "@",
                "v0.27.0/context/ctxhttp/ctxhttp.go"
            )),
            Some((
                "golang.org/x/net/context/ctxhttp".to_string(),
                Origin::Module
            ))
        );
    }
}
