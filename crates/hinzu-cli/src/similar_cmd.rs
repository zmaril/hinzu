//! The `hinzu similar` subcommand: its clap arg struct, handler, and the
//! similar-only glue (signature-doc collection, per-language merge, and the
//! stderr summary). The file/process I/O lives in the adapters
//! ([`crate::structural_rust`], [`crate::ts_adapter`]); this module is the thin
//! seam that resolves signatures, runs the pure similarity engine
//! ([`hinzu_core::similarity`]), and writes the report.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};

use crate::write_json;
use crate::{rust_adapter, structural_rust, ts_adapter};

#[derive(Parser)]
pub struct SimilarArgs {
    /// The project to analyze: a cargo project, a TypeScript project, or a repo
    /// containing either (or both — a mixed repo runs both extractors). Defaults
    /// to the current directory. Ignored when `--structural` is given.
    #[arg(default_value = ".")]
    path: PathBuf,
    /// Pre-extracted structural signatures JSON (an extractor's
    /// `{language, extractor, signatures}` document), in place of a live
    /// extraction. This is the offline path — it needs no toolchain.
    #[arg(long)]
    structural: Option<PathBuf>,
    /// The clustering threshold: a pair at or above this similarity is an edge.
    #[arg(long, default_value_t = 0.55)]
    min_similarity: f64,
    /// The cohesion gate: the minimum mean pairwise similarity a reported cluster
    /// must reach. A loose, transitively-chained cluster below this is split at
    /// this higher bar or rejected, never emitted as a mega-blob.
    #[arg(long, default_value_t = 0.6)]
    min_cohesion: f64,
    /// The minimum normalized size (node-kind count) a signature must have to be
    /// considered — trivial defs are filtered out.
    #[arg(long, default_value_t = 12)]
    min_size: u32,
    /// Only analyze signatures in this language (`rust` or `typescript`).
    #[arg(long)]
    language: Option<String>,
    /// Which Rust structural extractor to use: `auto` (default) uses the resolved
    /// StableMIR driver when it is available and falls back to the syntactic `syn`
    /// extractor otherwise; `syn` forces the toolchain-free syntactic path;
    /// `stablemir` forces the resolved-type path and errors honestly if the driver
    /// is unavailable (never silently downgrading). Ignored for a TypeScript
    /// project and for `--structural`.
    #[arg(long, value_enum, default_value_t = RustExtractor::Auto)]
    rust_extractor: RustExtractor,
    /// Where to write the similarity JSON. Defaults to stdout.
    #[arg(long)]
    out: Option<PathBuf>,
}

/// Which Rust structural extractor `hinzu similar` uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum RustExtractor {
    /// Resolved StableMIR when available, else the syntactic `syn` fallback.
    Auto,
    /// The syntactic `syn` extractor (toolchain-free).
    Syn,
    /// The resolved-type StableMIR driver (errors if unavailable).
    Stablemir,
}

/// The `hinzu similar` flow. Resolves structural signatures (from a
/// pre-extracted `--structural` JSON, else a live `syn` extraction over a cargo
/// project), runs the pure similarity engine, writes the JSON document to `--out`
/// or stdout, and prints a human summary to stderr (mirroring `port-diff` /
/// `graph`). When the path is not a cargo project and no `--structural` is given,
/// it fails honestly rather than faking an analysis.
pub fn run(args: SimilarArgs) -> Result<ExitCode> {
    let docs = collect_signature_docs(&args)?;
    let root = match &args.structural {
        Some(p) => p.display().to_string(),
        None => args.path.display().to_string(),
    };

    let params = hinzu_core::similarity::AnalyzeParams {
        min_similarity: args.min_similarity,
        min_cohesion: args.min_cohesion,
        min_size: args.min_size,
        min_statements: hinzu_core::similarity::SimilarityParams::default().min_statements,
        language_filter: args.language.clone(),
        extractor: None,
    };

    // Analyze each language's signatures independently and merge the results, so
    // candidates never cross a language boundary (cross-language matching is out
    // of scope for v1). A single-language project is the common one-doc case. The
    // doc's own extractor is threaded in so each run reports the RIGHT profile
    // (resolved `stablemir` vs syntactic `syn`) and applies the matching
    // confidence cap.
    let outputs: Vec<hinzu_core::similarity::SimilarityOutput> = docs
        .into_iter()
        .map(|doc| {
            let doc_params = hinzu_core::similarity::AnalyzeParams {
                extractor: Some(doc.extractor.clone()),
                ..params.clone()
            };
            hinzu_core::similarity::analyze(&root, doc.signatures, &doc_params)
        })
        .collect();
    let output = merge_similarity_outputs(&root, outputs, &params);

    print_similarity_summary(&output);

    let json = serde_json::to_string_pretty(&output)
        .context("serializing the similarity report to JSON")?;
    write_json(args.out.as_deref(), &json, "similarity report")
}

/// Merge the per-language similarity outputs into one document: union the
/// languages and profiles, sum the stats, and concatenate the candidates
/// (re-sorted by confidence and re-numbered). One output passes through
/// unchanged except for the shared `root`.
fn merge_similarity_outputs(
    root: &str,
    outputs: Vec<hinzu_core::similarity::SimilarityOutput>,
    params: &hinzu_core::similarity::AnalyzeParams,
) -> hinzu_core::similarity::SimilarityOutput {
    use std::collections::{BTreeMap, BTreeSet};

    let mut languages: BTreeSet<String> = BTreeSet::new();
    let mut profiles: BTreeMap<(String, String), hinzu_core::similarity::LanguageProfile> =
        BTreeMap::new();
    let mut stats = hinzu_core::similarity::SimilarityStats {
        signatures_analyzed: 0,
        signatures_after_filter: 0,
        pairs_compared: 0,
        pairs_over_threshold: 0,
        clusters_rejected_low_cohesion: 0,
        candidates_found: 0,
    };
    let mut candidates: Vec<hinzu_core::similarity::Finding> = Vec::new();

    for o in outputs {
        for l in o.languages {
            languages.insert(l);
        }
        for p in o.profiles {
            profiles.insert((p.language.clone(), p.extractor.clone()), p);
        }
        stats.signatures_analyzed += o.stats.signatures_analyzed;
        stats.signatures_after_filter += o.stats.signatures_after_filter;
        stats.pairs_compared += o.stats.pairs_compared;
        stats.pairs_over_threshold += o.stats.pairs_over_threshold;
        stats.clusters_rejected_low_cohesion += o.stats.clusters_rejected_low_cohesion;
        candidates.extend(o.candidates);
    }

    // Re-sort and re-mint stable ids across the merged set, using the same
    // ordering the core analyzer applies to a single-language run.
    hinzu_core::similarity::sort_and_number_findings(&mut candidates);
    stats.candidates_found = candidates.len();

    hinzu_core::similarity::SimilarityOutput {
        hinzu_similarity_version: hinzu_core::similarity::HINZU_SIMILARITY_VERSION,
        root: root.to_string(),
        languages: languages.into_iter().collect(),
        profiles: profiles.into_values().collect(),
        params: hinzu_core::similarity::SimilarityParams {
            min_similarity: params.min_similarity,
            min_cohesion: params.min_cohesion,
            min_size: params.min_size,
            min_statements: params.min_statements,
            language_filter: params.language_filter.clone(),
        },
        stats,
        candidates,
    }
}

/// Resolve the structural-signature documents for `hinzu similar`. Reads the
/// pre-extracted `--structural` document when given (one doc). Otherwise extracts
/// live: the `syn` extractor for a cargo project and/or the `tsc-checker`
/// extractor for each TypeScript project found (both, for a mixed repo). A path
/// that is neither a cargo nor a TypeScript project, without `--structural`,
/// fails honestly rather than faking an analysis.
fn collect_signature_docs(args: &SimilarArgs) -> Result<Vec<hinzu_core::similarity::SignatureDoc>> {
    if let Some(path) = &args.structural {
        let json = std::fs::read_to_string(path)
            .with_context(|| format!("reading structural signatures from {}", path.display()))?;
        let doc = serde_json::from_str(&json)
            .with_context(|| format!("parsing structural signatures from {}", path.display()))?;
        return Ok(vec![doc]);
    }

    let mut docs = Vec::new();
    if rust_adapter::is_cargo_project(&args.path) {
        docs.push(extract_rust_signatures(&args.path, args.rust_extractor)?);
    }
    for project in ts_adapter::find_ts_projects(&args.path) {
        docs.push(ts_adapter::extract_structural(&project).with_context(|| {
            format!(
                "extracting TypeScript signatures from {}",
                project.display()
            )
        })?);
    }

    if docs.is_empty() {
        anyhow::bail!(
            "{} is neither a cargo project nor a TypeScript project — pass --structural <json> to \
             analyze pre-extracted signatures",
            args.path.display()
        );
    }
    Ok(docs)
}

/// Extract Rust structural signatures with the chosen extractor. `syn` is the
/// toolchain-free syntactic path; `stablemir` is the resolved StableMIR driver and
/// errors honestly when it is unavailable (rather than silently downgrading);
/// `auto` uses `stablemir` when the driver is available and falls back to `syn`
/// otherwise — the CI-friendly degraded mode — printing which path it took to
/// stderr so the choice is never silent.
fn extract_rust_signatures(
    path: &Path,
    extractor: RustExtractor,
) -> Result<hinzu_core::similarity::SignatureDoc> {
    match extractor {
        RustExtractor::Syn => structural_rust::extract(path)
            .with_context(|| format!("extracting Rust signatures (syn) from {}", path.display())),
        RustExtractor::Stablemir => rust_adapter::extract_signatures(path).with_context(|| {
            format!(
                "extracting Rust signatures (stablemir) from {}",
                path.display()
            )
        }),
        RustExtractor::Auto => {
            if rust_adapter::driver_available() {
                match rust_adapter::extract_signatures(path) {
                    Ok(doc) => {
                        eprintln!("rust extractor: stablemir (resolved types)");
                        Ok(doc)
                    }
                    Err(e) => {
                        eprintln!(
                            "note: StableMIR extraction failed ({e:#}); falling back to the \
                             syntactic syn extractor"
                        );
                        structural_rust::extract(path).with_context(|| {
                            format!("extracting Rust signatures (syn) from {}", path.display())
                        })
                    }
                }
            } else {
                eprintln!(
                    "rust extractor: syn (syntactic) — the StableMIR driver is unavailable; use \
                     --rust-extractor stablemir to require it"
                );
                structural_rust::extract(path).with_context(|| {
                    format!("extracting Rust signatures (syn) from {}", path.display())
                })
            }
        }
    }
}

/// Print the human-readable similarity summary to stderr: the header count, the
/// capability edge (which languages had a profile), and a couple of lines per
/// candidate. Mirrors `port-diff`'s `=== … ===` convention.
fn print_similarity_summary(output: &hinzu_core::similarity::SimilarityOutput) {
    eprintln!(
        "=== similarity: {} candidates ===",
        output.stats.candidates_found
    );
    eprintln!(
        "analyzed {} signatures ({} after the trivial-def filter), compared {} pairs",
        output.stats.signatures_analyzed,
        output.stats.signatures_after_filter,
        output.stats.pairs_compared,
    );
    if output.stats.clusters_rejected_low_cohesion > 0 {
        eprintln!(
            "rejected {} loose cluster(s) below the {:.2} cohesion gate",
            output.stats.clusters_rejected_low_cohesion, output.params.min_cohesion,
        );
    }
    let langs_with_profiles: Vec<&str> = output
        .profiles
        .iter()
        .map(|p| p.language.as_str())
        .collect();
    if langs_with_profiles.is_empty() && !output.languages.is_empty() {
        eprintln!(
            "note: no shipped structural profile for {} — findings are unprofiled",
            output.languages.join(", ")
        );
    }
    for c in &output.candidates {
        eprintln!(
            "  {} [{:.2} confidence, {:.2} similarity] {} → {}",
            c.id,
            c.confidence,
            c.pattern.similarity,
            c.likely_abstraction.family,
            c.pattern.summary,
        );
        for m in &c.members {
            eprintln!(
                "      {} ({}:{}-{})",
                m.display, m.file, m.line_start, m.line_end
            );
        }
    }
}
