//! The **split-not-merge** invariant detector.
//!
//! A faithful port keeps each source file's identity and each package's boundary:
//! a source file's symbols may be decomposed or relocated, but they should not be
//! *merged* with an unrelated source file into one target file, and a package's
//! files should not be ported into a crate owned by a *different* package. The
//! invariant therefore has two distinct violation types:
//!
//!   * **FILE-MERGE** — a single target Rust file receives SUBSTANTIAL ported
//!     content from 2+ distinct source files. When those source files span 2+
//!     packages the merge also crosses a package boundary (`cross_package`), the
//!     high-severity case.
//!   * **PACKAGE-MISPLACEMENT** — a source file from package `P` is ported into a
//!     crate whose owning package is `Q ≠ P`. This is the package-level "no crate
//!     merges two packages" rule, detected per *source* file (so a single-source
//!     target file that simply landed in the wrong crate is still caught).
//!
//! The signal is entirely graph-derived — the detector reads the port-diff
//! matcher's per-symbol `at_id` matches, already rolled up per source file into
//! [`super::report::TargetFileContribution`] rows (one per destination target
//! file, split into STRONG-tier — exact-module / subtree — and total matches). It
//! never parses a provenance comment, so it survives the renames and refactors the
//! comments miss.
//!
//! **Contribution strength.** A source file `S` *substantially* contributes to a
//! target file `T` only when it lands there with real structural weight, not a
//! one-or-two-symbol name coincidence: ≥ 2 STRONG-tier symbols, OR ≥ 3 matched
//! symbols with ≥ 1 STRONG-tier, OR ≥ 4 matched of any tier (a large name
//! footprint — the shape a cross-package port takes, since it lands entirely via
//! global-name when the target module path differs). A single strong match, or
//! ≤ 3 bare global-name (leaf-only) matches, never qualifies — those are the
//! coincidences (e.g. a shared `render` / `new` leaf) that a
//! plurality-of-dominant-target scheme mistook for merges. Everything is sorted,
//! so the report is deterministic.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::report::FileEntry;

/// One `(source_file → target_file)` contribution: how many of a source file's
/// matched symbols landed in one target file, split by tier strength. The raw
/// input to the detector, one per distinct destination target file of a source
/// file (a source file usually contributes several).
#[derive(Clone, Debug)]
pub struct Contribution {
    /// The contributing source file path (`src/api/transform-messages.ts`).
    pub source_file: String,
    /// The package the source file belongs to (`ai`, `coding-agent`, …). Empty in
    /// a single-package report where the package split is meaningless.
    pub package: String,
    /// The target file this contribution landed in (`crates/pidgin-ai/src/...rs`).
    pub target_file: String,
    /// Symbols that landed here via a STRONG tier (exact-module or subtree).
    pub strong_matched: usize,
    /// Symbols that landed here across all tiers (strong plus global-name).
    pub total_matched: usize,
}

impl Contribution {
    /// Whether this source file *substantially* contributes to its target file —
    /// enough structural weight to be a real port slice, not a one-or-two-symbol
    /// name coincidence. Three ways to qualify:
    ///
    ///   * **≥ 2 STRONG-tier symbols** (exact-module / subtree): the source
    ///     symbols landed in the module they were expected to — the clearest
    ///     structural signal;
    ///   * **≥ 3 matched with ≥ 1 STRONG-tier**: a mixed slice anchored by at
    ///     least one structural match;
    ///   * **≥ 4 matched of any tier**: a large name-footprint. A *cross-package*
    ///     port lands entirely via global-name (the target module path differs, so
    ///     exact-module / subtree can never fire), and a type-heavy module matches
    ///     by PascalCase leaf alone; four or more such matches from one source file
    ///     into one target file is a real footprint, not a coincidence.
    ///
    /// A single strong-tier match, or ≤ 3 bare global-name matches, never qualifies
    /// — those are the coincidences a plurality scheme mistook for merges.
    fn is_substantial(&self) -> bool {
        self.strong_matched >= 2
            || (self.total_matched >= 3 && self.strong_matched >= 1)
            || self.total_matched >= 4
    }
}

/// One source file that substantially contributes to a merged target file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeContributor {
    /// The contributing source file path.
    pub source_file: String,
    /// The package the source file belongs to. Empty in a single-package report.
    pub package: String,
    /// Its STRONG-tier (exact-module / subtree) symbols landing in the target file
    /// — the structural weight it carries into the merge.
    pub strong_matched: usize,
    /// Its total matched symbols landing in the target file (strong + global-name).
    pub total_matched: usize,
}

/// One flagged **FILE-MERGE**: a target file that ≥ 2 distinct source files each
/// substantially contribute to.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeEntry {
    /// The target file that received the merge (`crates/pidgin-ai/src/...rs`).
    pub target_file: String,
    /// The substantial contributing source files, sorted by descending strong then
    /// total matched then path.
    pub contributors: Vec<MergeContributor>,
    /// The distinct packages the contributors span, sorted.
    pub packages: Vec<String>,
    /// Whether the contributors span ≥ 2 distinct packages (a cross-package merge,
    /// the high-severity case).
    pub cross_package: bool,
}

/// One flagged **PACKAGE-MISPLACEMENT**: a source file from `source_package`
/// substantially ported into `target_crate`, whose owning package is different.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Misplacement {
    /// The misplaced source file (`src/providers/provider-composer.ts`).
    pub source_file: String,
    /// The package the source file belongs to (`coding-agent`).
    pub source_package: String,
    /// The target file it landed in (`crates/pidgin-ai/src/providers/composer.rs`).
    pub target_file: String,
    /// The crate that target file belongs to (`pidgin-ai`).
    pub target_crate: String,
    /// The package that owns `target_crate` (`ai`) — the mismatch.
    pub owning_package: String,
    /// STRONG-tier symbols of the source file landing in the target file.
    pub strong_matched: usize,
    /// Total matched symbols of the source file landing in the target file.
    pub total_matched: usize,
}

/// The split-not-merge report: file-merges and package-misplacements.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergeReport {
    /// Every target file with ≥ 2 distinct substantial contributing source files,
    /// most contested (most contributors, then most strong symbols) first.
    pub file_merges: Vec<MergeEntry>,
    /// Every `(source_file, target_file)` where the source file substantially
    /// contributes to a crate owned by a different package, sorted by crate then
    /// target then source path.
    pub misplacements: Vec<Misplacement>,
}

/// The crate a workspace-relative target file belongs to: the segment after
/// `crates/` in `crates/<crate>/src/...`. Empty when the path is not under a
/// crate (a synthetic test may use a bare path).
pub(crate) fn crate_of(target_file: &str) -> &str {
    target_file
        .strip_prefix("crates/")
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("")
}

/// Total STRONG-tier symbols across a file-merge's contributors — the secondary
/// sort key (most structurally contested first).
fn total_strong(e: &MergeEntry) -> usize {
    e.contributors.iter().map(|c| c.strong_matched).sum()
}

impl MergeReport {
    /// Build the split-not-merge report from the flat list of per-target-file
    /// contributions. Only *substantial* contributions count (see
    /// [`Contribution::is_substantial`]); everything else is name-coincidence noise.
    ///
    /// **File-merges**: group the substantial contributions by target file; any
    /// target file with ≥ 2 distinct substantial source files is a file-merge,
    /// tagged `cross_package` when its contributors span ≥ 2 packages.
    ///
    /// **Misplacements**: each target crate's *owning package* is the package that
    /// substantially contributes the most into it (plurality), overridden by
    /// `owning_override` (a crate → package map from config's per-package primary
    /// crate) when the crate is present there. Any substantial contribution whose
    /// source package differs from its crate's owning package is a misplacement.
    ///
    /// Deterministic: contributors are sorted by descending strong then total then
    /// path, file-merges by descending contributor count then strong then path,
    /// and misplacements by crate then target then source path.
    pub fn from_contributions(
        contributions: Vec<Contribution>,
        owning_override: &BTreeMap<String, String>,
    ) -> MergeReport {
        // Keep only the substantial contributions — the rest are the 1–2-symbol
        // name coincidences that inflated the plurality scheme's false positives.
        let substantial: Vec<&Contribution> = contributions
            .iter()
            .filter(|c| c.is_substantial())
            .collect();

        // ---- (A) FILE-MERGE: group substantial contributions by target file ----
        let mut by_target: BTreeMap<&str, Vec<&Contribution>> = BTreeMap::new();
        for c in &substantial {
            by_target.entry(c.target_file.as_str()).or_default().push(c);
        }
        let mut file_merges: Vec<MergeEntry> = Vec::new();
        for (target_file, mut cs) in by_target {
            // Distinct source files only.
            cs.sort_by(|a, b| a.source_file.cmp(&b.source_file));
            cs.dedup_by(|a, b| a.source_file == b.source_file);
            if cs.len() < 2 {
                continue;
            }
            // Present the split proportions: biggest structural contributor first.
            cs.sort_by(|a, b| {
                b.strong_matched
                    .cmp(&a.strong_matched)
                    .then_with(|| b.total_matched.cmp(&a.total_matched))
                    .then_with(|| a.source_file.cmp(&b.source_file))
            });
            let contributors: Vec<MergeContributor> = cs
                .iter()
                .map(|c| MergeContributor {
                    source_file: c.source_file.clone(),
                    package: c.package.clone(),
                    strong_matched: c.strong_matched,
                    total_matched: c.total_matched,
                })
                .collect();
            let mut packages: Vec<String> =
                contributors.iter().map(|c| c.package.clone()).collect();
            packages.sort();
            packages.dedup();
            // An empty package label (single-package report) is not a distinct
            // package for the cross-package test.
            let distinct_pkgs = packages.iter().filter(|p| !p.is_empty()).count();
            let cross_package = distinct_pkgs >= 2;
            file_merges.push(MergeEntry {
                target_file: target_file.to_string(),
                contributors,
                packages,
                cross_package,
            });
        }
        // Most contested first: contributor count, then strong symbols, then path.
        file_merges.sort_by(|a, b| {
            b.contributors
                .len()
                .cmp(&a.contributors.len())
                .then_with(|| total_strong(b).cmp(&total_strong(a)))
                .then_with(|| a.target_file.cmp(&b.target_file))
        });

        // ---- Owning package per crate -------------------------------------
        // Plurality: the package with the most substantial contributions landing in
        // that crate; ties broken by lexicographically smallest package. Config's
        // per-package primary crate (`owning_override`) wins when present.
        let mut crate_pkg_counts: BTreeMap<&str, BTreeMap<&str, usize>> = BTreeMap::new();
        for c in &substantial {
            if c.package.is_empty() {
                continue;
            }
            let cr = crate_of(&c.target_file);
            if cr.is_empty() {
                continue;
            }
            *crate_pkg_counts
                .entry(cr)
                .or_default()
                .entry(c.package.as_str())
                .or_default() += 1;
        }
        let owning = |cr: &str| -> Option<String> {
            if let Some(p) = owning_override.get(cr) {
                return Some(p.clone());
            }
            crate_pkg_counts.get(cr).and_then(|counts| {
                counts
                    .iter()
                    .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
                    .map(|(p, _)| p.to_string())
            })
        };

        // ---- (B) PACKAGE-MISPLACEMENT -------------------------------------
        let mut misplacements: Vec<Misplacement> = Vec::new();
        for c in &substantial {
            if c.package.is_empty() {
                continue;
            }
            let cr = crate_of(&c.target_file);
            if cr.is_empty() {
                continue;
            }
            let Some(own) = owning(cr) else {
                continue;
            };
            if own != c.package {
                misplacements.push(Misplacement {
                    source_file: c.source_file.clone(),
                    source_package: c.package.clone(),
                    target_file: c.target_file.clone(),
                    target_crate: cr.to_string(),
                    owning_package: own,
                    strong_matched: c.strong_matched,
                    total_matched: c.total_matched,
                });
            }
        }
        misplacements.sort_by(|a, b| {
            a.target_crate
                .cmp(&b.target_crate)
                .then_with(|| a.target_file.cmp(&b.target_file))
                .then_with(|| a.source_file.cmp(&b.source_file))
        });

        MergeReport {
            file_merges,
            misplacements,
        }
    }
}

/// Lift a package's port-diff [`FileEntry`] rows into merge contributions: each
/// file emits one [`Contribution`] per target file its matched symbols landed in
/// (its [`super::report::TargetFileContribution`] rows), tagged with `package`.
/// Shared by the single-package report and the cross-package rollup so both feed
/// the detector the same per-destination signal.
pub fn contributions_from_files(files: &[FileEntry], package: &str) -> Vec<Contribution> {
    let mut out = Vec::new();
    for f in files {
        for tc in &f.target_file_contributions {
            out.push(Contribution {
                source_file: f.path.clone(),
                package: package.to_string(),
                target_file: tc.file.clone(),
                strong_matched: tc.strong_matched,
                total_matched: tc.total_matched,
            });
        }
    }
    out
}
