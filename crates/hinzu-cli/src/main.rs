//! The hinzu CLI. A thin shell: parse argv, hand off to hinzu-core.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use hinzu_core::facts::FactSet;
use hinzu_core::policy::Policy;

#[derive(Parser)]
#[command(name = "hinzu", version, about = "hinzu")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the built-in synthetic demo through the engine.
    Run,
    /// Check a project's effect usage against a `hinzu.toml` policy.
    Check(CheckArgs),
}

#[derive(Parser)]
struct CheckArgs {
    /// The project to analyze.
    path: PathBuf,
    /// The policy file. Defaults to `hinzu.toml` in the project (or the repo).
    #[arg(long)]
    policy: Option<PathBuf>,
    /// Pre-extracted facts as JSON, in place of a live adapter run.
    #[arg(long)]
    facts: Option<PathBuf>,
    /// The SQLite fact store to write. Defaults to an in-memory store.
    #[arg(long)]
    db: Option<PathBuf>,
}

fn main() -> ExitCode {
    match Cli::parse().cmd {
        Cmd::Run => match hinzu_core::run() {
            Ok(report) => {
                print!("{report}");
                ExitCode::SUCCESS
            }
            Err(e) => report_error(e),
        },
        Cmd::Check(args) => match check(args) {
            Ok(code) => code,
            Err(e) => report_error(e),
        },
    }
}

/// Print an error to stderr and exit non-zero.
fn report_error(e: anyhow::Error) -> ExitCode {
    eprintln!("error: {e:#}");
    ExitCode::FAILURE
}

/// The `hinzu check` flow. Loads facts (from `--facts` JSON for now — the Rust
/// adapter is a later phase), runs the engine and the policy check, prints the
/// report, and returns a non-zero code when violations are found so it is
/// usable as a CI gate.
fn check(args: CheckArgs) -> Result<ExitCode> {
    let facts = load_facts(&args)?;
    let policy = load_policy(&args)?;

    let outcome = hinzu_core::check_facts(facts, args.db.as_deref(), &policy)?;
    print!("{}", outcome.report);

    if outcome.violations == 0 {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

/// Load facts from the `--facts` JSON, or fail with an honest message when no
/// facts are given: the Rust adapter that would extract them live is a later
/// phase, so this phase never fakes an analysis.
fn load_facts(args: &CheckArgs) -> Result<FactSet> {
    match &args.facts {
        Some(path) => {
            let json = std::fs::read_to_string(path)
                .with_context(|| format!("reading facts from {}", path.display()))?;
            FactSet::from_json(&json)
                .with_context(|| format!("parsing facts from {}", path.display()))
        }
        None => anyhow::bail!(
            "no Rust adapter wired yet — run phase 2 (the StableMIR driver) to extract facts \
             from {}, or pass --facts <json> to analyze pre-extracted facts",
            args.path.display()
        ),
    }
}

/// Load the policy from `--policy`, else `hinzu.toml` in the target project,
/// else `hinzu.toml` at the current directory.
fn load_policy(args: &CheckArgs) -> Result<Policy> {
    let path = match &args.policy {
        Some(p) => p.clone(),
        None => default_policy_path(&args.path)?,
    };
    let src = std::fs::read_to_string(&path)
        .with_context(|| format!("reading policy from {}", path.display()))?;
    Policy::from_toml(&src)
}

/// Find the default policy file: `hinzu.toml` in the project, else in the
/// current directory.
fn default_policy_path(project: &Path) -> Result<PathBuf> {
    let in_project = project.join("hinzu.toml");
    if in_project.is_file() {
        return Ok(in_project);
    }
    let in_cwd = PathBuf::from("hinzu.toml");
    if in_cwd.is_file() {
        return Ok(in_cwd);
    }
    anyhow::bail!(
        "no policy found: pass --policy <hinzu.toml>, or add hinzu.toml to {}",
        project.display()
    )
}
