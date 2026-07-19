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

/// One package's port mapping.
#[derive(Debug, Deserialize)]
pub struct PackageConfig {
    /// The source package directory, relative to `base_dir` (extraction root).
    pub source_dir: String,
    /// The target crate directory, relative to `base_dir` (extraction root).
    pub target_dir: String,
    /// The target crate prefix on target ids (`"atilla_ai"`).
    pub strip_crate_prefix: String,
    /// The workspace-relative source dir a target file carries in the graph
    /// (`"crates/atilla-ai/src"`). Not resolved against `base_dir`.
    pub target_src_prefix: String,
    /// The manifest `package` this crate's conformance modules are filed under.
    pub conformance_package: String,
}

/// One resolved package: the absolute extraction paths + a ready [`PortDiffConfig`].
pub struct ResolvedPackage {
    /// The `--package` name.
    pub name: String,
    /// Absolute path to the source package (extraction root for the source graph).
    pub source_path: PathBuf,
    /// Absolute path to the target crate (extraction root for the target graph).
    pub target_path: PathBuf,
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
        let target_path = resolve_path(&base, &pkg.target_dir);
        let manifest_path = resolve_path(&base, &self.conformance_manifest);

        let config = PortDiffConfig {
            source_kind: self.source_kind.clone(),
            target_kind: self.target_kind.clone(),
            naming: NamingRules {
                file_segment_case: self.naming.file_segment_case.clone(),
                strip_suffixes: self.naming.strip_suffixes.clone(),
                fn_case: self.naming.fn_case.clone(),
                keep_pascal_types: self.naming.keep_pascal_types,
                keep_screaming_consts: self.naming.keep_screaming_consts,
                strip_crate_prefix: pkg.strip_crate_prefix.clone(),
                target_src_prefix: pkg.target_src_prefix.clone(),
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
            manifest_path,
            config,
        })
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
