//! The hinzu CLI. A thin shell: parse argv, hand off to hinzu-core.

mod adapter_harness;
mod api_diff;
mod api_py;
mod api_rust;
mod api_ts;
mod go_adapter;
mod portdiff_config;
mod portdiff_html;
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
    /// Cross-language port-progress diff: match a SOURCE package's symbol graph +
    /// plan against a TARGET port's symbol graph, file by file, and report how
    /// much has actually been ported — surviving file decomposition & relocation.
    /// Config-driven and multi-package: `--config` selects the naming rules and
    /// per-package paths, `--package` picks one. Writes a JSON report and, with
    /// `--html`, a self-contained dashboard.
    PortDiff(PortDiffArgs),
    /// Emit stable JSON describing a package's PUBLIC interface: exported
    /// functions/methods with real signatures (params + types, return type,
    /// async-ness, a knowable error type), exported types/enums/traits/aliases/
    /// consts with their shapes, visibility, module path, and doc comments,
    /// grouped by module. Two consumers: porting (the source package's public
    /// API as the contract a port must match) and binding/agent tooling
    /// (deciding what a generated binding should expose). Rust is extracted via
    /// `rustdoc --output-format=json`; TypeScript and Python are later phases.
    Api(ApiArgs),
    /// Freerange-style numeric range analysis: infer the interval each value can
    /// hold and flag arithmetic hazards — integer divide-by-zero / remainder-by-
    /// zero today — as evidence-carrying facts (which function, which statement,
    /// and the divisor range that proves it). Emits deterministic JSON: per-
    /// function parameter/return ranges plus the hazards found. Intraprocedural.
    /// Rust bodies are extracted from MIR by the StableMIR driver; `--bodies`
    /// takes a pre-extracted body-fact JSON instead (no toolchain needed). Exits
    /// non-zero when a hazard is found, so it is usable as a CI gate.
    Ranges(RangesArgs),
    /// Grade a TARGET package's public surface against a SOURCE package's,
    /// item by item — typically a port against the contract it must match. Takes
    /// two `hinzu api` reports (`--source` / `--target`) and emits a stable JSON
    /// conformance grade: each source item is matched (name + kind + shape),
    /// signatureMismatch (matched but the shape differs), or missing; target-only
    /// items are surfaced as extra. Advisory and evidence-carrying — names are
    /// normalized with the port config's naming rules (`--config` / `--package`)
    /// so convention renames (`streamText` ↔ `stream_text`) don't false-miss.
    /// Complements `hinzu port-diff` (which bands file/graph progress): this
    /// grades public-surface conformance. See `notes/api-diff.md`.
    ApiDiff(api_diff::ApiDiffArgs),
}

#[derive(Parser)]
struct RangesArgs {
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
    /// A port-diff config TOML (see `hinzu port-diff --config`). When given with
    /// `--merge-package`, the plan additionally runs the split-not-merge invariant
    /// check against the package's current Rust target and attaches the resulting
    /// `merges` section — the same [`MergeReport`] port-diff surfaces — so a plan
    /// shows which target files ≥ 2 of this package's source files already merged
    /// into.
    #[arg(long)]
    merge_config: Option<PathBuf>,
    /// The package (in `--merge-config`) whose target the merge check runs against.
    #[arg(long)]
    merge_package: Option<String>,
    /// Override: a pre-extracted TARGET graph JSON for the merge check, skipping the
    /// live Rust extraction. Used only with `--merge-config` / `--merge-package`.
    #[arg(long)]
    merge_target_graph: Option<PathBuf>,
}

#[derive(Parser)]
struct PortDiffArgs {
    /// The multi-package port-diff config TOML (shared naming rules + a table per
    /// package). See `notes/port-diff.md` for the schema.
    #[arg(long)]
    config: PathBuf,
    /// Which package in the config to diff. Give this or `--all`; if neither is
    /// given, the available package names (and `--all`) are listed. Mutually
    /// exclusive with `--all`.
    #[arg(long)]
    package: Option<String>,
    /// Run port-diff for EVERY package in `--config` and emit a combined rollup
    /// JSON (`--out`) + a combined HTML dashboard (`--html`). Mutually exclusive
    /// with `--package` and with the pre-extracted `--source-graph` /
    /// `--source-plan` / `--target-graph` overrides (those are single-package;
    /// `--all` extracts per package). Use `--cache-dir` to make the per-package
    /// extraction reusable across runs. With `--from`, `--all` switches to the
    /// **cross-package closure**: a union source graph across every package is
    /// built, the entry point's closure taken over it (spanning package
    /// boundaries), and each closure file routed to its owning package's target
    /// crate — "what does this entry need, across all packages, and how much is
    /// ported".
    #[arg(long)]
    all: bool,
    /// A directory for per-package extracted graphs/plans, used only with `--all`.
    /// For each package, `<dir>/<pkg>-{source-graph,source-plan,target-graph}.json`
    /// is read when present (skipping that package's extraction) and written after
    /// a live extraction — so re-runs avoid the slow Rust/TypeScript re-extraction.
    #[arg(long)]
    cache_dir: Option<PathBuf>,
    /// Override: a pre-extracted SOURCE graph JSON (`hinzu graph --out`). Skips
    /// the live source extraction. Extraction is slow, so pre-extracting once and
    /// re-running the diff off the JSON is the common path.
    #[arg(long)]
    source_graph: Option<PathBuf>,
    /// Override: a pre-extracted SOURCE plan JSON (`hinzu plan --out`). Used only
    /// when the source is not `--from`-scoped; a scoped run always rebuilds the
    /// plan from the scoped graph.
    #[arg(long)]
    source_plan: Option<PathBuf>,
    /// Override: a pre-extracted TARGET graph JSON. Skips the live (slow) Rust
    /// extraction.
    #[arg(long)]
    target_graph: Option<PathBuf>,
    /// Scope the SOURCE to the dependency closure of an entry point before
    /// diffing: the report then covers only what this symbol (or file)
    /// transitively needs, and which of it is unported. Repeatable (the closure
    /// is the union). Same pattern rules as `hinzu graph --from`. With `--package`
    /// this is a single-package rooted view; with `--all` it becomes the
    /// cross-package closure (resolved over the union source graph, routed per
    /// package). The `--source-graph` / `--target-graph` overrides apply only to
    /// the single-package form.
    #[arg(long = "from")]
    from: Vec<String>,
    /// Compare the current report against a saved BASELINE report JSON and emit a
    /// port-progress DELTA instead of the plain report: which files advanced /
    /// regressed band, the per-band net movement, the symbol-match delta, and an
    /// overall verdict (`forward` / `mixed` / `backward` / `no_change`). The
    /// baseline must be the same report shape as the current mode — a single
    /// `PortDiffReport`, a `MultiPackageReport` (`--all`), or a
    /// `RootedCrossPackageReport` (`--all --from`); a mismatched shape is a clear
    /// error. `--out` then writes the delta JSON, and a concise human summary is
    /// printed to stderr. Save a baseline with a plain `--out` run at the parent
    /// commit, then `--compare` it after the commit to check the commit moved the
    /// port forward. See `notes/port-diff.md`.
    #[arg(long)]
    compare: Option<PathBuf>,
    /// Where to write the report JSON (or, with `--compare`, the delta JSON).
    /// Defaults to stdout.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Also write a self-contained HTML dashboard to this file.
    #[arg(long)]
    html: Option<PathBuf>,
    /// Run the **split-not-merge** invariant check across every package against the
    /// UNION of all target crates (only with `--all`). Each package's source files
    /// are matched against the merged target graph of every crate, so a source file
    /// that landed in *another* package's crate is visible — the combined
    /// `merges` then flags both file-merges (a target file drawing substantial
    /// content from ≥ 2 source files, cross-package when they span packages) and
    /// misplacements (a source file ported into a crate owned by another package).
    /// Heavier than a plain `--all` (every package sees every crate's symbols), so
    /// it is opt-in.
    #[arg(long)]
    merge_check: bool,
}

#[derive(Parser)]
struct ApiArgs {
    /// The project to analyze.
    path: PathBuf,
    /// Where to write the API JSON. Defaults to stdout.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Override the language auto-detected from the project markers
    /// (`rust`/`typescript`/`python`/`go`). Phase 1 implements Rust.
    #[arg(long)]
    lang: Option<String>,
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
        Cmd::PortDiff(args) => match port_diff_cmd(args) {
            Ok(code) => code,
            Err(e) => report_error(e),
        },
        Cmd::Api(args) => match api(args) {
            Ok(code) => code,
            Err(e) => report_error(e),
        },
        Cmd::Ranges(args) => match ranges(args) {
            Ok(code) => code,
            Err(e) => report_error(e),
        },
        Cmd::ApiDiff(args) => match api_diff::run(args) {
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

/// The `hinzu api` flow. Auto-detects the language from the project markers (or
/// takes `--lang`), dispatches to the per-language extractor, and writes the
/// public-API JSON to `--out` or stdout. All the file/process effects live in
/// the extractor (the CLI side); core only normalizes the parsed result. Rust is
/// extracted via `rustdoc --output-format=json`, TypeScript via the compiler-API
/// adapter in `--api` mode, and Python via ty over its LSP; Go is not yet
/// implemented and fails honestly rather than faking a surface.
fn api(args: ApiArgs) -> Result<ExitCode> {
    let language = detect_api_language(&args.path, args.lang.as_deref())?;
    let report = match language.as_str() {
        "rust" => api_rust::extract(&args.path, &args.path.display().to_string())?,
        "typescript" => api_ts::extract(&args.path, &args.path.display().to_string())?,
        "python" => api_py::extract(&args.path, &args.path.display().to_string())?,
        "go" => {
            anyhow::bail!("go api extraction is not yet implemented")
        }
        other => {
            anyhow::bail!("unknown --lang '{other}'; expected rust, typescript, python, or go")
        }
    };
    let json =
        serde_json::to_string_pretty(&report).context("serializing the API report to JSON")?;
    write_json(args.out.as_deref(), &json, "api report")
}

/// The `hinzu ranges` flow. Loads body facts (from `--bodies` JSON, or by
/// extracting them live with the StableMIR driver — all the file/process I/O is
/// on the CLI side), runs the pure abstract-interpretation engine, writes the
/// deterministic ranges-and-hazards JSON, and returns a non-zero code when a
/// hazard is found so it is usable as a CI gate.
fn ranges(args: RangesArgs) -> Result<ExitCode> {
    let bodies = if let Some(path) = args.bodies.as_deref() {
        let json = std::fs::read_to_string(path)
            .with_context(|| format!("reading body facts from {}", path.display()))?;
        hinzu_core::absint::body::BodyFacts::from_json(&json)
            .with_context(|| format!("parsing body facts from {}", path.display()))?
    } else {
        if !rust_adapter::is_cargo_project(&args.path) {
            anyhow::bail!(
                "{} is not a cargo project — `hinzu ranges` extracts Rust MIR bodies today; \
                 pass --bodies <json> to analyze pre-extracted facts",
                args.path.display()
            );
        }
        rust_adapter::extract_bodies(&args.path)?
    };

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

/// Resolve the language for `hinzu api`: an explicit `--lang` (lowercased), else
/// the project's marker file — Cargo.toml → rust, tsconfig/package.json →
/// typescript, pyproject/setup → python, go.mod → go (Rust wins a tie, matching
/// `route_facts`). Errors when nothing matches so the operator gets a clear
/// message instead of a silent empty report.
fn detect_api_language(path: &Path, lang: Option<&str>) -> Result<String> {
    if let Some(l) = lang {
        return Ok(l.to_lowercase());
    }
    if rust_adapter::is_cargo_project(path) {
        Ok("rust".to_string())
    } else if ts_adapter::is_ts_project(path) {
        Ok("typescript".to_string())
    } else if py_adapter::is_python_project(path) {
        Ok("python".to_string())
    } else if go_adapter::is_go_project(path) {
        Ok("go".to_string())
    } else {
        anyhow::bail!(
            "{} is not a cargo, TypeScript, Python, or Go project — pass --lang to force one",
            path.display()
        )
    }
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

    let mut plan = hinzu_core::plan::build_plan(
        &graph,
        hinzu_core::plan::PlanOpts {
            max_group_loc: args.group_max_loc,
            coalesce_small: !args.no_coalesce,
        },
    );

    // Optional split-not-merge check: match this source graph + plan against the
    // package's Rust target and attach the same MergeReport port-diff produces, so
    // the plan carries the invariant it is scheduling against.
    if let Some(merge_cfg) = &args.merge_config {
        let package = args.merge_package.as_deref().ok_or_else(|| {
            anyhow::anyhow!("--merge-config needs --merge-package <name> to pick the target")
        })?;
        let cfg = portdiff_config::MultiPackageConfig::load(merge_cfg)?;
        let resolved = cfg.resolve(package)?;
        let target_graph = match &args.merge_target_graph {
            Some(p) => load_graph(p)?,
            None => extract_target_graph_live(&resolved)?,
        };
        let report = hinzu_core::portdiff::port_diff(
            &graph,
            &plan,
            &target_graph,
            &resolved.config,
            read_conformance_manifest(&resolved.manifest_path).as_deref(),
        );
        plan.merges = Some(report.merges);
    }

    let json = serde_json::to_string_pretty(&plan).context("serializing the plan to JSON")?;
    write_json(args.out.as_deref(), &json, "plan")
}

/// The `hinzu port-diff` flow. Loads the multi-package config, selects one
/// package, obtains the source graph + plan and the target graph (each either
/// loaded from a pre-extracted JSON override or extracted live), optionally
/// scopes the source to a `--from` closure, reads the conformance manifest text
/// (the CLI does the file read — core stays a pure functional core), runs
/// [`hinzu_core::portdiff::port_diff`], writes the JSON report, and — with
/// `--html` — a self-contained dashboard.
fn port_diff_cmd(args: PortDiffArgs) -> Result<ExitCode> {
    let cfg = portdiff_config::MultiPackageConfig::load(&args.config)?;
    if args.merge_check && !args.all {
        anyhow::bail!(
            "--merge-check runs the cross-package split-not-merge check over every package \
             against the union of all target crates; it requires --all"
        );
    }
    if args.all {
        return if args.from.is_empty() {
            port_diff_all(&cfg, &args)
        } else {
            // `--all --from` is the cross-package closure: a union source graph,
            // one entry point's closure across package boundaries, routed per file.
            port_diff_cross_from(&cfg, &args)
        };
    }
    let package = match &args.package {
        Some(p) => p.clone(),
        None => anyhow::bail!(
            "select a package with --package <name>, or --all to sweep every package; \
             available packages: {}",
            cfg.package_names().join(", ")
        ),
    };
    let resolved = cfg.resolve(&package)?;

    // ---- source graph: load an override, else extract live ----------------
    let source_graph = match &args.source_graph {
        Some(p) => load_graph(p)?,
        None => {
            eprintln!(
                "extracting source graph from {}",
                resolved.source_path.display()
            );
            build_graph_from_source(&resolved.source_path, None, None)?
        }
    };
    // Scope the SOURCE to the `--from` closure BEFORE building the plan, so the
    // plan is "exactly what this entry point needs" and nothing else.
    let scoped = !args.from.is_empty();
    let source_graph = scope_to_closure(source_graph, &args.from)?;

    // ---- source plan: override only when unscoped, else derive from the graph
    let source_plan = match &args.source_plan {
        Some(p) if !scoped => load_plan(p)?,
        Some(_) => {
            eprintln!("note: --from scopes the source, so the plan is rebuilt from the closure");
            hinzu_core::plan::build_plan(&source_graph, hinzu_core::plan::PlanOpts::default())
        }
        None => hinzu_core::plan::build_plan(&source_graph, hinzu_core::plan::PlanOpts::default()),
    };

    // ---- target graph: load an override, else extract live ----------------
    let target_graph = match &args.target_graph {
        Some(p) => load_graph(p)?,
        None => extract_target_graph_live(&resolved)?,
    };

    // The conformance manifest is read HERE (the CLI is outside the functional
    // core, so a filesystem read is allowed); core only parses the text it is
    // handed. Best-effort: an unreadable manifest bands no file DONE.
    let manifest_json = read_conformance_manifest(&resolved.manifest_path);

    let report = hinzu_core::portdiff::port_diff(
        &source_graph,
        &source_plan,
        &target_graph,
        &resolved.config,
        manifest_json.as_deref(),
    );

    if let Some(html_path) = &args.html {
        let meta = portdiff_html::HtmlMeta {
            package: resolved.name.clone(),
            source_label: resolved.source_path.display().to_string(),
            target_label: resolved.target_path.display().to_string(),
            scoped_from: args.from.clone(),
            input_mode: if args.source_graph.is_some() || args.target_graph.is_some() {
                "pre-extracted graphs".to_string()
            } else {
                "extracted live".to_string()
            },
        };
        let html = portdiff_html::render_html(&report, &meta);
        std::fs::write(html_path, html)
            .with_context(|| format!("writing HTML dashboard to {}", html_path.display()))?;
        eprintln!("wrote HTML dashboard to {}", html_path.display());
    }

    if let Some(compare_path) = &args.compare {
        return emit_delta(
            compare_path,
            args.out.as_deref(),
            "single PortDiffReport",
            |baseline: &hinzu_core::portdiff::PortDiffReport| {
                hinzu_core::portdiff::diff_reports(baseline, &report)
            },
        );
    }

    let json = serde_json::to_string_pretty(&report)
        .context("serializing the port-diff report to JSON")?;
    write_json(args.out.as_deref(), &json, "port-diff report")
}

/// The `hinzu port-diff --all` flow. Runs port-diff for EVERY package in the
/// config — extracting each package's source graph + plan and target graph the
/// same way the single-package live path does (with the same honest
/// `HINZU_RUSTC_DRIVER` requirement for a Rust target) — aggregates the
/// per-package reports into a [`MultiPackageReport`], and writes the combined JSON
/// (`--out`) and, with `--html`, a combined dashboard. `--cache-dir` makes the
/// per-package extraction reusable across runs.
fn port_diff_all(
    cfg: &portdiff_config::MultiPackageConfig,
    args: &PortDiffArgs,
) -> Result<ExitCode> {
    // `--all` is whole-port and multi-package, so the single-package selectors and
    // overrides are rejected rather than silently ignored.
    if args.package.is_some() {
        anyhow::bail!("--all runs every package; drop --package (they are mutually exclusive)");
    }
    if args.source_graph.is_some() || args.source_plan.is_some() || args.target_graph.is_some() {
        anyhow::bail!(
            "--source-graph / --source-plan / --target-graph are single-package overrides; \
             --all extracts per package (use --cache-dir to reuse extractions across runs)"
        );
    }

    // Pass 1: gather each package's source graph + plan and target graph (cached
    // under --cache-dir when set). Held so the merge-check can build the UNION of
    // every target crate before any package is diffed.
    struct Gathered {
        resolved: portdiff_config::ResolvedPackage,
        source_graph: hinzu_core::graph::GraphOutput,
        source_plan: hinzu_core::plan::PlanOutput,
        target_graph: hinzu_core::graph::GraphOutput,
    }
    let mut gathered: Vec<Gathered> = Vec::new();
    for name in cfg.package_names() {
        let resolved = cfg.resolve(&name)?;
        eprintln!("=== extract {name} ===");

        // Source graph + plan, then target graph — each cached under --cache-dir
        // when set, else extracted live exactly like the single-package path.
        let source_graph =
            cached_or_extract(args.cache_dir.as_deref(), &name, "source-graph", || {
                eprintln!(
                    "extracting source graph from {}",
                    resolved.source_path.display()
                );
                build_graph_from_source(&resolved.source_path, None, None)
            })?;
        let source_plan = cached_or_build_plan(args.cache_dir.as_deref(), &name, &source_graph)?;
        let target_graph =
            cached_or_extract(args.cache_dir.as_deref(), &name, "target-graph", || {
                extract_target_graph_live(&resolved)
            })?;
        gathered.push(Gathered {
            resolved,
            source_graph,
            source_plan,
            target_graph,
        });
    }

    // With --merge-check, build the UNION of every package's target crates once, so
    // each package is matched against every crate and a source file ported into
    // another package's crate stays visible for the cross-package merge detector.
    let union_target = if args.merge_check {
        let crates: Vec<hinzu_core::graph::GraphOutput> =
            gathered.iter().map(|g| g.target_graph.clone()).collect();
        eprintln!(
            "merge-check: matching every package against the union of {} target-crate graph(s)",
            crates.len()
        );
        Some(merge_target_graphs(crates))
    } else {
        None
    };

    // Pass 2: diff each package. Against the union target (merge-check) or its own
    // target crate (plain --all).
    let mut reports: Vec<(String, hinzu_core::portdiff::PortDiffReport)> = Vec::new();
    for g in &gathered {
        let name = g.resolved.name.clone();
        eprintln!("=== port-diff {name} ===");
        let target_graph = union_target.as_ref().unwrap_or(&g.target_graph);
        let manifest_json = read_conformance_manifest(&g.resolved.manifest_path);
        let report = hinzu_core::portdiff::port_diff(
            &g.source_graph,
            &g.source_plan,
            target_graph,
            &g.resolved.config,
            manifest_json.as_deref(),
        );
        reports.push((name, report));
    }

    // Map each target crate to the package that owns it (config's per-package
    // primary/secondary crates), so the misplacement detector uses the declared
    // ownership rather than plurality. Crate name = the segment after `crates/`
    // in the package's `target_src_prefix` (`crates/pidgin-ai/src` → `pidgin-ai`).
    let mut owning_override: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for g in &gathered {
        let pkg = g.resolved.name.clone();
        for prefix in &g.resolved.config.naming.target_src_prefix {
            if let Some(cr) = prefix
                .strip_prefix("crates/")
                .and_then(|rest| rest.split('/').next())
            {
                if !cr.is_empty() {
                    owning_override.insert(cr.to_string(), pkg.clone());
                }
            }
        }
    }
    let multi = hinzu_core::portdiff::MultiPackageReport::aggregate(
        &cfg.source_kind,
        &cfg.target_kind,
        reports,
        &owning_override,
    );

    if let Some(html_path) = &args.html {
        let meta = portdiff_html::MultiHtmlMeta {
            source_label: format!("{} · base {}", cfg.source_kind, cfg.base_dir),
            target_label: cfg.target_kind.clone(),
            input_mode: match &args.cache_dir {
                Some(d) => format!("extracted per package (cache {})", d.display()),
                None => "extracted live per package".to_string(),
            },
        };
        let html = portdiff_html::render_multi_html(&multi, &meta);
        std::fs::write(html_path, html).with_context(|| {
            format!("writing combined HTML dashboard to {}", html_path.display())
        })?;
        eprintln!("wrote combined HTML dashboard to {}", html_path.display());
    }

    if let Some(compare_path) = &args.compare {
        return emit_delta(
            compare_path,
            args.out.as_deref(),
            "MultiPackageReport (--all)",
            |baseline: &hinzu_core::portdiff::MultiPackageReport| {
                hinzu_core::portdiff::diff_multi_reports(baseline, &multi)
            },
        );
    }

    let json =
        serde_json::to_string_pretty(&multi).context("serializing the combined report to JSON")?;
    write_json(args.out.as_deref(), &json, "combined port-diff report")
}

/// The `hinzu port-diff --all --from <entry>` flow: the **cross-package closure**.
///
/// Where single-package `--from` scopes one package's source, and `--all` sweeps
/// every package whole, this combines them: it builds a UNION source graph across
/// every `[packages.*]` (one monorepo-rooted extraction whose files already span
/// the packages — see [`find_union_root`]), takes the entry point's dependency
/// closure over it (so the closure crosses package boundaries, following call,
/// reference *and* signature-type edges), then routes each closure file to its
/// owning package by path prefix and matches that package's slice against its own
/// target crate. The result is a [`RootedCrossPackageReport`]: overall closure
/// size, how many packages it spans, and each package's slice banded DONE / PORTED
/// / STARTED / NOT-STARTED — "what does this entry need, across all packages, and
/// how much is left".
///
/// `--cache-dir` makes both the union extraction (`<dir>/union-source-graph.json`)
/// and each package's target graph reusable across runs. The single-package
/// overrides (`--source-graph` / `--source-plan` / `--target-graph`) are rejected:
/// they name one package's graphs, whereas this extracts the union + per package.
fn port_diff_cross_from(
    cfg: &portdiff_config::MultiPackageConfig,
    args: &PortDiffArgs,
) -> Result<ExitCode> {
    if args.package.is_some() {
        anyhow::bail!(
            "--all --from is the cross-package closure (it spans every package); drop --package \
             — use --package --from for a single-package rooted view instead"
        );
    }
    if args.source_graph.is_some() || args.source_plan.is_some() || args.target_graph.is_some() {
        anyhow::bail!(
            "--source-graph / --source-plan / --target-graph are single-package overrides; the \
             cross-package closure extracts a union source graph + a target graph per package \
             (use --cache-dir to reuse extractions across runs)"
        );
    }

    // Resolve every package, then find the extraction root whose tree contains all
    // of them (so cross-package imports resolve to local source, not externals).
    let resolved: Vec<portdiff_config::ResolvedPackage> = cfg
        .package_names()
        .iter()
        .map(|n| cfg.resolve(n))
        .collect::<Result<_>>()?;
    let source_paths: Vec<PathBuf> = resolved.iter().map(|r| r.source_path.clone()).collect();
    let union_root = find_union_root(&source_paths);
    eprintln!(
        "cross-package --from: union source root {}",
        union_root.display()
    );

    // The union source graph (cache-reusable), the closure over it, then routing.
    let union = cached_or_extract_union(args.cache_dir.as_deref(), &union_root)?;
    let resolution =
        hinzu_core::graph::resolve_roots(&union, &args.from).map_err(|e| anyhow::anyhow!(e))?;
    for note in &resolution.notes {
        eprintln!("{note}");
    }
    let closure = hinzu_core::graph::filter_to_closure(&union, &resolution.roots);
    eprintln!(
        "closure of {}: {} symbols across {} files (of {} in the union)",
        resolution.roots.join(", "),
        closure.stats.symbol_count,
        closure.stats.file_count,
        union.stats.symbol_count,
    );

    // Route each closure file to its owning package (path prefix = the package's
    // source dir relative to the union root), then diff that package's slice.
    let mut slices: Vec<(String, usize, usize, hinzu_core::portdiff::PortDiffReport)> = Vec::new();
    for r in &resolved {
        let Ok(rel) = r.source_path.strip_prefix(&union_root) else {
            continue;
        };
        let prefix = format!("{}/", rel.display());
        let closure_files = closure
            .files
            .iter()
            .filter(|f| f.path.starts_with(&prefix))
            .count();
        let closure_symbols = closure
            .symbols
            .iter()
            .filter(|s| {
                !s.external
                    && s.file
                        .as_deref()
                        .map(|f| f.starts_with(&prefix))
                        .unwrap_or(false)
            })
            .count();
        if closure_files == 0 {
            continue;
        }
        eprintln!(
            "=== {} : {closure_files} closure files, {closure_symbols} symbols (prefix {prefix}) ===",
            r.name
        );

        // Slice the closure to this package and re-root it to `src/…` so the
        // package's own PortDiffConfig matches it unchanged, then plan + diff it.
        let scoped_source = hinzu_core::graph::reroot_subgraph(&closure, &prefix);
        let scoped_plan =
            hinzu_core::plan::build_plan(&scoped_source, hinzu_core::plan::PlanOpts::default());
        let target_graph =
            cached_or_extract(args.cache_dir.as_deref(), &r.name, "target-graph", || {
                extract_target_graph_live(r)
            })?;
        let manifest_json = read_conformance_manifest(&r.manifest_path);
        let report = hinzu_core::portdiff::port_diff(
            &scoped_source,
            &scoped_plan,
            &target_graph,
            &r.config,
            manifest_json.as_deref(),
        );
        slices.push((r.name.clone(), closure_files, closure_symbols, report));
    }

    if slices.is_empty() {
        anyhow::bail!(
            "the --from closure matched no package source files under the union root {}; \
             check the entry pattern and the packages' source_dir paths",
            union_root.display()
        );
    }

    let rooted = hinzu_core::portdiff::RootedCrossPackageReport::aggregate(
        &cfg.source_kind,
        &cfg.target_kind,
        resolution.roots.clone(),
        closure.stats.symbol_count,
        closure.stats.file_count,
        slices,
    );

    if let Some(html_path) = &args.html {
        // Reuse the whole-port dashboard renderer over the closure's per-package
        // slices; the header carries the closure roots + union root.
        let multi = rooted.as_multi();
        let meta = portdiff_html::MultiHtmlMeta {
            source_label: format!(
                "{} · closure of {} · union {}",
                cfg.source_kind,
                resolution.roots.join(", "),
                union_root.display()
            ),
            target_label: cfg.target_kind.clone(),
            input_mode: match &args.cache_dir {
                Some(d) => format!("cross-package closure · cache {}", d.display()),
                None => "cross-package closure · extracted live".to_string(),
            },
        };
        let html = portdiff_html::render_multi_html(&multi, &meta);
        std::fs::write(html_path, html).with_context(|| {
            format!(
                "writing cross-package HTML dashboard to {}",
                html_path.display()
            )
        })?;
        eprintln!(
            "wrote cross-package HTML dashboard to {}",
            html_path.display()
        );
    }

    if let Some(compare_path) = &args.compare {
        return emit_delta(
            compare_path,
            args.out.as_deref(),
            "RootedCrossPackageReport (--all --from)",
            |baseline: &hinzu_core::portdiff::RootedCrossPackageReport| {
                hinzu_core::portdiff::diff_cross_reports(baseline, &rooted)
            },
        );
    }

    let json = serde_json::to_string_pretty(&rooted)
        .context("serializing the cross-package rooted report to JSON")?;
    write_json(args.out.as_deref(), &json, "cross-package rooted report")
}

/// The extraction root for a cross-package union source graph: the nearest
/// ancestor of every package's source dir that the adapters recognize as a project
/// (so its own tsconfig/Cargo resolves the workspace imports to local source, and
/// the extraction owns files across every package). Starts at the common ancestor
/// of the source dirs and walks up until a project marker is found; falls back to
/// the common ancestor itself if none is (the adapter still searches upward for a
/// config).
fn find_union_root(source_paths: &[PathBuf]) -> PathBuf {
    let base = common_ancestor(source_paths);
    let mut cur = base.clone();
    loop {
        if is_recognized_project(&cur) {
            return cur;
        }
        match cur.parent() {
            Some(p) if p != cur => cur = p.to_path_buf(),
            _ => return base,
        }
    }
}

/// The longest directory prefix shared by every path.
fn common_ancestor(paths: &[PathBuf]) -> PathBuf {
    let mut iter = paths.iter();
    let Some(first) = iter.next() else {
        return PathBuf::new();
    };
    let mut comps: Vec<std::path::Component> = first.components().collect();
    for p in iter {
        let pc: Vec<std::path::Component> = p.components().collect();
        let n = comps
            .iter()
            .zip(pc.iter())
            .take_while(|(a, b)| a == b)
            .count();
        comps.truncate(n);
    }
    comps.iter().collect()
}

/// Whether any adapter recognizes `path` as a project it can extract from — the
/// same marker checks `route_facts` routes on.
fn is_recognized_project(path: &Path) -> bool {
    rust_adapter::is_cargo_project(path)
        || ts_adapter::is_ts_project(path)
        || py_adapter::is_python_project(path)
        || go_adapter::is_go_project(path)
}

/// The union source graph: read from `<dir>/union-source-graph.json` when present,
/// else extract it live over `union_root` and cache it. Keeps the (slow)
/// monorepo-wide extraction reusable across cross-package `--from` runs.
fn cached_or_extract_union(
    cache_dir: Option<&Path>,
    union_root: &Path,
) -> Result<hinzu_core::graph::GraphOutput> {
    if let Some(dir) = cache_dir {
        let path = dir.join("union-source-graph.json");
        if path.is_file() {
            eprintln!("cache hit: {}", path.display());
            return load_graph(&path);
        }
        eprintln!(
            "extracting union source graph from {}",
            union_root.display()
        );
        let graph = build_graph_from_source(union_root, None, None)?;
        write_cache(dir, &path, &graph, "graph")?;
        return Ok(graph);
    }
    eprintln!(
        "extracting union source graph from {}",
        union_root.display()
    );
    build_graph_from_source(union_root, None, None)
}

/// Extract a package's target graph live from its `target_dir`, requiring the
/// StableMIR driver (`HINZU_RUSTC_DRIVER`) when the target is Rust — the same
/// honest failure the single-package and `--all` paths share rather than faking
/// an analysis. Pre-extract the target graph to skip this (single-package
/// `--target-graph`, or a populated `--cache-dir`).
fn extract_target_graph_live(
    resolved: &portdiff_config::ResolvedPackage,
) -> Result<hinzu_core::graph::GraphOutput> {
    if resolved.config.target_kind == "rust" && std::env::var_os("HINZU_RUSTC_DRIVER").is_none() {
        anyhow::bail!(
            "extracting the Rust target graph needs the StableMIR driver: set \
             HINZU_RUSTC_DRIVER to a prebuilt hinzu-rustc-driver binary (built on its \
             pinned nightly), or supply a pre-extracted target graph"
        );
    }
    // A source package may have been ported across several crates. Extract each
    // and merge the graphs into one target index; symbols keep their real
    // `crates/<crate>/…` file paths, so the `file.starts_with("crates/")` filter
    // and per-file attribution in core still work unchanged.
    let mut graphs = Vec::with_capacity(resolved.target_paths.len());
    for path in &resolved.target_paths {
        eprintln!("extracting target graph from {}", path.display());
        graphs.push(build_graph_from_source(path, None, None)?);
    }
    Ok(merge_target_graphs(graphs))
}

/// Concatenate several extracted target graphs into one. `port_diff` reads only
/// `symbols` and `edges`; the file rollups are merged too for completeness. The
/// crates occupy disjoint id namespaces (`crates/<crate>/…` paths, `<crate>::…`
/// ids), so a plain concatenation needs no de-duplication. A single-crate package
/// returns its one graph unchanged.
fn merge_target_graphs(
    graphs: Vec<hinzu_core::graph::GraphOutput>,
) -> hinzu_core::graph::GraphOutput {
    let mut iter = graphs.into_iter();
    let mut merged = iter
        .next()
        .expect("a resolved package always has at least one target crate");
    for g in iter {
        merged.symbols.extend(g.symbols);
        merged.edges.extend(g.edges);
        merged.files.extend(g.files);
        merged.file_edges.extend(g.file_edges);
    }
    merged
}

/// Read a per-package graph from the cache (`<dir>/<pkg>-<kind>.json`) when it is
/// present, else run `extract`, write the result to the cache (when a cache dir is
/// set), and return it. `kind` is `"source-graph"` or `"target-graph"`.
fn cached_or_extract(
    cache_dir: Option<&Path>,
    pkg: &str,
    kind: &str,
    extract: impl FnOnce() -> Result<hinzu_core::graph::GraphOutput>,
) -> Result<hinzu_core::graph::GraphOutput> {
    if let Some(dir) = cache_dir {
        let path = dir.join(format!("{pkg}-{kind}.json"));
        if path.is_file() {
            eprintln!("cache hit: {}", path.display());
            return load_graph(&path);
        }
        let graph = extract()?;
        write_cache(dir, &path, &graph, "graph")?;
        return Ok(graph);
    }
    extract()
}

/// The source plan: read from `<dir>/<pkg>-source-plan.json` when present, else
/// build it from the source graph and cache it. Keeps `--all` reproducible without
/// re-deriving the plan on every run.
fn cached_or_build_plan(
    cache_dir: Option<&Path>,
    pkg: &str,
    source_graph: &hinzu_core::graph::GraphOutput,
) -> Result<hinzu_core::plan::PlanOutput> {
    if let Some(dir) = cache_dir {
        let path = dir.join(format!("{pkg}-source-plan.json"));
        if path.is_file() {
            eprintln!("cache hit: {}", path.display());
            return load_plan(&path);
        }
        let plan =
            hinzu_core::plan::build_plan(source_graph, hinzu_core::plan::PlanOpts::default());
        write_cache(dir, &path, &plan, "plan")?;
        return Ok(plan);
    }
    Ok(hinzu_core::plan::build_plan(
        source_graph,
        hinzu_core::plan::PlanOpts::default(),
    ))
}

/// Write a cache artifact as pretty JSON, creating the cache dir if needed.
fn write_cache<T: serde::Serialize>(dir: &Path, path: &Path, value: &T, what: &str) -> Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating cache dir {}", dir.display()))?;
    let json = serde_json::to_string_pretty(value)
        .with_context(|| format!("serializing cached {what}"))?;
    std::fs::write(path, format!("{json}\n"))
        .with_context(|| format!("writing cached {what} to {}", path.display()))?;
    eprintln!("cached {} → {}", what, path.display());
    Ok(())
}

/// Read a pre-extracted [`GraphOutput`] JSON.
fn load_graph(path: &Path) -> Result<hinzu_core::graph::GraphOutput> {
    let json = std::fs::read_to_string(path)
        .with_context(|| format!("reading graph from {}", path.display()))?;
    serde_json::from_str(&json).with_context(|| format!("parsing graph from {}", path.display()))
}

/// Read a pre-extracted [`PlanOutput`] JSON.
fn load_plan(path: &Path) -> Result<hinzu_core::plan::PlanOutput> {
    let json = std::fs::read_to_string(path)
        .with_context(|| format!("reading plan from {}", path.display()))?;
    serde_json::from_str(&json).with_context(|| format!("parsing plan from {}", path.display()))
}

/// Read the conformance manifest text, best-effort. An unreadable manifest is a
/// warning (no file is banded DONE), not a hard error — the structural bands are
/// still meaningful without the test-verified oracle.
fn read_conformance_manifest(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Some(t),
        Err(e) => {
            eprintln!(
                "warning: conformance manifest {} unreadable ({e}); no file will be banded DONE",
                path.display()
            );
            None
        }
    }
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
pub(crate) fn write_json(out: Option<&Path>, json: &str, what: &str) -> Result<ExitCode> {
    match out {
        Some(out) => {
            std::fs::write(out, format!("{json}\n"))
                .with_context(|| format!("writing {what} to {}", out.display()))?;
        }
        None => println!("{json}"),
    }
    Ok(ExitCode::SUCCESS)
}

/// Load and deserialize a baseline report JSON of the shape the current
/// `--compare` mode expects. A parse failure is reported as a shape mismatch,
/// since the usual cause is comparing against a baseline saved in a different
/// mode (a single report vs `--all` vs `--all --from`).
fn load_baseline<T: serde::de::DeserializeOwned>(path: &Path, shape: &str) -> Result<T> {
    let text = std::fs::read_to_string(path).with_context(|| {
        format!(
            "reading the --compare baseline report from {}",
            path.display()
        )
    })?;
    serde_json::from_str(&text).with_context(|| {
        format!(
            "parsing the --compare baseline at {} as a {shape} — the baseline must be the same \
             report shape as the current mode (a single report, --all, or --all --from)",
            path.display()
        )
    })
}

/// The band's human label, matching the report's serde spelling.
fn band_label(band: hinzu_core::portdiff::Band) -> &'static str {
    use hinzu_core::portdiff::Band;
    match band {
        Band::Done => "DONE",
        Band::Ported => "PORTED",
        Band::Relocated => "RELOCATED",
        Band::Started => "STARTED",
        Band::NotStarted => "NOT-STARTED",
    }
}

/// Print a concise, human-readable one-line delta summary to stderr, e.g.
/// `port moved FORWARD: 4 files advanced (3 NOT-STARTED→PORTED, 1 STARTED→DONE),
/// +37 symbols matched, 0 regressions`.
fn print_delta_summary(delta: &hinzu_core::portdiff::PortDiffDelta) {
    use hinzu_core::portdiff::Verdict;
    let t = &delta.totals;
    let verdict = match delta.verdict {
        Verdict::Forward => "FORWARD",
        Verdict::Mixed => "MIXED",
        Verdict::Backward => "BACKWARD",
        Verdict::NoChange => "NO CHANGE",
    };
    let transitions: Vec<String> = t
        .transitions
        .iter()
        .map(|tr| {
            format!(
                "{} {}→{}",
                tr.count,
                band_label(tr.band_before),
                band_label(tr.band_after)
            )
        })
        .collect();
    let breakdown = if transitions.is_empty() {
        String::new()
    } else {
        format!(" ({})", transitions.join(", "))
    };
    let sym = t.symbols_matched_delta;
    let sym_sign = if sym >= 0 { "+" } else { "" };
    let mut tail = format!(
        "{} files advanced{breakdown}, {sym_sign}{sym} symbols matched, {} regressions",
        t.advanced, t.regressed
    );
    if t.added > 0 || t.removed > 0 {
        tail.push_str(&format!(" ({} added, {} removed)", t.added, t.removed));
    }
    eprintln!("port moved {verdict}: {tail}");
}

/// Emit the `--compare` delta: load the baseline of shape `shape`, run `diff`,
/// print the stderr summary, and write the delta JSON to `--out` (or stdout).
/// Shared tail of every `--compare` branch so the three report modes emit the
/// delta identically.
fn emit_delta<T, F>(
    compare_path: &Path,
    out: Option<&Path>,
    shape: &str,
    diff: F,
) -> Result<ExitCode>
where
    T: serde::de::DeserializeOwned,
    F: FnOnce(&T) -> hinzu_core::portdiff::PortDiffDelta,
{
    let baseline: T = load_baseline(compare_path, shape)?;
    let delta = diff(&baseline);
    print_delta_summary(&delta);
    let json =
        serde_json::to_string_pretty(&delta).context("serializing the port-progress delta")?;
    write_json(out, &json, "port-progress delta")
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
