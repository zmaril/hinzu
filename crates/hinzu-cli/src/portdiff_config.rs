//! The multi-package port-diff config: a committed TOML that describes several
//! same-shape package ports (e.g. every `pi` TypeScript package → its `atilla`
//! Rust crate) with one set of shared naming rules, from which one package's
//! [`PortDiffConfig`] is materialized.
//!
//! The file has three parts:
//!   * top-level scalars — the language pair, the band thresholds, the `base_dir`
//!     every relative path is resolved against, the conformance manifest path,
//!     and the manifest status that marks a module test-verified;
//!   * `[naming]` — the shared TS→Rust naming rules, identical across packages;
//!   * `[packages.<name>]` — one table per package, giving its source/target
//!     directories, target crate prefixes, and the conformance package to filter
//!     the manifest on.
//!
//! **Path resolution.** Every relative path in the file (`source_dir`,
//! `target_dir`, `conformance_manifest`) is resolved against `base_dir`. Absolute
//! paths are used as-is. `target_src_prefix` is *not* a filesystem path — it is
//! the workspace-relative prefix a target file carries in the emitted graph
//! (`crates/atilla-ai/src`), so it is left verbatim.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use hinzu_core::portdiff::{ConformanceConfig, NamingRules, PortDiffConfig};
use serde::Deserialize;

/// The whole config file: shared settings + a map of packages.
#[derive(Debug, Deserialize)]
pub struct MultiPackageConfig {
    /// The source language / ecosystem tag (`"ts"`).
    pub source_kind: String,
    /// The target language / ecosystem tag (`"rust"`).
    pub target_kind: String,
    /// Coverage at or above which a (non-native) file is banded PORTED.
    pub ported_threshold: f64,
    /// Fraction of the winning weighted-vote mass a clustered subtree must retain.
    pub cluster_vote_retain: f64,
    /// The directory every relative path below is resolved against.
    pub base_dir: String,
    /// The conformance manifest, relative to `base_dir` (or absolute).
    pub conformance_manifest: String,
    /// The manifest `status` value that marks a module test-verified (`"native"`).
    pub native_status: String,
    /// The shared naming ruleset, identical across every package.
    pub naming: SharedNaming,
    /// One table per package, keyed by the CLI `--package` name.
    pub packages: BTreeMap<String, PackageConfig>,
}

/// The shared TS→Rust naming rules — the same for every package.
#[derive(Debug, Deserialize)]
pub struct SharedNaming {
    /// How a source file path segment is normalized (`"kebab_to_snake"`).
    pub file_segment_case: String,
    /// How a function / method leaf is normalized (`"camel_to_snake"`).
    pub fn_case: String,
    /// Keep PascalCase type names verbatim.
    pub keep_pascal_types: bool,
    /// Keep SCREAMING_SNAKE constants verbatim.
    pub keep_screaming_consts: bool,
    /// Compound file suffixes stripped before the extension (`[".lazy"]`).
    pub strip_suffixes: Vec<String>,
    /// The source package's leading source directory (`"src"`).
    pub source_src_prefix: String,
}

/// One package's port mapping. A package targets either a single crate (the
/// singular `target_dir`/`strip_crate_prefix`/`target_src_prefix` keys, the
/// backward-compatible form) OR several crates (the `[[packages.X.targets]]`
/// array). Exactly one form must be given; [`MultiPackageConfig::resolve`]
/// normalizes both to a `Vec<TargetCrate>`.
#[derive(Debug, Deserialize)]
pub struct PackageConfig {
    /// The source package directory, relative to `base_dir` (extraction root).
    pub source_dir: String,
    /// The target crate directory, relative to `base_dir` (single-crate form).
    pub target_dir: Option<String>,
    /// The target crate prefix on target ids, `"atilla_ai"` (single-crate form).
    pub strip_crate_prefix: Option<String>,
    /// The workspace-relative source dir a target file carries in the graph,
    /// `"crates/atilla-ai/src"` (single-crate form). Not resolved against `base_dir`.
    pub target_src_prefix: Option<String>,
    /// The target crates a source package was ported across (multi-crate form).
    /// Mutually exclusive with the singular keys above.
    #[serde(default)]
    pub targets: Vec<TargetCrate>,
    /// The manifest `package` this crate's conformance modules are filed under.
    pub conformance_package: String,
}

/// One target crate in the multi-crate form (`[[packages.X.targets]]`).
#[derive(Debug, Deserialize)]
pub struct TargetCrate {
    /// The target crate directory, relative to `base_dir` (extraction root).
    pub dir: String,
    /// The target crate prefix on target ids (`"pidgin_cli"`).
    pub strip_crate_prefix: String,
    /// The workspace-relative source dir a target file carries in the graph
    /// (`"crates/pidgin-cli/src"`). Not resolved against `base_dir`.
    pub src_prefix: String,
}

/// One resolved package: the absolute extraction paths + a ready [`PortDiffConfig`].
pub struct ResolvedPackage {
    /// The `--package` name.
    pub name: String,
    /// Absolute path to the source package (extraction root for the source graph).
    pub source_path: PathBuf,
    /// Absolute path to the primary target crate (the first `target_paths` entry).
    /// Used for labels/logging; extraction merges every `target_paths` crate.
    pub target_path: PathBuf,
    /// Absolute paths to every target crate this package was ported across. The
    /// CLI extracts each and merges the graphs into one target index, so a source
    /// package ported across crates stays visible to the matcher. One entry in the
    /// single-crate form; several in the multi-crate (`targets`) form.
    pub target_paths: Vec<PathBuf>,
    /// Absolute path to the conformance manifest (read by the CLI, never by core).
    pub manifest_path: PathBuf,
    /// The per-package config the matcher keys on.
    pub config: PortDiffConfig,
}

impl MultiPackageConfig {
    /// Parse the config file. Only reads + parses; no package is resolved yet.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading port-diff config {}", path.display()))?;
        toml::from_str(&text)
            .with_context(|| format!("parsing port-diff config {}", path.display()))
    }

    /// The package names, in sorted (map) order — for the "which package?" error.
    pub fn package_names(&self) -> Vec<String> {
        self.packages.keys().cloned().collect()
    }

    /// Resolve one package into its extraction paths + [`PortDiffConfig`]. Merges
    /// the shared naming rules with the package's crate-specific fields and builds
    /// the conformance oracle (whose `src_prefix_strip` is
    /// `packages/<conformance_package>/`, the layout every `pi` package shares).
    pub fn resolve(&self, name: &str) -> Result<ResolvedPackage> {
        let pkg = self.packages.get(name).ok_or_else(|| {
            anyhow::anyhow!(
                "package '{name}' is not in the config; available packages: {}",
                self.package_names().join(", ")
            )
        })?;
        let base = PathBuf::from(&self.base_dir);
        let source_path = resolve_path(&base, &pkg.source_dir);
        let manifest_path = resolve_path(&base, &self.conformance_manifest);

        // Normalize the single-crate keys and the multi-crate `targets` array into
        // one ordered list of target crates. Exactly one form must be given.
        let targets = pkg.normalized_targets(name)?;
        let target_paths: Vec<PathBuf> = targets
            .iter()
            .map(|t| resolve_path(&base, &t.dir))
            .collect();
        let target_path = target_paths[0].clone();

        let config = PortDiffConfig {
            source_kind: self.source_kind.clone(),
            target_kind: self.target_kind.clone(),
            naming: NamingRules {
                file_segment_case: self.naming.file_segment_case.clone(),
                strip_suffixes: self.naming.strip_suffixes.clone(),
                fn_case: self.naming.fn_case.clone(),
                keep_pascal_types: self.naming.keep_pascal_types,
                keep_screaming_consts: self.naming.keep_screaming_consts,
                strip_crate_prefix: targets
                    .iter()
                    .map(|t| t.strip_crate_prefix.clone())
                    .collect(),
                target_src_prefix: targets.iter().map(|t| t.src_prefix.clone()).collect(),
                source_src_prefix: self.naming.source_src_prefix.clone(),
            },
            ported_threshold: self.ported_threshold,
            cluster_vote_retain: self.cluster_vote_retain,
            conformance: Some(ConformanceConfig {
                manifest_path: manifest_path.display().to_string(),
                native_status: self.native_status.clone(),
                package: pkg.conformance_package.clone(),
                src_prefix_strip: format!("packages/{}/", pkg.conformance_package),
            }),
        };

        Ok(ResolvedPackage {
            name: name.to_string(),
            source_path,
            target_path,
            target_paths,
            manifest_path,
            config,
        })
    }
}

impl PackageConfig {
    /// Normalize this package's target mapping into one ordered `Vec<TargetCrate>`.
    /// Accepts either the singular `target_dir`/`strip_crate_prefix`/
    /// `target_src_prefix` keys (yields a 1-element vec) or the `targets` array,
    /// but not both and not neither.
    fn normalized_targets(&self, name: &str) -> Result<Vec<TargetCrate>> {
        let has_single = self.target_dir.is_some()
            || self.strip_crate_prefix.is_some()
            || self.target_src_prefix.is_some();
        let has_multi = !self.targets.is_empty();
        match (has_single, has_multi) {
            (true, true) => anyhow::bail!(
                "package '{name}' sets both the singular target_dir keys and a \
                 [[packages.{name}.targets]] array; use exactly one form"
            ),
            (false, false) => anyhow::bail!(
                "package '{name}' has no target: set target_dir/strip_crate_prefix/\
                 target_src_prefix, or one or more [[packages.{name}.targets]] tables"
            ),
            (false, true) => Ok(self
                .targets
                .iter()
                .map(|t| TargetCrate {
                    dir: t.dir.clone(),
                    strip_crate_prefix: t.strip_crate_prefix.clone(),
                    src_prefix: t.src_prefix.clone(),
                })
                .collect()),
            (true, false) => {
                let missing = self.target_dir.is_none()
                    || self.strip_crate_prefix.is_none()
                    || self.target_src_prefix.is_none();
                if missing {
                    anyhow::bail!(
                        "package '{name}' single-crate form needs all of target_dir, \
                         strip_crate_prefix, and target_src_prefix"
                    );
                }
                Ok(vec![TargetCrate {
                    dir: self.target_dir.clone().unwrap(),
                    strip_crate_prefix: self.strip_crate_prefix.clone().unwrap(),
                    src_prefix: self.target_src_prefix.clone().unwrap(),
                }])
            }
        }
    }
}

/// Resolve `rel` against `base`, unless it is already absolute.
fn resolve_path(base: &Path, rel: &str) -> PathBuf {
    let p = PathBuf::from(rel);
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse `packages_toml` (one or more `[packages.X]` sections) atop the shared
    /// header and resolve package `pkg`. The base_dir is absolute so `target_paths`
    /// come back predictable.
    fn resolve_pkg(packages_toml: &str, pkg: &str) -> Result<ResolvedPackage> {
        let text = format!(
            r#"
source_kind = "ts"
target_kind = "rust"
ported_threshold = 0.6
cluster_vote_retain = 0.6
base_dir = "/ws"
conformance_manifest = "conformance/manifest.json"
native_status = "native"

[naming]
file_segment_case = "kebab_to_snake"
fn_case = "camel_to_snake"
keep_pascal_types = true
keep_screaming_consts = true
strip_suffixes = [".lazy"]
source_src_prefix = "src"

{packages_toml}
"#
        );
        let cfg: MultiPackageConfig = toml::from_str(&text).expect("parse config");
        cfg.resolve(pkg)
    }

    #[test]
    fn resolve_single_crate_form() {
        let resolved = resolve_pkg(
            r#"
[packages.ai]
source_dir = "vendor/pi/packages/ai"
target_dir = "crates/pidgin-ai"
strip_crate_prefix = "pidgin_ai"
target_src_prefix = "crates/pidgin-ai/src"
conformance_package = "ai"
"#,
            "ai",
        )
        .unwrap();
        assert_eq!(
            resolved.target_paths,
            vec![PathBuf::from("/ws/crates/pidgin-ai")]
        );
        assert_eq!(resolved.target_path, PathBuf::from("/ws/crates/pidgin-ai"));
        assert_eq!(
            resolved.config.naming.strip_crate_prefix,
            vec!["pidgin_ai".to_string()]
        );
        assert_eq!(
            resolved.config.naming.target_src_prefix,
            vec!["crates/pidgin-ai/src".to_string()]
        );
    }

    #[test]
    fn resolve_multi_crate_form() {
        let resolved = resolve_pkg(
            r#"
[packages.coding-agent]
source_dir = "vendor/pi/packages/coding-agent"
conformance_package = "coding-agent"

[[packages.coding-agent.targets]]
dir = "crates/pidgin-coding"
strip_crate_prefix = "pidgin_coding"
src_prefix = "crates/pidgin-coding/src"

[[packages.coding-agent.targets]]
dir = "crates/pidgin-cli"
strip_crate_prefix = "pidgin_cli"
src_prefix = "crates/pidgin-cli/src"
"#,
            "coding-agent",
        )
        .unwrap();
        assert_eq!(
            resolved.target_paths,
            vec![
                PathBuf::from("/ws/crates/pidgin-coding"),
                PathBuf::from("/ws/crates/pidgin-cli"),
            ]
        );
        // The primary target (labels/logging) is the first crate.
        assert_eq!(
            resolved.target_path,
            PathBuf::from("/ws/crates/pidgin-coding")
        );
        assert_eq!(
            resolved.config.naming.strip_crate_prefix,
            vec!["pidgin_coding".to_string(), "pidgin_cli".to_string()]
        );
        assert_eq!(
            resolved.config.naming.target_src_prefix,
            vec![
                "crates/pidgin-coding/src".to_string(),
                "crates/pidgin-cli/src".to_string(),
            ]
        );
    }

    #[test]
    fn resolve_rejects_both_and_neither_forms() {
        // Both singular keys and a targets array: rejected.
        let both = resolve_pkg(
            r#"
[packages.x]
source_dir = "s"
target_dir = "crates/x"
strip_crate_prefix = "x"
target_src_prefix = "crates/x/src"
conformance_package = "x"

[[packages.x.targets]]
dir = "crates/y"
strip_crate_prefix = "y"
src_prefix = "crates/y/src"
"#,
            "x",
        );
        assert!(both.is_err());
        // Neither form: rejected.
        let neither = resolve_pkg(
            r#"
[packages.x]
source_dir = "s"
conformance_package = "x"
"#,
            "x",
        );
        assert!(neither.is_err());
    }
}
