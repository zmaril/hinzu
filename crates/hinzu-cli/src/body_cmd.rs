//! The body-IR subcommands: `hinzu ranges` (numeric range analysis) and
//! `hinzu model` (lower the body IR to a formal-model skeleton). Both consume
//! the same extracted `BodyFacts` and share the body-loading path, so they live
//! together here as a thin shell over the pure engines in hinzu-core — all the
//! file/process I/O stays on the CLI side.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};

use crate::{rust_adapter, write_json};

/// The formal-model backend `hinzu model` lowers the body IR to. Structured as an
/// enum so more targets can be added without changing the CLI surface.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum EmitTarget {
    /// A Quint (`.qnt`) module skeleton.
    Quint,
    // phase D: Stateright
}

#[derive(Parser)]
pub struct ModelArgs {
    /// The project to analyze (a cargo project, when extracting live).
    path: PathBuf,
    /// Pre-extracted body facts as JSON (the StableMIR driver's `bodies-*.json`
    /// schema), in place of a live extraction. Lets the lowering run with no
    /// nightly toolchain.
    #[arg(long)]
    bodies: Option<PathBuf>,
    /// The formal-model backend to emit. Defaults to Quint.
    #[arg(long, value_enum, default_value_t = EmitTarget::Quint)]
    emit: EmitTarget,
    /// Where to write the model text. Defaults to stdout.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Parser)]
pub struct RangesArgs {
    /// The project to analyze (a cargo project, when extracting live).
    path: PathBuf,
    /// Pre-extracted body facts as JSON (the StableMIR driver's `bodies-*.json`
    /// schema), in place of a live extraction. Lets the analysis run with no
    /// nightly toolchain.
    #[arg(long)]
    bodies: Option<PathBuf>,
    /// Where to write the ranges JSON. Defaults to stdout.
    #[arg(long)]
    out: Option<PathBuf>,
}

/// The `hinzu ranges` flow. Loads body facts (from `--bodies` JSON, or by
/// extracting them live with the StableMIR driver — all the file/process I/O is
/// on the CLI side), runs the pure abstract-interpretation engine, writes the
/// deterministic ranges-and-hazards JSON, and returns a non-zero code when a
/// hazard is found so it is usable as a CI gate.
pub fn ranges(args: RangesArgs) -> Result<ExitCode> {
    let bodies = load_bodies(&args.path, args.bodies.as_deref(), "ranges")?;

    let report = hinzu_core::absint::analyze_bodies(&bodies);
    let json =
        serde_json::to_string_pretty(&report).context("serializing the ranges report to JSON")?;
    write_json(args.out.as_deref(), &json, "ranges report")?;

    if report.hazards.is_empty() {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

/// The `hinzu model` flow. Loads body facts exactly like `hinzu ranges`, lowers
/// them to a formal-model skeleton with the pure emitter for the chosen backend,
/// and writes the model text (to `--out` or stdout). No analysis gate — the
/// output is a skeleton to finish, so it always exits zero on success.
pub fn model(args: ModelArgs) -> Result<ExitCode> {
    let bodies = load_bodies(&args.path, args.bodies.as_deref(), "model")?;
    let text = match args.emit {
        EmitTarget::Quint => hinzu_core::absint::emit_quint(&bodies),
    };
    write_text(args.out.as_deref(), &text, "quint model")
}

/// Load body facts for the body-IR commands: from a `--bodies` JSON file (no
/// toolchain), or by extracting Rust MIR live with the StableMIR driver. `cmd`
/// names the subcommand in the not-a-cargo-project error. All the file/process
/// I/O is on the CLI side; the core lowering stays pure.
fn load_bodies(
    path: &Path,
    bodies: Option<&Path>,
    cmd: &str,
) -> Result<hinzu_core::absint::body::BodyFacts> {
    if let Some(bodies) = bodies {
        let json = std::fs::read_to_string(bodies)
            .with_context(|| format!("reading body facts from {}", bodies.display()))?;
        hinzu_core::absint::body::BodyFacts::from_json(&json)
            .with_context(|| format!("parsing body facts from {}", bodies.display()))
    } else {
        if !rust_adapter::is_cargo_project(path) {
            anyhow::bail!(
                "{} is not a cargo project — `hinzu {cmd}` extracts Rust MIR bodies today; \
                 pass --bodies <json> to analyze pre-extracted facts",
                path.display()
            );
        }
        rust_adapter::extract_bodies(path)
    }
}

/// Write text (already terminated by its own trailing newline) to `out` or
/// stdout. Like [`write_json`] but does not re-wrap the payload — used for the
/// `hinzu model` skeletons, whose emitters produce a final newline themselves.
fn write_text(out: Option<&Path>, text: &str, what: &str) -> Result<ExitCode> {
    match out {
        Some(out) => {
            std::fs::write(out, text)
                .with_context(|| format!("writing {what} to {}", out.display()))?;
        }
        None => print!("{text}"),
    }
    Ok(ExitCode::SUCCESS)
}
