//! Port-progress **delta**: comparing a current [`PortDiffReport`] against a saved
//! baseline report to answer "did this diff/commit actually move the port
//! forward?".
//!
//! Where a [`PortDiffReport`] is a snapshot of how much of a source package is
//! ported *right now*, a [`PortDiffDelta`] is the signed difference between two
//! snapshots: which files advanced to a higher band, which regressed to a lower
//! one, which were added or removed, how the symbol-match total moved, and an
//! overall [`Verdict`] (`forward` / `mixed` / `backward` / `no_change`).
//!
//! ## Band ordering
//!
//! "Forward" is defined by the band rank
//! `NOT-STARTED < STARTED < PORTED < DONE` ([`band_rank`]). A file whose band
//! rank rose between baseline and current has **advanced**; one whose rank fell
//! has **regressed**; an equal rank is **unchanged** (though its symbol coverage
//! may still have moved). Files present only in the current report are **added**;
//! files present only in the baseline are **removed**.
//!
//! ## Purity
//!
//! [`diff_reports`] (and the multi-package / cross-package variants) are pure
//! functions of their two report inputs — no clock, no randomness, no filesystem.
//! Everything is sorted, so re-running over the same pair yields byte-identical
//! output. The CLI is responsible for loading the baseline JSON off disk and
//! handing the deserialized report in.

use serde::{Deserialize, Serialize};

use super::report::{
    Band, FileEntry, MultiPackageReport, PortDiffReport, RootedCrossPackageReport,
};

/// The forward-ordering rank of a band: `NOT-STARTED` (0) `< STARTED` (1) `<
/// PORTED` (2) `< DONE` (3). A higher rank is "more ported"; a file whose rank
/// rises has advanced, one whose rank falls has regressed.
pub fn band_rank(band: Band) -> u8 {
    match band {
        Band::NotStarted => 0,
        Band::Started => 1,
        Band::Ported => 2,
        Band::Done => 3,
    }
}

/// The direction a single file moved between the baseline and the current report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// The file's band rose to a higher band (more ported).
    Advanced,
    /// The file's band fell to a lower band (less ported).
    Regressed,
    /// The file stayed in the same band (its coverage may still have changed).
    Unchanged,
    /// The file is present in the current report but not the baseline.
    Added,
    /// The file was present in the baseline but is gone from the current report.
    Removed,
}

/// One file's movement between the two reports.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileDelta {
    /// The source file path (the match key between baseline and current).
    pub path: String,
    /// The owning package, for a multi-package / cross-package delta; `None` for a
    /// single-package delta.
    pub package: Option<String>,
    /// The file's band in the baseline report, or `None` when the file is added.
    pub band_before: Option<Band>,
    /// The file's band in the current report, or `None` when the file is removed.
    pub band_after: Option<Band>,
    /// Which way it moved.
    pub direction: Direction,
    /// Symbol coverage (`matched / total`) in the baseline, or `None`.
    pub coverage_before: Option<f64>,
    /// Symbol coverage in the current report, or `None`.
    pub coverage_after: Option<f64>,
    /// `coverage_after - coverage_before`, rounded to 3 dp, when both are present.
    pub coverage_delta: Option<f64>,
}

/// One band-transition tally: how many files moved from `band_before` to
/// `band_after`. Only advanced / regressed files (a genuine band change) are
/// tallied; unchanged, added, and removed files are not.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BandTransition {
    pub band_before: Band,
    pub band_after: Band,
    pub count: usize,
}

/// The net change in the number of files in each band (`after - before`). Signed:
/// a band that gained files is positive, one that lost files is negative.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct BandNetMovement {
    pub done: i64,
    pub ported: i64,
    pub started: i64,
    pub not_started: i64,
}

/// The rolled-up totals across every file delta.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DeltaTotals {
    /// Files that moved to a higher band.
    pub advanced: usize,
    /// Files that moved to a lower band.
    pub regressed: usize,
    /// Files that stayed in the same band.
    pub unchanged: usize,
    /// Files present in the current report but not the baseline.
    pub added: usize,
    /// Files present in the baseline but not the current report.
    pub removed: usize,
    /// The band-transition breakdown for the advanced / regressed files, sorted
    /// by `(band_before rank, band_after rank)`. Drives the human summary
    /// (`3 NOT-STARTED→PORTED, 1 STARTED→DONE`).
    pub transitions: Vec<BandTransition>,
    /// Net per-band file-count movement (`after - before`).
    pub band_net_movement: BandNetMovement,
    /// Total matchable source symbols matched in the baseline report.
    pub symbols_matched_before: usize,
    /// Total matchable source symbols matched in the current report.
    pub symbols_matched_after: usize,
    /// `symbols_matched_after - symbols_matched_before` (signed).
    pub symbols_matched_delta: i64,
}

/// The overall port-movement verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// At least one file advanced and none regressed.
    Forward,
    /// Files advanced *and* files regressed.
    Mixed,
    /// At least one file regressed and none advanced.
    Backward,
    /// No file changed band in either direction.
    NoChange,
}

/// The full port-progress delta: the per-file movements plus the rolled-up
/// totals and the overall verdict. The serialized shape of `hinzu port-diff
/// --compare`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortDiffDelta {
    /// The overall verdict.
    pub verdict: Verdict,
    /// The rolled-up totals.
    pub totals: DeltaTotals,
    /// The per-file deltas, sorted by `(package, path)`.
    pub files: Vec<FileDelta>,
}

/// Round a coverage value to 3 dp, matching the report's own rounding.
fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// Compare a baseline single-package report against a current one and produce the
/// port-progress delta. Files are matched by path; a file only in the current
/// report is `added`, one only in the baseline is `removed`.
pub fn diff_reports(baseline: &PortDiffReport, current: &PortDiffReport) -> PortDiffDelta {
    let files = diff_file_lists(&baseline.files, &current.files, None);
    let totals = totals_from(
        &files,
        baseline.overall.symbols_matched,
        current.overall.symbols_matched,
    );
    finish(files, totals)
}

/// Compare two whole-port [`MultiPackageReport`]s. Files are matched by path
/// **within the same package** (paired by package name); a package present on
/// only one side has all its files treated as added / removed.
pub fn diff_multi_reports(
    baseline: &MultiPackageReport,
    current: &MultiPackageReport,
) -> PortDiffDelta {
    diff_packaged_reports(
        &multi_pkg_files(baseline),
        &multi_pkg_files(current),
        baseline.totals.symbols_matched,
        current.totals.symbols_matched,
    )
}

/// Compare two [`RootedCrossPackageReport`]s (the `--all --from` closure shape).
/// Files are matched by path within the same package's closure slice.
pub fn diff_cross_reports(
    baseline: &RootedCrossPackageReport,
    current: &RootedCrossPackageReport,
) -> PortDiffDelta {
    diff_packaged_reports(
        &cross_pkg_files(baseline),
        &cross_pkg_files(current),
        baseline.totals.symbols_matched,
        current.totals.symbols_matched,
    )
}

/// The `(package name, file list)` pairs of a whole-port report.
fn multi_pkg_files(report: &MultiPackageReport) -> Vec<(&str, &[FileEntry])> {
    report
        .packages
        .iter()
        .map(|p| (p.package.as_str(), p.report.files.as_slice()))
        .collect()
}

/// The `(package name, closure-slice file list)` pairs of a cross-package report.
fn cross_pkg_files(report: &RootedCrossPackageReport) -> Vec<(&str, &[FileEntry])> {
    report
        .packages
        .iter()
        .map(|p| (p.package.as_str(), p.rollup.report.files.as_slice()))
        .collect()
}

/// Shared tail of the multi-package / cross-package diffs: pair the packages,
/// diff each package's file lists, and roll up the totals with the two overall
/// symbol-match counts.
fn diff_packaged_reports(
    base_pkgs: &[(&str, &[FileEntry])],
    cur_pkgs: &[(&str, &[FileEntry])],
    symbols_before: usize,
    symbols_after: usize,
) -> PortDiffDelta {
    let files = diff_packaged(base_pkgs, cur_pkgs);
    let totals = totals_from(&files, symbols_before, symbols_after);
    finish(files, totals)
}

/// Pair packages by name across the two sides and diff each package's file lists,
/// tagging every resulting [`FileDelta`] with its package. A package on only one
/// side is diffed against an empty list (so its files come out added / removed).
fn diff_packaged(
    baseline: &[(&str, &[FileEntry])],
    current: &[(&str, &[FileEntry])],
) -> Vec<FileDelta> {
    let mut names: Vec<&str> = baseline
        .iter()
        .map(|(n, _)| *n)
        .chain(current.iter().map(|(n, _)| *n))
        .collect();
    names.sort_unstable();
    names.dedup();

    let empty: &[FileEntry] = &[];
    let mut out = Vec::new();
    for name in names {
        let b = baseline
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, f)| *f)
            .unwrap_or(empty);
        let c = current
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, f)| *f)
            .unwrap_or(empty);
        out.extend(diff_file_lists(b, c, Some(name)));
    }
    out
}

/// The per-file diff over two file lists, matched by path. `package` tags every
/// emitted delta. The output is sorted by `(package, path)`.
fn diff_file_lists(
    baseline: &[FileEntry],
    current: &[FileEntry],
    package: Option<&str>,
) -> Vec<FileDelta> {
    let mut out = Vec::new();

    // Matched + removed: walk the baseline, look each path up in the current.
    for b in baseline {
        match current.iter().find(|c| c.path == b.path) {
            Some(c) => {
                let rank_before = band_rank(b.band);
                let rank_after = band_rank(c.band);
                let direction = match rank_after.cmp(&rank_before) {
                    std::cmp::Ordering::Greater => Direction::Advanced,
                    std::cmp::Ordering::Less => Direction::Regressed,
                    std::cmp::Ordering::Equal => Direction::Unchanged,
                };
                let coverage_delta = match (b.coverage, c.coverage) {
                    (Some(bc), Some(cc)) => Some(round3(cc - bc)),
                    _ => None,
                };
                out.push(FileDelta {
                    path: b.path.clone(),
                    package: package.map(str::to_string),
                    band_before: Some(b.band),
                    band_after: Some(c.band),
                    direction,
                    coverage_before: b.coverage,
                    coverage_after: c.coverage,
                    coverage_delta,
                });
            }
            None => out.push(FileDelta {
                path: b.path.clone(),
                package: package.map(str::to_string),
                band_before: Some(b.band),
                band_after: None,
                direction: Direction::Removed,
                coverage_before: b.coverage,
                coverage_after: None,
                coverage_delta: None,
            }),
        }
    }

    // Added: paths in the current not present in the baseline.
    for c in current {
        if !baseline.iter().any(|b| b.path == c.path) {
            out.push(FileDelta {
                path: c.path.clone(),
                package: package.map(str::to_string),
                band_before: None,
                band_after: Some(c.band),
                direction: Direction::Added,
                coverage_before: None,
                coverage_after: c.coverage,
                coverage_delta: None,
            });
        }
    }

    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Roll the per-file deltas + the two symbol-match totals into [`DeltaTotals`].
fn totals_from(files: &[FileDelta], symbols_before: usize, symbols_after: usize) -> DeltaTotals {
    use std::collections::BTreeMap;

    let mut totals = DeltaTotals {
        symbols_matched_before: symbols_before,
        symbols_matched_after: symbols_after,
        symbols_matched_delta: symbols_after as i64 - symbols_before as i64,
        ..Default::default()
    };

    // Per-band net movement: +1 for the after band, -1 for the before band of
    // every file (a matched file bumps both, an added file only the after, a
    // removed file only the before).
    let mut net = BandNetMovement::default();
    let bump = |net: &mut BandNetMovement, band: Band, by: i64| match band {
        Band::Done => net.done += by,
        Band::Ported => net.ported += by,
        Band::Started => net.started += by,
        Band::NotStarted => net.not_started += by,
    };

    // Transition tally keyed by (before rank, after rank) for deterministic order.
    let mut transitions: BTreeMap<(u8, u8), (Band, Band, usize)> = BTreeMap::new();

    for f in files {
        match f.direction {
            Direction::Advanced => totals.advanced += 1,
            Direction::Regressed => totals.regressed += 1,
            Direction::Unchanged => totals.unchanged += 1,
            Direction::Added => totals.added += 1,
            Direction::Removed => totals.removed += 1,
        }
        if let Some(b) = f.band_after {
            bump(&mut net, b, 1);
        }
        if let Some(b) = f.band_before {
            bump(&mut net, b, -1);
        }
        if matches!(f.direction, Direction::Advanced | Direction::Regressed) {
            if let (Some(before), Some(after)) = (f.band_before, f.band_after) {
                let entry = transitions
                    .entry((band_rank(before), band_rank(after)))
                    .or_insert((before, after, 0));
                entry.2 += 1;
            }
        }
    }

    totals.band_net_movement = net;
    totals.transitions = transitions
        .into_values()
        .map(|(band_before, band_after, count)| BandTransition {
            band_before,
            band_after,
            count,
        })
        .collect();
    totals
}

/// Derive the verdict from the advanced / regressed counts and assemble the
/// delta.
fn finish(files: Vec<FileDelta>, totals: DeltaTotals) -> PortDiffDelta {
    let verdict = match (totals.advanced > 0, totals.regressed > 0) {
        (true, false) => Verdict::Forward,
        (true, true) => Verdict::Mixed,
        (false, true) => Verdict::Backward,
        (false, false) => Verdict::NoChange,
    };
    PortDiffDelta {
        verdict,
        totals,
        files,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portdiff::report::{
        BandCounts, ConformanceCrosscheck, Fidelity, FileMapSummary, FileTierBreakdown,
        GraphConfirmSummary, NaiveVsGraph, Overall, TierCounts,
    };

    /// A minimal [`FileEntry`] at a given band + coverage.
    fn file(path: &str, band: Band, matched: usize, total: usize) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            module: path.to_string(),
            band,
            coverage: if total > 0 {
                Some(round3(matched as f64 / total as f64))
            } else {
                None
            },
            graph_confirmed_coverage: None,
            mapped_target: None,
            map_method: None,
            map_votes: None,
            total_symbols: total,
            matched_symbols: matched,
            tier_breakdown: FileTierBreakdown::default(),
            fan_in: 0,
        }
    }

    /// A [`PortDiffReport`] over the given files + a symbols-matched total.
    fn report(files: Vec<FileEntry>, symbols_matched: usize) -> PortDiffReport {
        PortDiffReport {
            source_kind: "ts".to_string(),
            target_kind: "rust".to_string(),
            overall: Overall {
                source_files_total: files.len(),
                symbols_total: symbols_matched + 10,
                symbols_synthetic_excluded: 0,
                symbols_matched,
                symbols_matched_pct: 0.0,
                tier_counts: TierCounts::default(),
                graph: GraphConfirmSummary::default(),
                bands: BandCounts::default(),
            },
            file_map_summary: FileMapSummary::default(),
            files,
            waves: Vec::new(),
            ready_frontier: Vec::new(),
            ready_frontier_total: 0,
            naive_vs_graph: NaiveVsGraph {
                naive_files_matched: 0,
                graph_files_matched: 0,
                recovered_files: Vec::new(),
                recovered_count: 0,
            },
            conformance_crosscheck: ConformanceCrosscheck {
                native_modules: 0,
                native_files: Vec::new(),
                done_band: 0,
                ported_plus_done: 0,
                note: String::new(),
            },
            fidelity: Fidelity {
                structural_not_correctness: true,
                matchable_denominator: String::new(),
                cluster_caveat: String::new(),
                notes: Vec::new(),
            },
        }
    }

    #[test]
    fn advance_is_forward() {
        // a.ts: NOT-STARTED → PORTED (an advance), symbols 5 → 12.
        let base = report(vec![file("a.ts", Band::NotStarted, 0, 8)], 5);
        let cur = report(vec![file("a.ts", Band::Ported, 6, 8)], 12);
        let d = diff_reports(&base, &cur);

        assert_eq!(d.verdict, Verdict::Forward);
        assert_eq!(d.totals.advanced, 1);
        assert_eq!(d.totals.regressed, 0);
        assert_eq!(d.files.len(), 1);
        assert_eq!(d.files[0].direction, Direction::Advanced);
        assert_eq!(d.files[0].band_before, Some(Band::NotStarted));
        assert_eq!(d.files[0].band_after, Some(Band::Ported));
        assert_eq!(d.totals.symbols_matched_delta, 7);
        // One transition: NOT-STARTED → PORTED, count 1.
        assert_eq!(d.totals.transitions.len(), 1);
        assert_eq!(d.totals.transitions[0].band_before, Band::NotStarted);
        assert_eq!(d.totals.transitions[0].band_after, Band::Ported);
        assert_eq!(d.totals.transitions[0].count, 1);
        // Net movement: +1 into PORTED, -1 out of NOT-STARTED.
        assert_eq!(d.totals.band_net_movement.ported, 1);
        assert_eq!(d.totals.band_net_movement.not_started, -1);
    }

    #[test]
    fn regression_is_backward() {
        // a.ts: PORTED → STARTED (a regression).
        let base = report(vec![file("a.ts", Band::Ported, 6, 8)], 12);
        let cur = report(vec![file("a.ts", Band::Started, 2, 8)], 8);
        let d = diff_reports(&base, &cur);

        assert_eq!(d.verdict, Verdict::Backward);
        assert_eq!(d.totals.advanced, 0);
        assert_eq!(d.totals.regressed, 1);
        assert_eq!(d.files[0].direction, Direction::Regressed);
        assert_eq!(d.totals.symbols_matched_delta, -4);
    }

    #[test]
    fn advance_and_regression_is_mixed() {
        let base = report(
            vec![
                file("a.ts", Band::NotStarted, 0, 8),
                file("b.ts", Band::Ported, 6, 8),
            ],
            10,
        );
        let cur = report(
            vec![
                file("a.ts", Band::Ported, 6, 8),
                file("b.ts", Band::Started, 2, 8),
            ],
            10,
        );
        let d = diff_reports(&base, &cur);

        assert_eq!(d.verdict, Verdict::Mixed);
        assert_eq!(d.totals.advanced, 1);
        assert_eq!(d.totals.regressed, 1);
    }

    #[test]
    fn added_and_removed_files() {
        // a.ts unchanged, b.ts removed, c.ts added — no band change ⇒ no_change.
        let base = report(
            vec![
                file("a.ts", Band::Ported, 6, 8),
                file("b.ts", Band::Started, 2, 8),
            ],
            8,
        );
        let cur = report(
            vec![
                file("a.ts", Band::Ported, 6, 8),
                file("c.ts", Band::Started, 3, 8),
            ],
            9,
        );
        let d = diff_reports(&base, &cur);

        assert_eq!(d.verdict, Verdict::NoChange);
        assert_eq!(d.totals.added, 1);
        assert_eq!(d.totals.removed, 1);
        assert_eq!(d.totals.unchanged, 1);
        let added: Vec<&FileDelta> = d
            .files
            .iter()
            .filter(|f| f.direction == Direction::Added)
            .collect();
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].path, "c.ts");
        assert_eq!(added[0].band_before, None);
        let removed: Vec<&FileDelta> = d
            .files
            .iter()
            .filter(|f| f.direction == Direction::Removed)
            .collect();
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].path, "b.ts");
        assert_eq!(removed[0].band_after, None);
    }

    #[test]
    fn unchanged_band_still_reports_coverage_delta() {
        // Same band (STARTED), but coverage moved 0.25 → 0.5.
        let base = report(vec![file("a.ts", Band::Started, 2, 8)], 2);
        let cur = report(vec![file("a.ts", Band::Started, 4, 8)], 4);
        let d = diff_reports(&base, &cur);

        assert_eq!(d.verdict, Verdict::NoChange);
        assert_eq!(d.files[0].direction, Direction::Unchanged);
        assert_eq!(d.files[0].coverage_delta, Some(0.25));
    }
}
