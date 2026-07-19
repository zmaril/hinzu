//! The [`super::PortDiffReport`] data structures — the serializable output of
//! [`super::port_diff`].

use serde::{Deserialize, Serialize};

/// The band a file lands in. Ordered from most to least ported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Band {
    /// Test-verified via the conformance native set.
    #[serde(rename = "DONE")]
    Done,
    /// Coverage ≥ `ported_threshold`, but not in the native set.
    #[serde(rename = "PORTED")]
    Ported,
    /// At least one symbol matched (or a target subtree was mapped), below
    /// threshold.
    #[serde(rename = "STARTED")]
    Started,
    /// No symbol matched.
    #[serde(rename = "NOT-STARTED")]
    NotStarted,
}

/// Per-band file counts.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BandCounts {
    pub done: usize,
    pub ported: usize,
    pub started: usize,
    pub not_started: usize,
}

impl BandCounts {
    pub(crate) fn bump(&mut self, band: Band) {
        match band {
            Band::Done => self.done += 1,
            Band::Ported => self.ported += 1,
            Band::Started => self.started += 1,
            Band::NotStarted => self.not_started += 1,
        }
    }
}

/// How many source symbols landed in each match tier.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TierCounts {
    pub exact_module: usize,
    pub subtree: usize,
    pub global_name: usize,
    pub unmatched: usize,
}

/// The graph-confirm summary over all evaluable matched symbols.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GraphConfirmSummary {
    /// Matched symbols with ≥1 matched internal callee (so overlap is defined).
    pub evaluable: usize,
    /// Of the evaluable, how many had edge overlap ≥ 0.5.
    pub confirmed: usize,
    /// `confirmed / evaluable`, rounded to 3 dp (`0.0` when none evaluable).
    pub confirmed_pct_of_evaluable: f64,
    /// Mean edge overlap across the evaluable set, rounded to 3 dp.
    pub mean_edge_overlap: f64,
}

/// The headline aggregate.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Overall {
    /// Distinct source files considered (the band denominator).
    pub source_files_total: usize,
    /// Matchable source symbols (named, non-synthetic) — the match denominator.
    pub symbols_total: usize,
    /// Synthetic / anonymous source symbols excluded from the denominator.
    pub symbols_synthetic_excluded: usize,
    /// Matchable source symbols that matched a target symbol.
    pub symbols_matched: usize,
    /// `symbols_matched / symbols_total`, rounded to 3 dp.
    pub symbols_matched_pct: f64,
    /// Per-tier match counts.
    pub tier_counts: TierCounts,
    /// The graph-confirm summary.
    pub graph: GraphConfirmSummary,
    /// Per-band file counts.
    pub bands: BandCounts,
}

/// The per-tier breakdown of the symbols in one file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FileTierBreakdown {
    pub exact_module: usize,
    pub subtree: usize,
    pub global_name: usize,
    pub unmatched: usize,
}

/// One source file's port status.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileEntry {
    /// The source file path.
    pub path: String,
    /// Its normalized module path.
    pub module: String,
    /// The band it lands in.
    pub band: Band,
    /// `matched / total` symbols, or `None` when the file has no matchable symbol.
    pub coverage: Option<f64>,
    /// `graph_confirmed / evaluable` for this file, or `None` when nothing is
    /// evaluable.
    pub graph_confirmed_coverage: Option<f64>,
    /// The mapped target subtree/module (may be a cluster root covering several
    /// target modules), or `None` when unmapped.
    pub mapped_target: Option<String>,
    /// How the mapping was found: `"exact"`, `"exact-subtree"`, or
    /// `"graph-cluster"`.
    pub map_method: Option<String>,
    /// The clustering vote mass, when the mapping came from `"graph-cluster"`.
    pub map_votes: Option<f64>,
    /// Matchable symbols in the file.
    pub total_symbols: usize,
    /// Of those, how many matched.
    pub matched_symbols: usize,
    /// The per-tier breakdown of the file's symbols.
    pub tier_breakdown: FileTierBreakdown,
    /// The file's in-degree (distinct dependents), from the source file rollup.
    pub fan_in: usize,
}

/// A wave's band breakdown, for the plan-level view.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WaveBand {
    /// The wave number.
    pub wave: u32,
    /// How many (source) files land in this wave.
    pub files: usize,
    /// Per-band counts within the wave.
    pub bands: BandCounts,
    /// Total matchable symbols across the wave's files.
    pub symbols_total: usize,
    /// Matched symbols across the wave's files.
    pub symbols_matched: usize,
    /// `symbols_matched / symbols_total`, rounded to 3 dp (`0.0` when empty).
    pub symbols_pct: f64,
}

/// A file on the ready frontier — unported, with every source dependency ported.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FrontierEntry {
    pub path: String,
    pub band: Band,
    pub fan_in: usize,
    pub total_symbols: usize,
    pub matched_symbols: usize,
    pub coverage: Option<f64>,
    /// How many in-source dependencies it has (all already ported/done).
    pub dep_count: usize,
    pub mapped_target: Option<String>,
}

/// The naive-vs-graph delta: what the decomposition-aware clustering recovers
/// over a naive exact-path baseline.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NaiveVsGraph {
    /// Files a naive pass would map by exact normalized path alone.
    pub naive_files_matched: usize,
    /// Files the graph pass matches (any band but NOT-STARTED).
    pub graph_files_matched: usize,
    /// Files whose target mapping was recovered by clustering (`map_method =
    /// "graph-cluster"` / `"exact-subtree"`) — the decomposed / relocated files a
    /// naive exact-path match misses.
    pub recovered_files: Vec<String>,
    /// `recovered_files.len()`.
    pub recovered_count: usize,
}

/// The file-map method tally.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FileMapSummary {
    pub exact: usize,
    pub exact_subtree: usize,
    pub graph_cluster: usize,
    pub unmapped: usize,
}

/// The conformance cross-check: the structural DONE band vs the test-verified
/// native module count.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConformanceCrosscheck {
    /// Native modules found in the manifest (for the configured package).
    pub native_modules: usize,
    /// The source files those native modules map to.
    pub native_files: Vec<String>,
    /// Files banded DONE.
    pub done_band: usize,
    /// DONE + PORTED — a structural upper bound on "might pass".
    pub ported_plus_done: usize,
    /// A note reconciling structural bands with test verification.
    pub note: String,
}

/// The honest fidelity block.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Fidelity {
    /// Always true: the matching is structural, not a correctness proof.
    pub structural_not_correctness: bool,
    /// How the matchable-symbol denominator is defined.
    pub matchable_denominator: String,
    /// The caveat on clustered (subtree) file mappings.
    pub cluster_caveat: String,
    /// Free-form notes, including any best-effort conformance load failure.
    pub notes: Vec<String>,
}

/// The complete port-diff report, ready to serialize as JSON.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortDiffReport {
    /// The source/target language tags echoed from config.
    pub source_kind: String,
    pub target_kind: String,
    /// The headline aggregate.
    pub overall: Overall,
    /// The file-map method tally.
    pub file_map_summary: FileMapSummary,
    /// Per-file port status, sorted by path.
    pub files: Vec<FileEntry>,
    /// Per-wave band breakdown, in wave order.
    pub waves: Vec<WaveBand>,
    /// The ready frontier, highest fan-in first.
    pub ready_frontier: Vec<FrontierEntry>,
    /// The total frontier size (`ready_frontier` may be truncated).
    pub ready_frontier_total: usize,
    /// The naive-vs-graph recovery delta.
    pub naive_vs_graph: NaiveVsGraph,
    /// The conformance cross-check.
    pub conformance_crosscheck: ConformanceCrosscheck,
    /// The honest fidelity block.
    pub fidelity: Fidelity,
}
