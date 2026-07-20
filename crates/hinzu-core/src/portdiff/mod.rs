//! Cross-language **port-progress matching**: given a SOURCE codebase's symbol
//! graph + porting plan (e.g. a TypeScript package) and a TARGET codebase's
//! symbol graph (e.g. the Rust port of it), decide — file by file, symbol by
//! symbol — how much of the source has actually been ported, in a way that
//! survives file **decomposition** and **relocation** (a source file whose
//! contents were split across, or moved into, a different target subtree).
//!
//! This is a native, config-driven port of the validated JS prototype
//! (`scripts/port-graph-match.mjs`). It is entirely deterministic — everything
//! is sorted, and no clock or randomness is consulted — so re-running over the
//! same inputs yields byte-identical output.
//!
//! ## What it does, in five steps
//!
//! 1. **Normalize symbols across languages.** A source id like
//!    `src/api/google-shared#convertMessages` and a target id like
//!    `atilla_ai::api::google_shared::convert_messages` both reduce to a
//!    `(module_path, leaf)` pair in a common spelling: file segments
//!    kebab→snake, functions camelCase→snake_case, PascalCase types and
//!    SCREAMING consts kept verbatim. Target module paths are *anchored on the
//!    defining file* (`crates/atilla-ai/src/api/anthropic/boundary.rs` →
//!    `api/anthropic/boundary`, `.../X/mod.rs` → `X`), and trait-impl
//!    (`<Type as Trait>::method`) / synthetic (`::{closure#N}`, `::FIELDS`, …)
//!    tails are handled.
//! 2. **Map each source file to a target subtree** via *distinctive-leaf-weighted
//!    clustering*: a leaf that appears in `K` target modules casts a vote of
//!    `1/K`, so a file-distinctive name concentrates the cluster while a generic
//!    helper name does not; the deepest subtree retaining ≥ `cluster_vote_retain`
//!    of the winning vote mass is chosen. This recovers relocated / decomposed
//!    files that a naive path match misses.
//! 3. **Match each source symbol** in tiers — exact-module, subtree,
//!    global-name, else unmatched.
//! 4. **Graph-confirm** each match by edge overlap: of the source symbol's
//!    internal callees that themselves matched, what fraction does the matched
//!    target counterpart also call? This *labels confidence only* — it never
//!    drops a name-match.
//! 5. **Band each file** — DONE (test-verified via the conformance native set),
//!    PORTED (coverage ≥ `ported_threshold`, not native), STARTED (≥1 match,
//!    below threshold), NOT-STARTED (0) — and roll the bands up per wave, plus a
//!    ready-frontier of unported files whose dependencies are all ported.
//!
//! ## Honesty stance
//!
//! The matching is **structural, not a correctness proof**. DONE is the only
//! band with test backing (it comes from the conformance manifest); the others
//! are graph-derived and *under*-count by design (an unmatched symbol is only
//! ever a missed match, never a fabricated one). The matchable-symbol
//! denominator excludes synthetic/anonymous source symbols, and a clustered file
//! mapping can point at a subtree rather than a single file. These caveats travel
//! with the data in [`Fidelity`].

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::Deserialize;

use crate::graph::GraphOutput;
use crate::plan::PlanOutput;

mod config;
mod delta;
mod merge;
mod report;
pub use config::{ConformanceConfig, NamingRules, PortDiffConfig};
pub use delta::{
    band_rank, diff_cross_reports, diff_multi_reports, diff_reports, BandNetMovement,
    BandTransition, DeltaTotals, Direction, FileDelta, PortDiffDelta, Verdict,
};
pub use merge::{MergeContributor, MergeEntry, MergeReport};
pub use report::{
    Band, BandCounts, ConformanceCrosscheck, Fidelity, FileEntry, FileMapSummary,
    FileTierBreakdown, FrontierEntry, GraphConfirmSummary, MultiPackageReport, NaiveVsGraph,
    Overall, PackageClosureRollup, PackageRollup, PortDiffReport, RollupTotals,
    RootedCrossPackageReport, TierCounts, WaveBand,
};

// ===========================================================================
// Normalization (config-driven)
// ===========================================================================

/// `google-shared` → `google_shared`.
fn kebab_to_snake(s: &str) -> String {
    s.replace('-', "_")
}

/// `convertMessages` → `convert_messages`, replicating the prototype's two-pass
/// regex: (1) underscore between a lower/digit and an upper; (2) underscore
/// before the final upper of an all-caps run that starts a new lower word; then
/// lowercase.
fn camel_to_snake(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    // Pass 1: `([a-z0-9])([A-Z])` -> `$1_$2`.
    let mut pass1: Vec<char> = Vec::with_capacity(chars.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 {
            let prev = chars[i - 1];
            if (prev.is_ascii_lowercase() || prev.is_ascii_digit()) && c.is_ascii_uppercase() {
                pass1.push('_');
            }
        }
        pass1.push(c);
    }
    // Pass 2: `([A-Z]+)([A-Z][a-z])` -> `$1_$2`: an upper preceded by an upper and
    // followed by a lower gets an underscore before it.
    let mut pass2: Vec<char> = Vec::with_capacity(pass1.len() + 4);
    for i in 0..pass1.len() {
        let c = pass1[i];
        if i > 0 && i + 1 < pass1.len() {
            let prev = pass1[i - 1];
            let next = pass1[i + 1];
            if c.is_ascii_uppercase() && prev.is_ascii_uppercase() && next.is_ascii_lowercase() {
                pass2.push('_');
            }
        }
        pass2.push(c);
    }
    pass2.into_iter().collect::<String>().to_lowercase()
}

/// `^[A-Z0-9]+(_[A-Z0-9]+)*$` with at least one letter.
fn is_screaming(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut has_alpha = false;
    let mut prev_us = true; // no leading/trailing/doubled underscore, no empty run
    let mut run_len = 0usize;
    for c in s.chars() {
        if c == '_' {
            if prev_us {
                return false;
            }
            prev_us = true;
            run_len = 0;
        } else if c.is_ascii_uppercase() || c.is_ascii_digit() {
            if c.is_ascii_uppercase() {
                has_alpha = true;
            }
            prev_us = false;
            run_len += 1;
        } else {
            return false;
        }
    }
    run_len > 0 && has_alpha
}

/// Starts uppercase and contains a lowercase (`AnthropicModel`).
fn is_pascal(s: &str) -> bool {
    s.chars()
        .next()
        .map(|c| c.is_ascii_uppercase())
        .unwrap_or(false)
        && s.chars().any(|c| c.is_ascii_lowercase())
}

/// Normalize a leaf identifier for cross-language comparison: keep SCREAMING
/// consts and PascalCase types verbatim (when configured), else camel→snake.
fn norm_leaf(leaf: &str, rules: &NamingRules) -> String {
    if leaf.is_empty() {
        return leaf.to_string();
    }
    if rules.keep_screaming_consts && is_screaming(leaf) {
        return leaf.to_string();
    }
    if rules.keep_pascal_types && is_pascal(leaf) {
        return leaf.to_string();
    }
    camel_to_snake(leaf)
}

/// A source file path → its normalized module path (source-relative, snake
/// segments, extension + compound suffixes stripped).
fn source_file_to_module(file: &str, rules: &NamingRules) -> String {
    let mut f = file;
    let src_pref = format!("{}/", rules.source_src_prefix);
    if let Some(rest) = f.strip_prefix(&src_pref) {
        f = rest;
    }
    // Strip the TS extension, then any compound suffix (e.g. `.lazy`).
    let mut base = f.to_string();
    for ext in [".tsx", ".ts"] {
        if let Some(stripped) = base.strip_suffix(ext) {
            base = stripped.to_string();
            break;
        }
    }
    for suf in &rules.strip_suffixes {
        if let Some(stripped) = base.strip_suffix(suf.as_str()) {
            base = stripped.to_string();
        }
    }
    base.split('/')
        .map(kebab_to_snake)
        .collect::<Vec<_>>()
        .join("/")
}

/// A target file (workspace-relative) → its normalized module path in the
/// source-relative form: strip the crate `src` prefix, the `.rs` extension, and a
/// trailing `/mod`.
fn target_file_to_module(file: &str, rules: &NamingRules) -> String {
    let mut f = file;
    // Try each configured target crate's src prefix (a package may map to several
    // crates); the first that matches wins. Fall back to the generic
    // `crates/<x>/src/` shape for any crate not spelled out in the config.
    let stripped = rules.target_src_prefix.iter().find_map(|p| {
        let pref = format!("{p}/");
        f.strip_prefix(&pref)
    });
    if let Some(rest) = stripped {
        f = rest;
    } else if let Some(rest) = strip_generic_crate_src(f) {
        f = rest;
    }
    let mut m = f.strip_suffix(".rs").unwrap_or(f).to_string();
    if let Some(base) = m.strip_suffix("/mod") {
        m = base.to_string();
    }
    m
}

/// Whether a target file lives in the package's PRIMARY target crate — the first
/// entry of [`NamingRules::target_src_prefix`]. A source package may map to
/// several target crates (PR1); the primary is the config's first, and a match
/// landing outside it drives the RELOCATED band. When no target crate prefix is
/// configured, everything counts as primary (no relocation is possible), and for
/// a single-crate package the sole prefix *is* the primary, so every match is
/// primary and RELOCATED never fires.
fn target_file_in_primary_crate(file: &str, rules: &NamingRules) -> bool {
    match rules.target_src_prefix.first() {
        Some(primary) => file.starts_with(&format!("{primary}/")),
        None => true,
    }
}

/// The generic `crates/<one-segment>/src/<rest>` shape → `<rest>`.
fn strip_generic_crate_src(file: &str) -> Option<&str> {
    let rest = file.strip_prefix("crates/")?;
    let (_crate, tail) = rest.split_once('/')?;
    tail.strip_prefix("src/")
}

/// A source id `src/<segs>/<name>#<Symbol>` → the raw leaf after `#`, if any.
fn source_leaf_raw(id: &str) -> Option<&str> {
    id.split_once('#').map(|(_, leaf)| leaf)
}

/// Is a source leaf a real, matchable named symbol (not a synthetic anon /
/// callback / positional target)?
fn source_matchable_leaf(leaf: &str) -> bool {
    if leaf.is_empty() {
        return false;
    }
    if leaf.contains('(') || leaf.contains(')') {
        return false;
    }
    // `@\d` positional (e.g. `foo@1095`).
    let bytes = leaf.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'@' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            return false;
        }
    }
    true
}

/// The match name of a source leaf, splitting a `Type.method` into its method.
fn source_leaf_match_name(leaf: &str, rules: &NamingRules) -> String {
    if let Some(idx) = leaf.rfind('.') {
        norm_leaf(&leaf[idx + 1..], rules)
    } else {
        norm_leaf(leaf, rules)
    }
}

/// A target id → its matchable raw leaf, if any. Handles plain
/// `a::b::name`, trait-impl `<Type as Trait>::method`, and rejects synthetic
/// tails (`{closure#N}`, `{constant#N}`, `FIELDS`, `_`, empty).
fn target_leaf_raw(id: &str) -> Option<String> {
    let work = if id.starts_with('<') {
        // trait-impl `<TYPE as TRAIT>::method` — the method follows the LAST `>::`.
        let close = id.rfind(">::")?;
        &id[close + 3..]
    } else {
        id
    };
    let leaf = work.rsplit("::").next().unwrap_or(work);
    if leaf.is_empty()
        || leaf == "FIELDS"
        || leaf == "_"
        || is_braced_synthetic(leaf, "closure")
        || is_braced_synthetic(leaf, "constant")
    {
        return None;
    }
    Some(leaf.to_string())
}

/// `{closure#12}` / `{constant#3}` style synthetic tails.
fn is_braced_synthetic(leaf: &str, word: &str) -> bool {
    let inner = match leaf.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
        Some(inner) => inner,
        None => return false,
    };
    match inner.strip_prefix(word).and_then(|s| s.strip_prefix('#')) {
        Some(num) => !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()),
        None => false,
    }
}

/// Round to 2 dp (`Math.round(x*100)/100`).
fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Round to 3 dp, matching the prototype's `round`.
fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// `mod == pref || mod.startsWith(pref + "/")`.
fn under_prefix(module: &str, pref: &str) -> bool {
    module == pref
        || (module.len() > pref.len()
            && module.starts_with(pref)
            && module.as_bytes()[pref.len()] == b'/')
}

// ===========================================================================
// Working structures
// ===========================================================================

/// A normalized target symbol.
struct AtSym {
    id: String,
    module: String,
    leaf_norm: String,
    is_impl: bool,
    /// Whether this target symbol's defining file lives in the package's PRIMARY
    /// target crate (the first entry of `target_src_prefix`). Matches that land
    /// in a non-primary crate drive the RELOCATED band. Always `true` for
    /// single-crate packages, so they never produce RELOCATED.
    in_primary_crate: bool,
    /// The target symbol's defining file, workspace-relative
    /// (`crates/<crate>/src/x.rs`) — the concrete destination the split-not-merge
    /// detector inverts (a source file's dominant target file is the plurality of
    /// its matched symbols' `file`s).
    file: String,
}

/// A normalized, matched source symbol.
struct PiSym {
    id: String,
    file: String,
    module: String,
    match_name: String,
    tier: Tier,
    at_id: Option<String>,
    graph: Option<GraphConfirm>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tier {
    ExactModule,
    Subtree,
    GlobalName,
    Unmatched,
}

impl Tier {
    fn matched(self) -> bool {
        self != Tier::Unmatched
    }
}

/// Per-symbol graph-confirm state.
struct GraphConfirm {
    evaluable: bool,
    overlap: f64,
    confirmed: bool,
}

/// A source file → target mapping decision.
struct FileMap {
    subtree: Option<String>,
    method: Option<String>,
    votes: Option<f64>,
}

// ===========================================================================
// Entry point
// ===========================================================================

/// Compute the cross-language port-progress report.
///
/// `source_graph` + `source_plan` describe the codebase being ported *from*
/// (its symbol graph and wave/group plan); `target_graph` describes the codebase
/// being ported *to*. `config` supplies the language pair, naming rules, band
/// thresholds, and the optional conformance oracle. `conformance_manifest_json`
/// is the *contents* of the conformance manifest the oracle reads, supplied as
/// data by the caller: core never opens a file (the functional-core boundary
/// keeps every filesystem effect out of the analysis engine), it only parses the
/// text with the trusted-pure `serde_json`. Pass `None` when there is no
/// manifest — then no file is banded DONE. The result is deterministic.
pub fn port_diff(
    source_graph: &GraphOutput,
    source_plan: &PlanOutput,
    target_graph: &GraphOutput,
    config: &PortDiffConfig,
    conformance_manifest_json: Option<&str>,
) -> PortDiffReport {
    let rules = &config.naming;
    let mut notes: Vec<String> = Vec::new();

    // ---- Build the target symbol index ------------------------------------
    // Internal (non-external, crate-local) target symbols, in id order (the
    // graph emits symbols sorted by id), skipping unmatchable ids.
    let mut at_syms: Vec<AtSym> = Vec::new();
    for s in &target_graph.symbols {
        if s.external {
            continue;
        }
        let Some(file) = s.file.as_deref() else {
            continue;
        };
        if !file.starts_with("crates/") {
            continue;
        }
        let Some(leaf_raw) = target_leaf_raw(&s.id) else {
            continue;
        };
        at_syms.push(AtSym {
            module: target_file_to_module(file, rules),
            leaf_norm: norm_leaf(&leaf_raw, rules),
            is_impl: s.id.starts_with('<'),
            in_primary_crate: target_file_in_primary_crate(file, rules),
            file: file.to_string(),
            id: s.id.clone(),
        });
    }

    // leaf_norm -> target indices; (module::leaf) and (basename::leaf) pairs.
    let mut at_by_leaf: HashMap<String, Vec<usize>> = HashMap::new();
    let mut at_by_module_leaf: HashMap<String, Vec<usize>> = HashMap::new();
    let mut at_by_base_leaf: HashMap<String, Vec<usize>> = HashMap::new();
    // Insertion-ordered unique module set (first occurrence in id order).
    let mut at_modules: Vec<String> = Vec::new();
    let mut at_module_seen: HashSet<String> = HashSet::new();
    for (i, r) in at_syms.iter().enumerate() {
        at_by_leaf.entry(r.leaf_norm.clone()).or_default().push(i);
        at_by_module_leaf
            .entry(format!("{}::{}", r.module, r.leaf_norm))
            .or_default()
            .push(i);
        let base = r.module.rsplit('/').next().unwrap_or(&r.module);
        at_by_base_leaf
            .entry(format!("{}::{}", base, r.leaf_norm))
            .or_default()
            .push(i);
        if at_module_seen.insert(r.module.clone()) {
            at_modules.push(r.module.clone());
        }
    }

    // Candidate prefixes = every module + every ancestor dir, insertion-ordered.
    let mut at_prefixes: Vec<String> = Vec::new();
    let mut at_prefix_seen: HashSet<String> = HashSet::new();
    for m in &at_modules {
        let segs: Vec<&str> = m.split('/').collect();
        for i in 1..=segs.len() {
            let pref = segs[..i].join("/");
            if at_prefix_seen.insert(pref.clone()) {
                at_prefixes.push(pref);
            }
        }
    }

    // Target adjacency: from-id -> set of callee leaf_norms (for graph-confirm).
    let at_id_to_idx: HashMap<&str, usize> = at_syms
        .iter()
        .enumerate()
        .map(|(i, r)| (r.id.as_str(), i))
        .collect();
    let mut at_out_leaves: HashMap<&str, HashSet<String>> = HashMap::new();
    for e in &target_graph.edges {
        if let Some(&to_idx) = at_id_to_idx.get(e.to.as_str()) {
            at_out_leaves
                .entry(e.from.as_str())
                .or_default()
                .insert(at_syms[to_idx].leaf_norm.clone());
        }
    }

    // ---- Build the source symbol universe ---------------------------------
    let source_files: Vec<String> = source_graph
        .files
        .iter()
        .filter(|f| f.path.starts_with(&format!("{}/", rules.source_src_prefix)))
        .map(|f| f.path.clone())
        .collect();
    let source_file_set: HashSet<&str> = source_files.iter().map(|s| s.as_str()).collect();
    let source_file_fan_in: HashMap<&str, usize> = source_graph
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.fan_in))
        .collect();

    let src_pref = format!("{}/", rules.source_src_prefix);
    let mut pi_syms: Vec<PiSym> = Vec::new();
    let mut pi_synthetic_excluded = 0usize;
    for s in &source_graph.symbols {
        if s.external {
            continue;
        }
        let Some(file) = s.file.as_deref() else {
            continue;
        };
        if !file.starts_with(&src_pref) {
            continue;
        }
        match source_leaf_raw(&s.id) {
            Some(leaf) if source_matchable_leaf(leaf) => {
                pi_syms.push(PiSym {
                    module: source_file_to_module(file, rules),
                    match_name: source_leaf_match_name(leaf, rules),
                    file: file.to_string(),
                    id: s.id.clone(),
                    tier: Tier::Unmatched,
                    at_id: None,
                    graph: None,
                });
            }
            _ => pi_synthetic_excluded += 1,
        }
    }
    let pi_id_set: HashSet<&str> = pi_syms.iter().map(|s| s.id.as_str()).collect();

    // Source internal call edges: from-id -> [to-id] (matchable targets only).
    let mut pi_out: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in &source_graph.edges {
        if pi_id_set.contains(e.to.as_str()) {
            pi_out
                .entry(e.from.as_str())
                .or_default()
                .push(e.to.as_str());
        }
    }

    // Per source file -> its matchable leaf match-names (for clustering).
    let mut pi_file_leaves: HashMap<&str, Vec<String>> = HashMap::new();
    for s in &pi_syms {
        pi_file_leaves
            .entry(s.file.as_str())
            .or_default()
            .push(s.match_name.clone());
    }

    // ---- Decomposition-aware file map -------------------------------------
    let mut file_map: HashMap<String, FileMap> = HashMap::new();
    for f in &source_files {
        file_map.insert(
            f.clone(),
            map_source_file(
                f,
                rules,
                config.cluster_vote_retain,
                &at_modules,
                at_module_seen.contains(&source_file_to_module(f, rules)),
                &at_by_leaf,
                &at_syms,
                &at_prefixes,
                pi_file_leaves
                    .get(f.as_str())
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]),
            ),
        );
    }

    // ---- Per-symbol matching ----------------------------------------------
    for s in &mut pi_syms {
        let subtree = file_map.get(&s.file).and_then(|m| m.subtree.as_deref());
        let (tier, at_id) = match_symbol(
            &s.module,
            &s.match_name,
            subtree,
            &at_by_module_leaf,
            &at_by_leaf,
            &at_by_base_leaf,
            &at_syms,
        );
        s.tier = tier;
        s.at_id = at_id;
    }

    // ---- Graph-confirm -----------------------------------------------------
    // Index source symbols by id for callee lookup.
    let match_name_of: HashMap<&str, (&str, bool)> = pi_syms
        .iter()
        .map(|s| (s.id.as_str(), (s.match_name.as_str(), s.tier.matched())))
        .collect();
    let at_id_of: HashMap<&str, Option<&str>> = pi_syms
        .iter()
        .map(|s| (s.id.as_str(), s.at_id.as_deref()))
        .collect();
    let mut graph_updates: Vec<(usize, GraphConfirm)> = Vec::new();
    for (i, s) in pi_syms.iter().enumerate() {
        if !s.tier.matched() {
            continue;
        }
        let callees = pi_out.get(s.id.as_str());
        let matched_callee_names: Vec<&str> = callees
            .map(|cs| {
                cs.iter()
                    .filter_map(|c| match_name_of.get(*c))
                    .filter(|(_, m)| *m)
                    .map(|(name, _)| *name)
                    .collect()
            })
            .unwrap_or_default();
        if matched_callee_names.is_empty() {
            graph_updates.push((
                i,
                GraphConfirm {
                    evaluable: false,
                    overlap: 0.0,
                    confirmed: false,
                },
            ));
            continue;
        }
        let at_id = at_id_of.get(s.id.as_str()).copied().flatten();
        let empty = HashSet::new();
        let at_callee_leaves = at_id.and_then(|id| at_out_leaves.get(id)).unwrap_or(&empty);
        let hit = matched_callee_names
            .iter()
            .filter(|name| at_callee_leaves.contains(**name))
            .count();
        let overlap = hit as f64 / matched_callee_names.len() as f64;
        graph_updates.push((
            i,
            GraphConfirm {
                evaluable: true,
                overlap,
                confirmed: overlap >= 0.5,
            },
        ));
    }
    for (i, g) in graph_updates {
        pi_syms[i].graph = Some(g);
    }

    // ---- Conformance native oracle (best-effort) --------------------------
    let (native_files, native_module_count) =
        load_native_files(&config.conformance, conformance_manifest_json, &mut notes);
    let native_set: HashSet<&str> = native_files.iter().map(|s| s.as_str()).collect();

    // ---- Aggregate --------------------------------------------------------
    let total_syms = pi_syms.len();
    let matched_syms = pi_syms.iter().filter(|s| s.tier.matched()).count();
    let mut tier_counts = TierCounts::default();
    for s in &pi_syms {
        match s.tier {
            Tier::ExactModule => tier_counts.exact_module += 1,
            Tier::Subtree => tier_counts.subtree += 1,
            Tier::GlobalName => tier_counts.global_name += 1,
            Tier::Unmatched => tier_counts.unmatched += 1,
        }
    }

    let evaluable: Vec<&PiSym> = pi_syms
        .iter()
        .filter(|s| s.graph.as_ref().map(|g| g.evaluable).unwrap_or(false))
        .collect();
    let confirmed = evaluable
        .iter()
        .filter(|s| s.graph.as_ref().unwrap().confirmed)
        .count();
    let mean_overlap = if evaluable.is_empty() {
        0.0
    } else {
        evaluable
            .iter()
            .map(|s| s.graph.as_ref().unwrap().overlap)
            .sum::<f64>()
            / evaluable.len() as f64
    };

    // Per-file aggregate + bands.
    let mut per_file_syms: HashMap<&str, Vec<&PiSym>> = HashMap::new();
    for s in &pi_syms {
        per_file_syms.entry(s.file.as_str()).or_default().push(s);
    }
    let mut file_entries: Vec<FileEntry> = Vec::with_capacity(source_files.len());
    let mut band_counts = BandCounts::default();
    let mut band_of: HashMap<&str, Band> = HashMap::new();
    let mut file_map_summary = FileMapSummary::default();
    for f in &source_files {
        let syms: &[&PiSym] = per_file_syms
            .get(f.as_str())
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let total = syms.len();
        let matched = syms.iter().filter(|s| s.tier.matched()).count();
        let ev: Vec<&&PiSym> = syms
            .iter()
            .filter(|s| s.graph.as_ref().map(|g| g.evaluable).unwrap_or(false))
            .collect();
        let conf = ev
            .iter()
            .filter(|s| s.graph.as_ref().unwrap().confirmed)
            .count();
        let coverage = if total > 0 {
            Some(matched as f64 / total as f64)
        } else {
            None
        };
        let gcov = if !ev.is_empty() {
            Some(conf as f64 / ev.len() as f64)
        } else {
            None
        };
        let fm = file_map.get(f);
        let has_mapped = fm.map(|m| m.subtree.is_some()).unwrap_or(false);

        let band = classify_band(
            native_set.contains(f.as_str()),
            total,
            coverage,
            matched >= 1 || has_mapped,
            config.ported_threshold,
        );
        // RELOCATED override: of this file's matched symbols that resolved to a
        // target symbol, tally how many landed in the PRIMARY target crate vs a
        // secondary one. If the port moved predominantly (> 50%) into a secondary
        // crate, relabel PORTED/STARTED as RELOCATED. Only these two bands are
        // overridden — DONE and NOT-STARTED keep their meaning. Single-crate
        // packages have no secondary crate, so `secondary` stays 0 and the band
        // is untouched.
        let (mut resolved, mut secondary) = (0usize, 0usize);
        // Tally which target FILE each matched symbol resolved to, so the file's
        // dominant target file (the plurality) can anchor the split-not-merge
        // detector. Keyed by target file path, insertion order irrelevant (the
        // winner is chosen by count with a deterministic path tie-break).
        let mut tf_counts: HashMap<&str, usize> = HashMap::new();
        for s in syms {
            if !s.tier.matched() {
                continue;
            }
            if let Some(idx) = s.at_id.as_deref().and_then(|id| at_id_to_idx.get(id)) {
                resolved += 1;
                if !at_syms[*idx].in_primary_crate {
                    secondary += 1;
                }
                *tf_counts.entry(at_syms[*idx].file.as_str()).or_default() += 1;
            }
        }
        // Plurality target file; ties broken by the lexicographically smallest
        // path so the choice is deterministic.
        let dominant = tf_counts
            .iter()
            .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)));
        let dominant_target_file = dominant.map(|(f, _)| f.to_string());
        let dominant_target_symbols = dominant.map(|(_, &c)| c).unwrap_or(0);
        let band = if matches!(band, Band::Ported | Band::Started)
            && resolved > 0
            && secondary * 2 > resolved
        {
            Band::Relocated
        } else {
            band
        };
        band_counts.bump(band);
        band_of.insert(f.as_str(), band);

        let mut tb = FileTierBreakdown::default();
        for s in syms {
            match s.tier {
                Tier::ExactModule => tb.exact_module += 1,
                Tier::Subtree => tb.subtree += 1,
                Tier::GlobalName => tb.global_name += 1,
                Tier::Unmatched => tb.unmatched += 1,
            }
        }

        let map_method = fm.and_then(|m| m.method.clone());
        match map_method.as_deref() {
            Some("exact") => file_map_summary.exact += 1,
            Some("exact-subtree") => file_map_summary.exact_subtree += 1,
            Some("graph-cluster") => file_map_summary.graph_cluster += 1,
            _ => file_map_summary.unmapped += 1,
        }

        file_entries.push(FileEntry {
            module: source_file_to_module(f, rules),
            path: f.clone(),
            band,
            coverage: coverage.map(round3),
            graph_confirmed_coverage: gcov.map(round3),
            mapped_target: fm.and_then(|m| m.subtree.clone()),
            map_method,
            map_votes: fm.and_then(|m| m.votes),
            dominant_target_file,
            dominant_target_symbols,
            total_symbols: total,
            matched_symbols: matched,
            tier_breakdown: tb,
            fan_in: source_file_fan_in.get(f.as_str()).copied().unwrap_or(0),
        });
    }
    let fa_by_path: HashMap<&str, &FileEntry> = file_entries
        .iter()
        .map(|fa| (fa.path.as_str(), fa))
        .collect();

    // ---- Per-wave band breakdown ------------------------------------------
    let group_by_id: HashMap<&str, &crate::plan::PlanGroup> = source_plan
        .groups
        .iter()
        .map(|g| (g.id.as_str(), g))
        .collect();
    let mut waves: Vec<WaveBand> = Vec::new();
    for w in &source_plan.waves {
        let mut files: BTreeSet<&str> = BTreeSet::new();
        for gid in &w.group_ids {
            if let Some(g) = group_by_id.get(gid.as_str()) {
                for f in &g.files {
                    if source_file_set.contains(f.as_str()) {
                        files.insert(f.as_str());
                    }
                }
            }
        }
        if files.is_empty() {
            continue;
        }
        let mut bands = BandCounts::default();
        let mut sym_total = 0usize;
        let mut sym_matched = 0usize;
        for f in &files {
            if let Some(fa) = fa_by_path.get(*f) {
                bands.bump(fa.band);
                sym_total += fa.total_symbols;
                sym_matched += fa.matched_symbols;
            }
        }
        waves.push(WaveBand {
            wave: w.wave,
            files: files.len(),
            bands,
            symbols_total: sym_total,
            symbols_matched: sym_matched,
            symbols_pct: if sym_total > 0 {
                round3(sym_matched as f64 / sym_total as f64)
            } else {
                0.0
            },
        });
    }

    // ---- Ready frontier ----------------------------------------------------
    let mut file_deps: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in &source_graph.file_edges {
        file_deps
            .entry(e.from.as_str())
            .or_default()
            .push(e.to.as_str());
    }
    let mut ready_frontier: Vec<FrontierEntry> = Vec::new();
    for fa in &file_entries {
        if fa.band == Band::Done || fa.band == Band::Ported {
            continue;
        }
        let deps = file_deps.get(fa.path.as_str());
        let src_deps: Vec<&str> = deps
            .map(|ds| {
                ds.iter()
                    .copied()
                    .filter(|d| d.starts_with(&src_pref) && *d != fa.path)
                    .collect()
            })
            .unwrap_or_default();
        let ok = src_deps.iter().all(|d| {
            matches!(
                band_of.get(*d),
                Some(Band::Ported) | Some(Band::Done) | None
            )
        });
        if ok {
            ready_frontier.push(FrontierEntry {
                path: fa.path.clone(),
                band: fa.band,
                fan_in: fa.fan_in,
                total_symbols: fa.total_symbols,
                matched_symbols: fa.matched_symbols,
                coverage: fa.coverage,
                dep_count: src_deps.len(),
                mapped_target: fa.mapped_target.clone(),
            });
        }
    }
    // Highest fan-in first; path as a stable tiebreak.
    ready_frontier.sort_by(|a, b| b.fan_in.cmp(&a.fan_in).then_with(|| a.path.cmp(&b.path)));
    let ready_frontier_total = ready_frontier.len();
    ready_frontier.truncate(25);

    // ---- Naive-vs-graph delta ---------------------------------------------
    let naive_files_matched = file_entries
        .iter()
        .filter(|fa| fa.map_method.as_deref() == Some("exact"))
        .count();
    let graph_files_matched = file_entries
        .iter()
        .filter(|fa| fa.band != Band::NotStarted)
        .count();
    let recovered_files: Vec<String> = file_entries
        .iter()
        .filter(|fa| {
            matches!(
                fa.map_method.as_deref(),
                Some("graph-cluster") | Some("exact-subtree")
            )
        })
        .map(|fa| fa.path.clone())
        .collect();

    // ---- Conformance cross-check ------------------------------------------
    let conformance_crosscheck = ConformanceCrosscheck {
        native_modules: native_module_count,
        native_files: {
            let mut v: Vec<String> = native_files.iter().cloned().collect();
            v.sort();
            v
        },
        done_band: band_counts.done,
        ported_plus_done: band_counts.ported + band_counts.done,
        note: "DONE is test-verified (conformance native modules). PORTED / STARTED are \
               structural (graph-derived) and under-count by design. DONE + PORTED is a \
               file-level upper bound on what MIGHT pass, not a claim that it does."
            .to_string(),
    };

    // ---- Split-not-merge detector -----------------------------------------
    // Invert this package's `source_file -> dominant_target_file` relation. Every
    // contributor shares the package label, so only same-package file-merges can
    // surface here; the cross-package rollup (`MultiPackageReport::merges`) tags
    // each package and matches against the union of target crates.
    let merges = merge::MergeReport::from_contributions(merge::contributions_from_files(
        &file_entries,
        config.package.as_deref().unwrap_or(""),
    ));

    PortDiffReport {
        source_kind: config.source_kind.clone(),
        target_kind: config.target_kind.clone(),
        overall: Overall {
            source_files_total: source_files.len(),
            symbols_total: total_syms,
            symbols_synthetic_excluded: pi_synthetic_excluded,
            symbols_matched: matched_syms,
            symbols_matched_pct: if total_syms > 0 {
                round3(matched_syms as f64 / total_syms as f64)
            } else {
                0.0
            },
            tier_counts,
            graph: GraphConfirmSummary {
                evaluable: evaluable.len(),
                confirmed,
                confirmed_pct_of_evaluable: round3(
                    confirmed as f64 / evaluable.len().max(1) as f64,
                ),
                mean_edge_overlap: round3(mean_overlap),
            },
            bands: band_counts,
        },
        file_map_summary,
        files: file_entries,
        waves,
        ready_frontier,
        ready_frontier_total,
        naive_vs_graph: NaiveVsGraph {
            naive_files_matched,
            graph_files_matched,
            recovered_count: recovered_files.len(),
            recovered_files,
        },
        conformance_crosscheck,
        merges,
        fidelity: Fidelity {
            structural_not_correctness: true,
            matchable_denominator:
                "Matchable source symbols = non-external, in-source-tree, named symbols; \
                 anonymous/callback/positional synthetic symbols are excluded from the \
                 denominator (counted separately as symbols_synthetic_excluded)."
                    .to_string(),
            cluster_caveat:
                "A file mapped by graph-cluster points at a target SUBTREE (possibly several \
                 modules), not a single file — the source file's symbols were decomposed or \
                 relocated across it. Coverage is still per-symbol, but the target anchor is a \
                 cluster root."
                    .to_string(),
            notes,
        },
    }
}

/// The per-file band decision.
fn classify_band(
    is_native: bool,
    total: usize,
    coverage: Option<f64>,
    has_match_or_map: bool,
    ported_threshold: f64,
) -> Band {
    if is_native {
        Band::Done
    } else if total > 0 && coverage.map(|c| c >= ported_threshold).unwrap_or(false) {
        Band::Ported
    } else if has_match_or_map {
        Band::Started
    } else {
        Band::NotStarted
    }
}

/// Pick the representative target symbol from a candidate list: prefer inherent
/// (non trait-impl) definitions, then the shortest id (most direct). Ties on
/// length break by id, matching the prototype's id-order stable sort.
fn pick_rep(cands: &[usize], at_syms: &[AtSym]) -> usize {
    let non_impl: Vec<usize> = cands
        .iter()
        .copied()
        .filter(|&i| !at_syms[i].is_impl)
        .collect();
    let pool: &[usize] = if non_impl.is_empty() {
        cands
    } else {
        &non_impl
    };
    *pool
        .iter()
        .min_by(|&&a, &&b| {
            at_syms[a]
                .id
                .len()
                .cmp(&at_syms[b].id.len())
                .then_with(|| at_syms[a].id.cmp(&at_syms[b].id))
        })
        .expect("candidate list is non-empty")
}

/// Tiered per-symbol match: exact-module → subtree → global-name (by
/// basename, then bare leaf) → unmatched.
#[allow(clippy::too_many_arguments)]
fn match_symbol(
    module: &str,
    leaf: &str,
    subtree: Option<&str>,
    at_by_module_leaf: &HashMap<String, Vec<usize>>,
    at_by_leaf: &HashMap<String, Vec<usize>>,
    at_by_base_leaf: &HashMap<String, Vec<usize>>,
    at_syms: &[AtSym],
) -> (Tier, Option<String>) {
    // Tier 1: exact-module.
    if let Some(cands) = at_by_module_leaf.get(&format!("{}::{}", module, leaf)) {
        let rep = pick_rep(cands, at_syms);
        return (Tier::ExactModule, Some(at_syms[rep].id.clone()));
    }
    // Tier 2: subtree.
    if let Some(subtree) = subtree {
        if let Some(all) = at_by_leaf.get(leaf) {
            let cands: Vec<usize> = all
                .iter()
                .copied()
                .filter(|&i| under_prefix(&at_syms[i].module, subtree))
                .collect();
            if !cands.is_empty() {
                let rep = pick_rep(&cands, at_syms);
                return (Tier::Subtree, Some(at_syms[rep].id.clone()));
            }
        }
    }
    // Tier 3: global-name by (basename, leaf).
    let base = module.rsplit('/').next().unwrap_or(module);
    if let Some(cands) = at_by_base_leaf.get(&format!("{}::{}", base, leaf)) {
        let rep = pick_rep(cands, at_syms);
        return (Tier::GlobalName, Some(at_syms[rep].id.clone()));
    }
    // Tier 3b: weakest global — bare leaf anywhere.
    if let Some(cands) = at_by_leaf.get(leaf) {
        let rep = pick_rep(cands, at_syms);
        return (Tier::GlobalName, Some(at_syms[rep].id.clone()));
    }
    (Tier::Unmatched, None)
}

/// Decide a source file's target mapping: exact normalized-path, else exact
/// subtree (target has the module as a directory), else distinctive-leaf-weighted
/// clustering onto the deepest subtree retaining ≥ `retain` of the vote mass.
#[allow(clippy::too_many_arguments)]
fn map_source_file(
    file: &str,
    rules: &NamingRules,
    retain: f64,
    at_modules: &[String],
    module_is_exact: bool,
    at_by_leaf: &HashMap<String, Vec<usize>>,
    at_syms: &[AtSym],
    at_prefixes: &[String],
    leaves: &[String],
) -> FileMap {
    let module = source_file_to_module(file, rules);
    // (1) exact normalized-path match.
    if module_is_exact {
        return FileMap {
            subtree: Some(module),
            method: Some("exact".to_string()),
            votes: None,
        };
    }
    // Renamed base: target has `module` as a directory (subtree) but no bare
    // module symbol.
    if at_modules.iter().any(|m| under_prefix(m, &module)) {
        return FileMap {
            subtree: Some(module),
            method: Some("exact-subtree".to_string()),
            votes: None,
        };
    }
    // (2) graph-assisted clustering.
    if leaves.is_empty() {
        return FileMap {
            subtree: None,
            method: None,
            votes: None,
        };
    }
    // Distinctive-leaf weighting: a leaf in K modules casts 1/K per module. The
    // per-module vote accumulator is insertion-ordered so the downstream stable
    // sort reproduces the prototype's tie-breaks exactly.
    let mut w_votes: Vec<(String, f64)> = Vec::new();
    let mut w_index: HashMap<String, usize> = HashMap::new();
    let mut uniq: BTreeSet<&str> = BTreeSet::new();
    // `new Set(leaves)` iterates in first-insertion order.
    let mut seen_leaf: HashSet<&str> = HashSet::new();
    let mut ordered_leaves: Vec<&str> = Vec::new();
    for ln in leaves {
        if seen_leaf.insert(ln.as_str()) {
            ordered_leaves.push(ln.as_str());
        }
        uniq.insert(ln.as_str());
    }
    for ln in &ordered_leaves {
        let Some(hits) = at_by_leaf.get(*ln) else {
            continue;
        };
        // Distinct modules this leaf lands in, first-occurrence order.
        let mut mods: Vec<&str> = Vec::new();
        let mut mseen: HashSet<&str> = HashSet::new();
        for &i in hits {
            let m = at_syms[i].module.as_str();
            if mseen.insert(m) {
                mods.push(m);
            }
        }
        if mods.is_empty() {
            continue;
        }
        let w = 1.0 / mods.len() as f64;
        for m in mods {
            match w_index.get(m) {
                Some(&idx) => w_votes[idx].1 += w,
                None => {
                    w_index.insert(m.to_string(), w_votes.len());
                    w_votes.push((m.to_string(), w));
                }
            }
        }
    }
    if w_votes.is_empty() {
        return FileMap {
            subtree: None,
            method: None,
            votes: None,
        };
    }
    // Vote mass under each candidate prefix (prefix insertion order).
    let mut max_v = 0.0f64;
    struct Row {
        pref_idx: usize,
        v: f64,
        breadth: usize,
        depth: usize,
    }
    let mut rows: Vec<Row> = Vec::new();
    for (pi, pref) in at_prefixes.iter().enumerate() {
        let mut v = 0.0f64;
        for (m, c) in &w_votes {
            if under_prefix(m, pref) {
                v += *c;
            }
        }
        if v == 0.0 {
            continue;
        }
        let breadth = at_modules.iter().filter(|m| under_prefix(m, pref)).count();
        rows.push(Row {
            pref_idx: pi,
            v,
            breadth,
            depth: pref.split('/').count(),
        });
        if v > max_v {
            max_v = v;
        }
    }
    // Retain ≥ retain * max_v; deepest, then most vote mass, then narrowest.
    let mut ok: Vec<&Row> = rows.iter().filter(|r| r.v >= retain * max_v).collect();
    ok.sort_by(|a, b| {
        b.depth
            .cmp(&a.depth)
            .then_with(|| b.v.partial_cmp(&a.v).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| a.breadth.cmp(&b.breadth))
    });
    match ok.first() {
        Some(best) if best.v >= 0.5 => FileMap {
            subtree: Some(at_prefixes[best.pref_idx].clone()),
            method: Some("graph-cluster".to_string()),
            votes: Some(round2(best.v)),
        },
        _ => FileMap {
            subtree: None,
            method: None,
            votes: None,
        },
    }
}

/// The manifest module shape we read for the DONE oracle.
#[derive(Deserialize)]
struct ManifestModule {
    package: String,
    src: String,
    status: String,
}

#[derive(Deserialize)]
struct Manifest {
    modules: Vec<ManifestModule>,
}

/// Resolve the native (test-verified) source files from the conformance manifest
/// *text*, best-effort. The caller supplies the manifest contents as data
/// (`manifest_json`) — core does not read files, so the functional-core boundary
/// stays clean; only the trusted-pure `serde_json` parse and the pure
/// module-name → source-file mapping happen here. Returns the file set and the
/// native-module count. On any failure (no config, no text, unparsable) the set
/// is empty and a note is recorded.
fn load_native_files(
    conf: &Option<ConformanceConfig>,
    manifest_json: Option<&str>,
    notes: &mut Vec<String>,
) -> (BTreeSet<String>, usize) {
    let Some(conf) = conf else {
        notes.push("no conformance config: no file banded DONE".to_string());
        return (BTreeSet::new(), 0);
    };
    let Some(text) = manifest_json else {
        notes.push(format!(
            "conformance manifest '{}' contents not supplied: no file banded DONE",
            conf.manifest_path
        ));
        return (BTreeSet::new(), 0);
    };
    let manifest: Manifest = match serde_json::from_str(text) {
        Ok(m) => m,
        Err(e) => {
            notes.push(format!(
                "conformance manifest '{}' unparsable ({e}): no file banded DONE",
                conf.manifest_path
            ));
            return (BTreeSet::new(), 0);
        }
    };
    let mut files = BTreeSet::new();
    let mut count = 0usize;
    for m in &manifest.modules {
        if m.package == conf.package && m.status == conf.native_status {
            count += 1;
            let src = m
                .src
                .strip_prefix(&conf.src_prefix_strip)
                .unwrap_or(&m.src)
                .to_string();
            files.insert(src);
        }
    }
    (files, count)
}

#[cfg(test)]
mod tests;
