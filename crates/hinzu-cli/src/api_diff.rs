//! The `hinzu api-diff` CLI flow: grade a TARGET package's public surface
//! against a SOURCE package's, item by item, from two `hinzu api` report JSONs.
//!
//! All the file I/O lives here in the CLI; the pure grading is
//! [`hinzu_core::apidiff::build_api_diff`]. The naming rules are pulled from the
//! same port-diff config port-diff uses (`--config` + `--package`), or fall back
//! to the built-in TS→Rust ruleset, so cross-language comparisons don't
//! false-miss on convention renames.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;

use crate::{portdiff_config, write_json};

#[derive(Parser)]
pub struct ApiDiffArgs {
    /// The SOURCE package's `hinzu api` report JSON — the public-surface contract
    /// the target must match. Produce it with `hinzu api <src-pkg> --out src.json`.
    #[arg(long)]
    source: PathBuf,
    /// The TARGET package's `hinzu api` report JSON — the port whose surface is
    /// graded. Produce it with `hinzu api <tgt-pkg> --out tgt.json`.
    #[arg(long)]
    target: PathBuf,
    /// A port-diff config TOML (`--config`) + `--package <name>`: pull the same
    /// naming rules (camel↔snake fns, kebab↔snake files, PascalCase/SCREAMING
    /// kept) and package→crate module mapping port-diff uses, so cross-language
    /// comparisons don't false-miss on convention renames. Optional — without it
    /// the built-in TS→Rust naming ruleset is used.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Which package in `--config` to pull the naming rules + crate mapping from.
    /// Required when `--config` is given.
    #[arg(long)]
    package: Option<String>,
    /// Where to write the conformance report JSON. Defaults to stdout.
    #[arg(long)]
    out: Option<PathBuf>,
}

/// The `hinzu api-diff` flow. Reads the two `hinzu api` reports off disk, builds
/// the [`NamingRules`](hinzu_core::portdiff::NamingRules) (from `--config` +
/// `--package`, else the built-in TS→Rust ruleset), calls the pure
/// [`hinzu_core::apidiff::build_api_diff`], and writes the conformance JSON to
/// `--out` or stdout. All the file I/O is here in the CLI; core only grades the
/// two in-memory reports.
pub fn run(args: ApiDiffArgs) -> Result<ExitCode> {
    let source = load_api_report(&args.source)?;
    let target = load_api_report(&args.target)?;
    let rules = api_diff_rules(args.config.as_deref(), args.package.as_deref())?;
    let report = hinzu_core::apidiff::build_api_diff(&source, &target, &rules);
    let json =
        serde_json::to_string_pretty(&report).context("serializing the api-diff report to JSON")?;
    write_json(args.out.as_deref(), &json, "api-diff report")
}

/// Read + parse a [`hinzu_core::api::ApiReport`] JSON produced by `hinzu api`.
fn load_api_report(path: &Path) -> Result<hinzu_core::api::ApiReport> {
    let json = std::fs::read_to_string(path)
        .with_context(|| format!("reading api report from {}", path.display()))?;
    serde_json::from_str(&json)
        .with_context(|| format!("parsing api report from {}", path.display()))
}

/// Build the naming ruleset for `hinzu api-diff`: from the port-diff config's
/// selected package (so it reuses the exact naming rules + crate mapping
/// port-diff uses), or — when no `--config` is given — the built-in TS→Rust
/// prototype ruleset. `--config` without `--package` is an error, since the rules
/// (and the crate prefix) are per package.
fn api_diff_rules(
    config: Option<&Path>,
    package: Option<&str>,
) -> Result<hinzu_core::portdiff::NamingRules> {
    match config {
        Some(cfg_path) => {
            let package = package.ok_or_else(|| {
                anyhow::anyhow!("--config needs --package <name> to select the naming rules")
            })?;
            let cfg = portdiff_config::MultiPackageConfig::load(cfg_path)?;
            Ok(cfg.resolve(package)?.config.naming)
        }
        None => {
            if package.is_some() {
                anyhow::bail!("--package needs --config <toml> to read the package's naming rules");
            }
            Ok(hinzu_core::portdiff::PortDiffConfig::default_ts_rust().naming)
        }
    }
}
