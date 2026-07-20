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
    /// A would-be PORTED/STARTED file whose matched symbols land predominantly
    /// (> 50%) in a **secondary** target crate — the port moved out of the
    /// package's primary crate. A variant of matched, reported between PORTED and
    /// STARTED. Single-crate packages never produce this band.
    #[serde(rename = "RELOCATED")]
    Relocated,
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
    pub relocated: usize,
    pub started: usize,
    pub not_started: usize,
}

impl BandCounts {
    pub(crate) fn bump(&mut self, band: Band) {
        match band {
            Band::Done => self.done += 1,
            Band::Ported => self.ported += 1,
            Band::Relocated => self.relocated += 1,
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
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
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

/// One package's headline numbers, plus the full per-package report embedded so
/// a combined `--out` is self-contained. The headline fields are lifted out of
/// [`PortDiffReport`] for a compact rollup table without re-walking the report.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackageRollup {
    /// The `--package` name.
    pub package: String,
    /// Distinct source files considered (the band denominator).
    pub source_files_total: usize,
    /// Per-band file counts for this package.
    pub bands: BandCounts,
    /// Matchable source symbols (the match denominator).
    pub symbols_total: usize,
    /// Matchable source symbols that matched a target symbol.
    pub symbols_matched: usize,
    /// `symbols_matched / symbols_total`, rounded to 3 dp (`0.0` when empty).
    pub symbols_matched_pct: f64,
    /// Conformance native (test-verified) modules for this package.
    pub conformance_native: usize,
    /// Files banded DONE — equals `conformance_native` when the oracle holds.
    pub done_band: usize,
    /// How many waves the source port plan has.
    pub wave_count: usize,
    /// The full per-package report, embedded so the combined JSON is complete.
    pub report: PortDiffReport,
}

impl PackageRollup {
    /// Lift a package's report to a rollup: the headline numbers pulled out of the
    /// report, plus the full report embedded. Shared by the whole-port and
    /// cross-package rollups so both extract the headline the same way.
    pub(crate) fn from_report(package: String, report: PortDiffReport) -> PackageRollup {
        let o = &report.overall;
        let cc = &report.conformance_crosscheck;
        PackageRollup {
            package,
            source_files_total: o.source_files_total,
            bands: o.bands.clone(),
            symbols_total: o.symbols_total,
            symbols_matched: o.symbols_matched,
            symbols_matched_pct: o.symbols_matched_pct,
            conformance_native: cc.native_modules,
            done_band: cc.done_band,
            wave_count: report.waves.len(),
            report,
        }
    }
}

/// The summed totals across every package in a [`MultiPackageReport`].
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RollupTotals {
    /// Summed source files across all packages.
    pub source_files_total: usize,
    /// Summed per-band file counts across all packages.
    pub bands: BandCounts,
    /// Summed matchable source symbols.
    pub symbols_total: usize,
    /// Summed matched source symbols.
    pub symbols_matched: usize,
    /// Overall `symbols_matched / symbols_total`, rounded to 3 dp.
    pub symbols_matched_pct: f64,
    /// Summed conformance native modules.
    pub conformance_native: usize,
    /// Summed DONE-banded files.
    pub done_band: usize,
}

impl RollupTotals {
    /// Fold one package report's headline numbers into the running totals. Shared
    /// by the whole-port and cross-package rollups so both sum identically.
    pub(crate) fn add_report(&mut self, report: &PortDiffReport) {
        let o = &report.overall;
        let cc = &report.conformance_crosscheck;
        self.source_files_total += o.source_files_total;
        self.bands.done += o.bands.done;
        self.bands.ported += o.bands.ported;
        self.bands.relocated += o.bands.relocated;
        self.bands.started += o.bands.started;
        self.bands.not_started += o.bands.not_started;
        self.symbols_total += o.symbols_total;
        self.symbols_matched += o.symbols_matched;
        self.conformance_native += cc.native_modules;
        self.done_band += cc.done_band;
    }

    /// Recompute the overall match % from the accumulated matched / total (not an
    /// average of per-package percentages). Call once after the last `add_report`.
    pub(crate) fn recompute_pct(&mut self) {
        self.symbols_matched_pct = if self.symbols_total > 0 {
            ((self.symbols_matched as f64 / self.symbols_total as f64) * 1000.0).round() / 1000.0
        } else {
            0.0
        };
    }
}

/// The combined whole-port rollup: every package's [`PortDiffReport`] plus the
/// summed totals. The serialized shape of `hinzu port-diff --all`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MultiPackageReport {
    /// The source language / ecosystem tag echoed from config (`"ts"`).
    pub source_kind: String,
    /// The target language / ecosystem tag (`"rust"`).
    pub target_kind: String,
    /// Per-package rollups, in the order the packages were run.
    pub packages: Vec<PackageRollup>,
    /// The summed totals across every package.
    pub totals: RollupTotals,
}

impl MultiPackageReport {
    /// Aggregate per-package reports into the combined rollup. Each `(name,
    /// report)` pair is lifted to a [`PackageRollup`] (headline numbers +
    /// embedded report), and the [`RollupTotals`] are the element-wise sums, with
    /// the overall match % recomputed from the summed matched / total (not an
    /// average of per-package percentages). Deterministic: package order is the
    /// caller's order, nothing is sorted or randomized.
    pub fn aggregate(
        source_kind: &str,
        target_kind: &str,
        reports: Vec<(String, PortDiffReport)>,
    ) -> MultiPackageReport {
        let mut packages = Vec::with_capacity(reports.len());
        let mut totals = RollupTotals::default();
        for (name, report) in reports {
            totals.add_report(&report);
            packages.push(PackageRollup::from_report(name, report));
        }
        totals.recompute_pct();
        MultiPackageReport {
            source_kind: source_kind.to_string(),
            target_kind: target_kind.to_string(),
            packages,
            totals,
        }
    }
}

/// One package's slice of a cross-package `--from` closure: how much of the
/// closure lives in this package, plus the full port-diff over *just that slice*.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackageClosureRollup {
    /// The `--package` name this slice belongs to.
    pub package: String,
    /// Files of this package that fall in the union closure.
    pub closure_files: usize,
    /// Local symbols of this package that fall in the union closure.
    pub closure_symbols: usize,
    /// The port-diff of this package's closure slice against its target crate —
    /// headline numbers + the embedded per-package [`PortDiffReport`].
    pub rollup: PackageRollup,
}

/// The cross-package rooted report: a `--from` closure taken over the **union**
/// source graph (so it crosses package boundaries), routed per file to its owning
/// package, and matched — per package — against that package's target crate. The
/// serialized shape of `hinzu port-diff --all --from <entry>`.
///
/// It answers "what does *this entry point* need, across every package, and how
/// much of it is ported" — e.g. the whole CLI's bootstrap closure, spanning
/// several packages, with each package's slice banded DONE / PORTED / STARTED /
/// NOT-STARTED against its own port.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RootedCrossPackageReport {
    /// The source / target language tags echoed from config.
    pub source_kind: String,
    pub target_kind: String,
    /// The resolved entry roots the closure was taken from.
    pub roots: Vec<String>,
    /// Total local symbols in the union closure (across every package).
    pub closure_symbols: usize,
    /// Total files in the union closure (across every package).
    pub closure_files: usize,
    /// How many packages the closure touches (have ≥1 closure file).
    pub packages_spanned: usize,
    /// Per-package closure slices, in config (sorted-name) order.
    pub packages: Vec<PackageClosureRollup>,
    /// Summed totals across the per-package slices (files, bands, symbols).
    pub totals: RollupTotals,
}

impl RootedCrossPackageReport {
    /// Assemble the cross-package rooted report from the resolved roots, the union
    /// closure totals, and each package's `(name, closure_files, closure_symbols,
    /// report)`. Per-package slices are lifted to [`PackageRollup`]s (reusing the
    /// same headline extraction as the whole-port rollup) and the [`RollupTotals`]
    /// are the element-wise sums, with the overall match % recomputed from the
    /// summed matched / total. Deterministic: package order is the caller's.
    pub fn aggregate(
        source_kind: &str,
        target_kind: &str,
        roots: Vec<String>,
        closure_symbols: usize,
        closure_files: usize,
        slices: Vec<(String, usize, usize, PortDiffReport)>,
    ) -> RootedCrossPackageReport {
        let mut packages = Vec::with_capacity(slices.len());
        let mut totals = RollupTotals::default();
        for (name, cfiles, csyms, report) in slices {
            totals.add_report(&report);
            packages.push(PackageClosureRollup {
                closure_files: cfiles,
                closure_symbols: csyms,
                rollup: PackageRollup::from_report(name.clone(), report),
                package: name,
            });
        }
        totals.recompute_pct();
        RootedCrossPackageReport {
            source_kind: source_kind.to_string(),
            target_kind: target_kind.to_string(),
            roots,
            closure_symbols,
            closure_files,
            packages_spanned: packages.len(),
            packages,
            totals,
        }
    }

    /// Project to a [`MultiPackageReport`] so the combined whole-port HTML renderer
    /// can be reused for the cross-package closure dashboard. The per-package
    /// slices become the rollup's packages and the summed totals carry over.
    pub fn as_multi(&self) -> MultiPackageReport {
        MultiPackageReport {
            source_kind: self.source_kind.clone(),
            target_kind: self.target_kind.clone(),
            packages: self.packages.iter().map(|p| p.rollup.clone()).collect(),
            totals: self.totals.clone(),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal [`PortDiffReport`] carrying just the headline numbers the rollup
    /// reads — every other field is an empty default. `wave_count` empty waves are
    /// pushed so `report.waves.len()` is exact.
    #[allow(clippy::too_many_arguments)]
    fn tiny(
        files: usize,
        bands: (usize, usize, usize, usize, usize),
        sym_total: usize,
        sym_matched: usize,
        native: usize,
        done_band: usize,
        wave_count: usize,
    ) -> PortDiffReport {
        let bands = BandCounts {
            done: bands.0,
            ported: bands.1,
            relocated: bands.2,
            started: bands.3,
            not_started: bands.4,
        };
        PortDiffReport {
            source_kind: "ts".to_string(),
            target_kind: "rust".to_string(),
            overall: Overall {
                source_files_total: files,
                symbols_total: sym_total,
                symbols_synthetic_excluded: 0,
                symbols_matched: sym_matched,
                symbols_matched_pct: if sym_total > 0 {
                    (sym_matched as f64 / sym_total as f64 * 1000.0).round() / 1000.0
                } else {
                    0.0
                },
                tier_counts: TierCounts::default(),
                graph: GraphConfirmSummary::default(),
                bands: bands.clone(),
            },
            file_map_summary: FileMapSummary::default(),
            files: Vec::new(),
            waves: vec![
                WaveBand {
                    wave: 0,
                    files: 0,
                    bands: BandCounts::default(),
                    symbols_total: 0,
                    symbols_matched: 0,
                    symbols_pct: 0.0,
                };
                wave_count
            ],
            ready_frontier: Vec::new(),
            ready_frontier_total: 0,
            naive_vs_graph: NaiveVsGraph::default(),
            conformance_crosscheck: ConformanceCrosscheck {
                native_modules: native,
                done_band,
                ported_plus_done: bands.done + bands.ported,
                ..Default::default()
            },
            fidelity: Fidelity {
                structural_not_correctness: true,
                ..Default::default()
            },
        }
    }

    #[test]
    fn aggregate_sums_totals_and_recomputes_overall_pct() {
        // Two tiny packages: 10 files (D1 P2 R1 S3 N4), 20 symbols 10 matched, and
        // 6 files (D0 P1 R1 S2 N3), 30 symbols 20 matched.
        let a = tiny(10, (1, 2, 1, 3, 4), 20, 10, 1, 1, 2);
        let b = tiny(6, (0, 1, 1, 2, 3), 30, 20, 0, 0, 1);
        let multi = MultiPackageReport::aggregate(
            "ts",
            "rust",
            vec![("ai".to_string(), a), ("agent".to_string(), b)],
        );

        // Per-package rollups preserve order + headline numbers + wave counts.
        assert_eq!(multi.packages.len(), 2);
        assert_eq!(multi.packages[0].package, "ai");
        assert_eq!(multi.packages[0].wave_count, 2);
        assert_eq!(multi.packages[1].package, "agent");
        assert_eq!(multi.packages[1].source_files_total, 6);

        // Totals are the element-wise sums.
        let t = &multi.totals;
        assert_eq!(t.source_files_total, 16);
        assert_eq!(t.bands.done, 1);
        assert_eq!(t.bands.ported, 3);
        assert_eq!(t.bands.relocated, 2);
        assert_eq!(t.bands.started, 5);
        assert_eq!(t.bands.not_started, 7);
        assert_eq!(t.symbols_total, 50);
        assert_eq!(t.symbols_matched, 30);
        assert_eq!(t.conformance_native, 1);
        assert_eq!(t.done_band, 1);
        // Overall % is recomputed from the sums (30/50 = 0.6), not an average of
        // the per-package 0.5 and 0.667.
        assert_eq!(t.symbols_matched_pct, 0.6);
    }
}
