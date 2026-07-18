//! The hinzu CLI. A thin shell: parse argv, hand off to hinzu-core.

mod rust_adapter;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use hinzu_core::effects::{EffectEngine, NaiveEngine};
use hinzu_core::facts::FactSet;
use hinzu_core::policy::{OnUnknown, Policy};
use hinzu_core::roots::RootSeeds;
use hinzu_dbsp::DbspEngine;

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
    /// The propagation engine: `dbsp` (default) or the reference `naive` BFS.
    #[arg(long, value_enum, default_value_t = Engine::Dbsp)]
    engine: Engine,
}

/// Which propagation engine `hinzu check` runs. Both produce the same effect
/// sets; `dbsp` is the incremental-capable engine, `naive` the reference BFS.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum Engine {
    Dbsp,
    Naive,
}

impl Engine {
    /// Dispatch to the chosen engine behind the shared `EffectEngine` seam.
    fn run(
        self,
        facts: FactSet,
        db: Option<&Path>,
        policy: &Policy,
    ) -> Result<hinzu_core::CheckOutcome> {
        let engine: &dyn EffectEngine = match self {
            Engine::Dbsp => &DbspEngine,
            Engine::Naive => &NaiveEngine,
        };
        hinzu_core::check_facts(facts, db, policy, engine)
    }
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

/// The `hinzu check` flow. Loads facts (from `--facts` JSON, or by extracting
/// them live with the StableMIR driver), seeds effect roots, runs the engine
/// and the policy check, prints the report, and returns a non-zero code when
/// violations are found so it is usable as a CI gate.
fn check(args: CheckArgs) -> Result<ExitCode> {
    let mut facts = load_facts(&args)?;

    // The policy file carries both the region rules and the `[roots]` seed
    // table, so read it once and parse both. Seeding turns edges into a
    // registry dependency (say `rusqlite::…`) into effect roots before
    // propagation runs, so effects that leave the workspace still light up.
    let policy_src = read_policy_src(&args)?;
    let policy = Policy::from_toml(&policy_src)?;
    let seeds = RootSeeds::from_toml(&policy_src)?;
    // Under `on_unknown = ignore` an unseen external is read as pure (the old
    // behavior), so seed effect roots only. Otherwise also seed an `Unknown`
    // root for every unseen callee, so uncertainty propagates and is reported.
    if policy.on_unknown == OnUnknown::Ignore {
        seeds.seed(&mut facts);
    } else {
        seeds.seed_unknowns(&mut facts);
    }

    let outcome = args.engine.run(facts, args.db.as_deref(), &policy)?;
    print!("{}", outcome.report);

    if outcome.violations == 0 {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

/// Load facts from the `--facts` JSON (the offline path), or extract them live
/// by running the StableMIR driver over the target cargo project. When the path
/// is not a cargo project and no facts are given, fail honestly rather than
/// faking an analysis.
fn load_facts(args: &CheckArgs) -> Result<FactSet> {
    if let Some(path) = &args.facts {
        let json = std::fs::read_to_string(path)
            .with_context(|| format!("reading facts from {}", path.display()))?;
        return FactSet::from_json(&json)
            .with_context(|| format!("parsing facts from {}", path.display()));
    }
    if rust_adapter::is_cargo_project(&args.path) {
        return rust_adapter::extract_facts(&args.path)
            .with_context(|| format!("extracting Rust facts from {}", args.path.display()));
    }
    anyhow::bail!(
        "{} is not a cargo project — pass --facts <json> to analyze pre-extracted facts",
        args.path.display()
    )
}

/// Read the policy file source from `--policy`, else `hinzu.toml` in the target
/// project, else `hinzu.toml` at the current directory. The caller parses the
/// regions and the `[roots]` seed table from the same string.
fn read_policy_src(args: &CheckArgs) -> Result<String> {
    let path = match &args.policy {
        Some(p) => p.clone(),
        None => default_policy_path(&args.path)?,
    };
    std::fs::read_to_string(&path)
        .with_context(|| format!("reading policy from {}", path.display()))
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
