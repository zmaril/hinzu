//! The Rust extraction harness: drive the StableMIR driver over a target cargo
//! project and collect the effect facts it emits.
//!
//! The mechanism is the clippy/miri trick. Cargo is run over the target with
//! `RUSTC_WORKSPACE_WRAPPER` pointed at the `hinzu-rustc-driver` binary, so the
//! driver wraps the compilation of the target's own (workspace) crates while
//! registry dependencies compile with the real rustc. Each wrapped crate walks
//! its monomorphized MIR and writes a `facts-<crate>-<pid>.json` file in hinzu's
//! `FactSet` schema; this module merges them into one `FactSet`.
//!
//! Everything here needs the pinned nightly the driver is built against, so it
//! is off the stable CI path: `hinzu check` only reaches it on a real cargo
//! project without `--facts`, and it fails with an honest message when the
//! toolchain or driver is missing rather than faking an analysis.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use hinzu_core::absint::body::BodyFacts;
use hinzu_core::facts::FactSet;

/// The nightly the driver is pinned to (its `rust-toolchain.toml`). The target
/// crate must compile with the same rustc the driver linked against, so the
/// target build is driven with this toolchain too.
const DRIVER_NIGHTLY: &str = "nightly-2026-07-18";

/// Whether `path` looks like a cargo project (has a `Cargo.toml`).
pub fn is_cargo_project(path: &Path) -> bool {
    path.join("Cargo.toml").is_file()
}

/// Extract effect facts from a cargo project by running the StableMIR driver
/// over it. Returns the merged `FactSet`, or an honest error when the nightly
/// toolchain or the driver binary is unavailable.
pub fn extract_facts(project: &Path) -> Result<FactSet> {
    let facts_dir = run_driver(project, false)?;
    let facts = merge_facts_dir(&facts_dir)?;

    if facts.defs.is_empty() && facts.edges.is_empty() {
        bail!(
            "the driver produced no facts for {} — is it a buildable cargo project?",
            project.display()
        );
    }
    Ok(facts)
}

/// Extract per-function MIR **body facts** (the range-analysis input) from a
/// cargo project, by running the same StableMIR driver with `HINZU_EMIT_BODIES`
/// set. Returns the merged `BodyFacts`, or an honest error when the nightly
/// toolchain or the driver binary is unavailable.
pub fn extract_bodies(project: &Path) -> Result<BodyFacts> {
    let facts_dir = run_driver(project, true)?;
    let bodies = merge_bodies_dir(&facts_dir)?;

    if bodies.functions.is_empty() {
        bail!(
            "the driver produced no function bodies for {} — is it a buildable cargo project?",
            project.display()
        );
    }
    Ok(bodies)
}

/// Locate the driver + its sysroot, create a fresh facts dir, and run the
/// extraction build over `project`; returns the facts dir the driver wrote into.
/// Shared by the effect-fact and body-fact extraction paths.
fn run_driver(project: &Path, emit_bodies: bool) -> Result<PathBuf> {
    let driver = driver_binary().context(
        "the StableMIR driver is unavailable — build crates/hinzu-rustc-driver with its pinned \
         nightly, or point HINZU_RUSTC_DRIVER at a prebuilt binary",
    )?;
    let sysroot = driver_sysroot()?;
    let facts_dir = tempdir_for_facts()?;
    run_extraction(project, &driver, &sysroot, &facts_dir, emit_bodies)?;
    Ok(facts_dir)
}

/// Merge every `bodies-*.json` in `dir` into one `BodyFacts`: functions from
/// each wrapped compilation unit are concatenated, deduped by symbol id.
fn merge_bodies_dir(dir: &Path) -> Result<BodyFacts> {
    let mut merged = BodyFacts::default();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    for file in &output_files(dir, "bodies-")? {
        let json =
            std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        let facts = BodyFacts::from_json(&json)
            .with_context(|| format!("parsing driver body facts from {}", file.display()))?;
        for function in facts.functions {
            if seen.insert(function.id.clone()) {
                merged.functions.push(function);
            }
        }
    }
    Ok(merged)
}

/// The sorted `<prefix>*.json` output files the driver wrote into `dir`.
fn output_files(dir: &Path, prefix: &str) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading facts dir {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| is_output_file(p, prefix))
        .collect();
    files.sort();
    Ok(files)
}

/// Whether `path` is a `<prefix>*.json` driver output file.
fn is_output_file(path: &Path, prefix: &str) -> bool {
    path.extension().is_some_and(|e| e == "json")
        && path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(prefix))
}

/// Locate the driver binary: an explicit `HINZU_RUSTC_DRIVER` override, else
/// build it from the in-tree crate under its pinned nightly.
fn driver_binary() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("HINZU_RUSTC_DRIVER") {
        let path = PathBuf::from(path);
        if !path.is_file() {
            bail!("HINZU_RUSTC_DRIVER={} is not a file", path.display());
        }
        return Ok(path);
    }
    build_driver()
}

/// The in-tree driver crate directory, relative to this crate.
fn driver_crate_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("hinzu-cli has a parent crates/ dir")
        .join("hinzu-rustc-driver")
}

/// Build the driver crate with its pinned nightly and return the built binary.
fn build_driver() -> Result<PathBuf> {
    let dir = driver_crate_dir();
    if !dir.join("Cargo.toml").is_file() {
        bail!("driver crate not found at {}", dir.display());
    }
    let status = Command::new("cargo")
        .current_dir(&dir)
        .args(["build", "--release"])
        .status()
        .context("running `cargo build` for the StableMIR driver (is the nightly installed?)")?;
    if !status.success() {
        bail!("building the StableMIR driver failed");
    }
    let bin = dir.join("target/release/hinzu-rustc-driver");
    if !bin.is_file() {
        bail!("driver built but binary missing at {}", bin.display());
    }
    Ok(bin)
}

/// The driver's toolchain sysroot, whose `lib/` holds `librustc_driver.so`. Run
/// from the driver crate dir so its `rust-toolchain.toml` selects the nightly.
fn driver_sysroot() -> Result<PathBuf> {
    let dir = driver_crate_dir();
    let out = Command::new("rustc")
        .current_dir(&dir)
        .args(["--print", "sysroot"])
        .output()
        .context("querying the driver toolchain sysroot")?;
    if !out.status.success() {
        bail!("`rustc --print sysroot` failed for the driver toolchain");
    }
    let path = String::from_utf8(out.stdout)
        .context("sysroot path was not utf-8")?
        .trim()
        .to_string();
    Ok(PathBuf::from(path))
}

/// A fresh directory the driver writes its per-crate facts files into.
fn tempdir_for_facts() -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("hinzu-facts-{}", std::process::id()));
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating facts dir {}", dir.display()))?;
    Ok(dir)
}

/// Run the target's build with the driver as the workspace wrapper. A fresh
/// `CARGO_TARGET_DIR` forces a clean compile, so the wrapper actually runs over
/// every workspace crate rather than being skipped by an existing cache.
fn run_extraction(
    project: &Path,
    driver: &Path,
    sysroot: &Path,
    facts_dir: &Path,
    emit_bodies: bool,
) -> Result<()> {
    let ld = prepend_ld_library_path(sysroot.join("lib"));
    let target_dir = facts_dir.join("cargo-target");

    let mut command = Command::new("cargo");
    command
        .current_dir(project)
        .arg(format!("+{DRIVER_NIGHTLY}"))
        .arg("build")
        .env("RUSTC_WORKSPACE_WRAPPER", driver)
        .env("LD_LIBRARY_PATH", ld)
        .env("HINZU_FACTS_DIR", facts_dir)
        .env("CARGO_TARGET_DIR", target_dir);
    if emit_bodies {
        command.env("HINZU_EMIT_BODIES", "1");
    }
    let status = command
        .status()
        .with_context(|| format!("building {} with the StableMIR driver", project.display()))?;
    if !status.success() {
        bail!(
            "extraction build failed for {} — the target must compile on {DRIVER_NIGHTLY}",
            project.display()
        );
    }
    Ok(())
}

/// Prepend `dir` to the existing `LD_LIBRARY_PATH`, if any.
fn prepend_ld_library_path(dir: PathBuf) -> String {
    match std::env::var("LD_LIBRARY_PATH") {
        Ok(existing) if !existing.is_empty() => format!("{}:{existing}", dir.display()),
        _ => dir.display().to_string(),
    }
}

/// Merge every `facts-*.json` in `dir` into one `FactSet`: definitions upsert
/// by id, edges and roots union (deduped) so the two wrapped compilation units
/// (lib and bin) combine without double-counting shared roots.
fn merge_facts_dir(dir: &Path) -> Result<FactSet> {
    let mut merged = FactSet::default();
    let mut seen_edges: BTreeSet<(String, String, String)> = BTreeSet::new();
    let mut seen_roots: BTreeSet<(String, String)> = BTreeSet::new();

    for file in &output_files(dir, "facts-")? {
        let json =
            std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        let facts = FactSet::from_json(&json)
            .with_context(|| format!("parsing driver facts from {}", file.display()))?;

        for def in facts.defs.into_values() {
            merged.add_def(def);
        }
        for edge in facts.edges {
            let key = (
                edge.caller.clone(),
                edge.callee.clone(),
                edge.kind.as_str().to_string(),
            );
            if seen_edges.insert(key) {
                merged.add_edge(edge);
            }
        }
        for root in facts.roots {
            if seen_roots.insert((root.symbol.clone(), root.effect.as_str().to_string())) {
                merged.add_root(root);
            }
        }
    }
    Ok(merged)
}
