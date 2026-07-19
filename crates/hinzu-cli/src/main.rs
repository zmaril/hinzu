//! The hinzu CLI. A thin shell: parse argv, hand off to hinzu-core.

mod adapter_harness;
mod go_adapter;
mod py_adapter;
mod rust_adapter;
mod ts_adapter;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use hinzu_core::effects::{EffectEngine, NaiveEngine};
use hinzu_core::facts::{FactSet, Language};
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
    /// Emit a JSON dependency graph of a project, for AI-assisted porting: call
    /// edges at the symbol level, module-dependency edges at the file level. Port
    /// leaves (no dependencies) first, then symbols whose dependencies are all
    /// ported. The graph may contain cycles; the acyclic SCC-condensation gives
    /// the port order. No policy is needed — it does not run the propagation gate.
    Graph(GraphArgs),
    /// Emit a JSON porting plan: the dependency graph organized into file-level
    /// groups (a PR per group; cycles ported together) and topological waves (a
    /// wave is a batch of groups with no dependency between them, portable in
    /// parallel). Reuses `hinzu graph`, or loads a previously emitted graph.json.
    Plan(PlanArgs),
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

#[derive(Parser)]
struct GraphArgs {
    /// The project to analyze.
    path: PathBuf,
    /// Pre-extracted facts as JSON, in place of a live adapter run.
    #[arg(long)]
    facts: Option<PathBuf>,
    /// An existing SQLite fact store to read facts from, in place of a live run.
    #[arg(long)]
    db: Option<PathBuf>,
    /// Scope the graph to the dependency closure of an entry point: only what
    /// this symbol (or file) transitively depends on, and nothing else.
    /// Repeatable — the closure is the union of every root. A pattern resolves
    /// as: exact symbol id, then id-suffix / display name, then id substring,
    /// then a file path (all its symbols). Errors if it matches nothing.
    #[arg(long = "from")]
    from: Vec<String>,
    /// Where to write the graph JSON. Defaults to stdout.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Parser)]
struct PlanArgs {
    /// The project to analyze.
    path: PathBuf,
    /// Pre-extracted facts as JSON, in place of a live adapter run.
    #[arg(long)]
    facts: Option<PathBuf>,
    /// An existing SQLite fact store to read facts from, in place of a live run.
    #[arg(long)]
    db: Option<PathBuf>,
    /// A previously emitted graph JSON (`hinzu graph --out`). When given, the plan
    /// is built straight from it — no facts are extracted.
    #[arg(long)]
    graph: Option<PathBuf>,
    /// Scope the plan to the dependency closure of an entry point: the plan then
    /// covers only what this symbol (or file) transitively needs to run, in port
    /// order — "everything main() depends on, and nothing else". Repeatable (the
    /// closure is the union). Same pattern rules as `hinzu graph --from`.
    #[arg(long = "from")]
    from: Vec<String>,
    /// Where to write the plan JSON. Defaults to stdout.
    #[arg(long)]
    out: Option<PathBuf>,
    /// The loc ceiling a coalesced group is kept under.
    #[arg(long, default_value_t = 200)]
    group_max_loc: usize,
    /// Disable small-file coalescing: group by dependency cycles only.
    #[arg(long)]
    no_coalesce: bool,
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
        Cmd::Graph(args) => match graph(args) {
            Ok(code) => code,
            Err(e) => report_error(e),
        },
        Cmd::Plan(args) => match plan(args) {
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
    // Seed from the language's own built-in annotation base: `std.toml` for Rust,
    // `node.toml` for TypeScript. The language is read from the facts themselves,
    // so `--facts` JSON routes the same way a live extraction does.
    let seeds = RootSeeds::from_toml_for(facts_language(&facts), &policy_src)?;
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

/// The `hinzu graph` flow. Resolves a fact set (from `--facts` JSON, an existing
/// `--db` store, or a live adapter run), seeds effect roots best-effort from the
/// language's built-in annotations (no policy needed), builds the porting
/// dependency graph, and writes it as pretty JSON to `--out` or stdout.
fn graph(args: GraphArgs) -> Result<ExitCode> {
    let graph = build_graph_from_source(&args.path, args.facts.as_deref(), args.db.as_deref())?;
    let graph = scope_to_closure(graph, &args.from)?;
    let json = serde_json::to_string_pretty(&graph).context("serializing the graph to JSON")?;
    write_json(args.out.as_deref(), &json, "graph")
}

/// The `hinzu plan` flow. Builds (or loads) the porting dependency graph,
/// organizes it into file-level groups and topological waves, and writes the plan
/// as pretty JSON. With `--graph <file>`, a previously emitted graph is
/// deserialized directly, so no facts are extracted; otherwise facts are resolved
/// exactly as `hinzu graph` does.
fn plan(args: PlanArgs) -> Result<ExitCode> {
    let graph = match args.graph.as_deref() {
        Some(graph_path) => {
            let json = std::fs::read_to_string(graph_path)
                .with_context(|| format!("reading graph from {}", graph_path.display()))?;
            serde_json::from_str::<hinzu_core::graph::GraphOutput>(&json)
                .with_context(|| format!("parsing graph from {}", graph_path.display()))?
        }
        None => build_graph_from_source(&args.path, args.facts.as_deref(), args.db.as_deref())?,
    };
    let graph = scope_to_closure(graph, &args.from)?;

    let plan = hinzu_core::plan::build_plan(
        &graph,
        hinzu_core::plan::PlanOpts {
            max_group_loc: args.group_max_loc,
            coalesce_small: !args.no_coalesce,
        },
    );
    let json = serde_json::to_string_pretty(&plan).context("serializing the plan to JSON")?;
    write_json(args.out.as_deref(), &json, "plan")
}

/// Scope a freshly built graph to the transitive dependency closure of the
/// `--from` roots, printing a one-line stderr note (plus any ambiguous-match
/// notes) so the operator sees what it resolved to. Returns the graph unchanged
/// when `from` is empty, so behavior is identical without `--from`.
fn scope_to_closure(
    graph: hinzu_core::graph::GraphOutput,
    from: &[String],
) -> Result<hinzu_core::graph::GraphOutput> {
    if from.is_empty() {
        return Ok(graph);
    }
    let resolution =
        hinzu_core::graph::resolve_roots(&graph, from).map_err(|e| anyhow::anyhow!(e))?;
    for note in &resolution.notes {
        eprintln!("{note}");
    }
    let total = graph.stats.symbol_count;
    let scoped = hinzu_core::graph::filter_to_closure(&graph, &resolution.roots);
    eprintln!(
        "scoped to closure of {}: {} symbols across {} files (of {})",
        resolution.roots.join(", "),
        scoped.stats.symbol_count,
        scoped.stats.file_count,
        total
    );
    Ok(scoped)
}

/// Resolve a fact set (from `--facts` JSON, an existing `--db` store, or a live
/// adapter run), seed effect roots best-effort from the language's built-in
/// annotations (no policy needed), and build the porting dependency graph. Shared
/// by `hinzu graph` and `hinzu plan` so both extract and build identically.
fn build_graph_from_source(
    path: &Path,
    facts: Option<&Path>,
    db: Option<&Path>,
) -> Result<hinzu_core::graph::GraphOutput> {
    // A `--db` store is a valid offline source, like `--facts`; otherwise route
    // by marker (or the given `--facts`).
    let mut facts = match db {
        Some(db) if facts.is_none() => hinzu_core::store::Store::open(db)
            .and_then(|s| s.load_facts())
            .with_context(|| format!("loading facts from store {}", db.display()))?,
        _ => route_facts(path, facts)?,
    };

    // Seed effect roots so the per-symbol `effect_roots` field is populated, the
    // same built-in annotation base `hinzu check` starts from — but policy-free:
    // no `[roots]`/`[trust]` overrides, so it stays best-effort. `seed_unknowns`
    // also marks unresolved externals, sharpening edge provenance.
    let language = facts_language(&facts);
    RootSeeds::for_language(language).seed_unknowns(&mut facts);

    Ok(hinzu_core::graph::build_graph(
        &facts,
        &path.display().to_string(),
        Some(language.as_str()),
    ))
}

/// Write pretty JSON to `out` (with a trailing newline) or stdout. `what` names
/// the document in any I/O error.
fn write_json(out: Option<&Path>, json: &str, what: &str) -> Result<ExitCode> {
    match out {
        Some(out) => {
            std::fs::write(out, format!("{json}\n"))
                .with_context(|| format!("writing {what} to {}", out.display()))?;
        }
        None => println!("{json}"),
    }
    Ok(ExitCode::SUCCESS)
}

/// Load facts from the `--facts` JSON (the offline path), or extract them live
/// by running the StableMIR driver over the target cargo project. When the path
/// is not a cargo project and no facts are given, fail honestly rather than
/// faking an analysis.
fn load_facts(args: &CheckArgs) -> Result<FactSet> {
    route_facts(&args.path, args.facts.as_deref())
}

/// Route to a fact source: the `--facts` JSON when given, else a live adapter
/// run chosen by the project's marker file. Shared by `hinzu check` and
/// `hinzu graph` so both resolve facts identically.
fn route_facts(path: &Path, facts: Option<&Path>) -> Result<FactSet> {
    if let Some(facts) = facts {
        let json = std::fs::read_to_string(facts)
            .with_context(|| format!("reading facts from {}", facts.display()))?;
        return FactSet::from_json(&json)
            .with_context(|| format!("parsing facts from {}", facts.display()));
    }
    // A Cargo.toml routes to the Rust StableMIR path; a tsconfig/package.json to
    // the TypeScript compiler-API adapter; a pyproject/setup.py/setup.cfg to the
    // ty-driven Python adapter; a go.mod to the gopls-driven Go adapter. Rust
    // wins a tie so a Rust crate with a stray package.json is not misrouted.
    if rust_adapter::is_cargo_project(path) {
        return rust_adapter::extract_facts(path)
            .with_context(|| format!("extracting Rust facts from {}", path.display()));
    }
    if ts_adapter::is_ts_project(path) {
        return ts_adapter::extract_facts(path)
            .with_context(|| format!("extracting TypeScript facts from {}", path.display()));
    }
    if py_adapter::is_python_project(path) {
        return py_adapter::extract_facts(path)
            .with_context(|| format!("extracting Python facts from {}", path.display()));
    }
    if go_adapter::is_go_project(path) {
        return go_adapter::extract_facts(path)
            .with_context(|| format!("extracting Go facts from {}", path.display()));
    }
    anyhow::bail!(
        "{} is not a cargo, TypeScript, Python, or Go project — pass --facts <json> to analyze \
         pre-extracted facts",
        path.display()
    )
}

/// The language to seed effect roots for: whichever non-Rust language any
/// definition declares (TypeScript, Python, or Go), else Rust. Reading it from
/// the facts keeps `--facts` JSON and a live extraction on the same path.
fn facts_language(facts: &FactSet) -> Language {
    facts
        .defs
        .values()
        .map(|d| d.language)
        .find(|l| *l != Language::Rust)
        .unwrap_or(Language::Rust)
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
