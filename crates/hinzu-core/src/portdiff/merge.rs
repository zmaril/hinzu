//! The **split-not-merge** invariant detector.
//!
//! A faithful port keeps each source file's identity: its symbols may be
//! decomposed or relocated, but they should not be *merged* with an unrelated
//! source file into one target destination. When two or more distinct source
//! files land predominantly in the *same* target file, the port has quietly
//! collapsed a boundary — a **file-merge**. When those contributing source files
//! come from two or more distinct *packages*, the collapse crosses a package
//! boundary — a **package-merge**, the high-severity case (a single Rust file now
//! carries logic that lived in separate upstream packages).
//!
//! The detector reuses the port-diff matcher's per-symbol `at_id` matches: each
//! source file already knows its **dominant target file** (the target file
//! holding the plurality of that file's matched symbols — see
//! [`super::report::FileEntry::dominant_target_file`]). This module inverts that
//! `source_file -> target_file` relation into `target_file -> [source_file]` and
//! flags any target file with ≥ 2 contributing source files. It never parses a
//! provenance comment; the signal is entirely graph-derived, so it survives
//! renames and refactors the comments would miss. Everything is sorted, so the
//! report is deterministic.

use serde::{Deserialize, Serialize};

use super::report::FileEntry;

/// One source file that contributes its matched symbols to a merged target file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeContributor {
    /// The contributing source file path (`src/api/anthropic/content.ts`).
    pub source_file: String,
    /// The package the source file belongs to (`ai`, `coding-agent`, …). Empty in
    /// a single-package report, where every contributor shares one package and the
    /// package split is meaningless.
    pub package: String,
    /// How many of this source file's matched symbols landed in the target file —
    /// the split proportion a reviewer reads to see who contributed how much.
    pub matched_symbols: usize,
}

/// One flagged target file: the port destination that ≥ 2 distinct source files
/// mapped into.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeEntry {
    /// The target file that received the merge (`crates/pidgin-ai/src/...rs`).
    pub target_file: String,
    /// The contributing source files, sorted by descending matched-symbol count
    /// then path — the split proportions.
    pub contributors: Vec<MergeContributor>,
    /// The distinct packages the contributors span, sorted.
    pub packages: Vec<String>,
    /// Whether the contributors span ≥ 2 distinct packages (a package-merge).
    pub cross_package: bool,
    /// Total matched symbols across all contributors landing in this target file.
    pub total_matched_symbols: usize,
}

/// The split-not-merge report: target files that ≥ 2 source files merged into.
/// `package_merges` is the cross-package subset of `file_merges` (the
/// high-severity violations), duplicated out so a reader sees them first.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergeReport {
    /// Every target file with ≥ 2 distinct contributing source files, most
    /// contested (most contributors, then most symbols) first.
    pub file_merges: Vec<MergeEntry>,
    /// The subset of `file_merges` whose contributors span ≥ 2 packages.
    pub package_merges: Vec<MergeEntry>,
}

impl MergeReport {
    /// Invert a flat list of `(target_file, contributor)` pairs into the merge
    /// report. One pair per source file (its dominant target file); grouping by
    /// target file, any target with ≥ 2 distinct contributing source files is a
    /// file-merge, and the cross-package ones are also package-merges.
    ///
    /// Deterministic: contributors are sorted by descending symbol count then
    /// path, packages are sorted-unique, and entries are ordered by descending
    /// contributor count, then descending total symbols, then target path.
    pub fn from_contributions(contributions: Vec<(String, MergeContributor)>) -> MergeReport {
        use std::collections::BTreeMap;

        // target_file -> its contributors (one per distinct source file).
        let mut by_target: BTreeMap<String, Vec<MergeContributor>> = BTreeMap::new();
        for (target_file, c) in contributions {
            by_target.entry(target_file).or_default().push(c);
        }

        let mut file_merges: Vec<MergeEntry> = Vec::new();
        for (target_file, mut contributors) in by_target {
            // Distinct source files only — a source file maps to one dominant
            // target, so duplicates should not occur, but collapse defensively.
            contributors.sort_by(|a, b| a.source_file.cmp(&b.source_file));
            contributors.dedup_by(|a, b| a.source_file == b.source_file);
            if contributors.len() < 2 {
                continue;
            }
            // Present the split proportions: biggest contributor first.
            contributors.sort_by(|a, b| {
                b.matched_symbols
                    .cmp(&a.matched_symbols)
                    .then_with(|| a.source_file.cmp(&b.source_file))
            });
            let mut packages: Vec<String> =
                contributors.iter().map(|c| c.package.clone()).collect();
            packages.sort();
            packages.dedup();
            // An empty package label (single-package report) is not a distinct
            // package for the cross-package test.
            let distinct_pkgs = packages.iter().filter(|p| !p.is_empty()).count();
            let cross_package = distinct_pkgs >= 2;
            let total_matched_symbols = contributors.iter().map(|c| c.matched_symbols).sum();
            file_merges.push(MergeEntry {
                target_file,
                contributors,
                packages,
                cross_package,
                total_matched_symbols,
            });
        }

        // Most contested first: contributor count, then total symbols, then path.
        file_merges.sort_by(|a, b| {
            b.contributors
                .len()
                .cmp(&a.contributors.len())
                .then_with(|| b.total_matched_symbols.cmp(&a.total_matched_symbols))
                .then_with(|| a.target_file.cmp(&b.target_file))
        });

        let package_merges: Vec<MergeEntry> = file_merges
            .iter()
            .filter(|e| e.cross_package)
            .cloned()
            .collect();

        MergeReport {
            file_merges,
            package_merges,
        }
    }
}

/// Lift a package's port-diff [`FileEntry`] rows into merge contributions: each
/// file with a resolved dominant target file becomes one `(target_file,
/// contributor)` pair tagged with `package`. Files that matched nothing (no
/// dominant target) contribute nothing. Shared by the single-package report and
/// the cross-package rollup so both invert the same relation.
pub fn contributions_from_files(
    files: &[FileEntry],
    package: &str,
) -> Vec<(String, MergeContributor)> {
    let mut out = Vec::new();
    for f in files {
        let Some(target_file) = f.dominant_target_file.as_deref() else {
            continue;
        };
        out.push((
            target_file.to_string(),
            MergeContributor {
                source_file: f.path.clone(),
                package: package.to_string(),
                matched_symbols: f.dominant_target_symbols,
            },
        ));
    }
    out
}
