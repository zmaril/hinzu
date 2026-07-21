//! Structural similarity analysis: find places where several implementations are
//! structurally alike enough that a human or an agent should investigate a shared
//! abstraction. This is the pure engine behind `hinzu similar`.
//!
//! It is **advisory and evidence-based**, in the same spirit as the rest of
//! hinzu. It never performs a refactor and never claims an abstraction is
//! definitely correct. For each cluster of similar code it reports: where the
//! members are, what they *share*, what *differs* (the axes an abstraction would
//! have to range over), the likely abstraction family, a confidence, the
//! per-language capability/limitations that bear on the finding, and explicit
//! reasons **not** to consolidate. Uncertainty is fail-closed: a syntactic-only
//! extractor caps confidence and shows up as limitations and counter-evidence,
//! never as a faked claim.
//!
//! ## The seam, stated honestly
//!
//! The core reads no files. An extractor (the CLI/adapter layer) parses source
//! into [`StructuralSignature`]s — one per function/def, a language-neutral
//! structural fingerprint — and hands them here. [`analyze`] buckets, scores,
//! clusters, and explains, returning a [`SimilarityOutput`]. Everything the
//! analysis can and cannot see is carried in the [`profile::LanguageProfile`]
//! block, so a consumer reads the caveats next to the data. The types are
//! language-neutral by design: a TypeScript extractor drops in by emitting the
//! same signatures and a TS profile, with no change to this engine.
//!
//! ## What it captures, and what it does not
//!
//! Similarity is measured over structure only: a k-gram (shingle) fingerprint of
//! the normalized AST-node-kind sequence, the control-flow skeleton, the
//! statement mix, the ordered call sequence, and the *shape* of the signature
//! types (identifiers erased, so "same shape, different types" is a strong
//! signal). It does **not** understand semantics: two functions can be
//! structurally identical and behave differently, and two behaviourally
//! equivalent functions written in different styles will not match. That is why
//! the output is a set of *candidates to investigate*, not a verdict.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub mod profile;
pub use profile::{profile_for_language, rust_syn_profile, ts_tsc_profile, LanguageProfile};

/// The schema version embedded in every emitted similarity document, so a
/// consumer can branch on shape changes.
pub const HINZU_SIMILARITY_VERSION: u32 = 1;

/// The k in the k-gram shingles an extractor hashes over the AST-node-kind
/// sequence. Fixed here so extractor and engine agree.
pub const SHINGLE_K: usize = 3;

// ---------------------------------------------------------------------------
// The structural signature (extractor → engine).
// ---------------------------------------------------------------------------

/// The arity of a callable: how many parameters, results, and generic
/// parameters it declares. A coarse but language-neutral size/shape signal.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Arity {
    /// Declared parameters (including a receiver like `self`).
    pub params: u32,
    /// Declared results (0 for a unit return, 1 otherwise — a tuple counts once).
    pub results: u32,
    /// Declared generic type parameters.
    pub generics: u32,
}

/// The control-flow skeleton of a body: counts that summarize its branching and
/// looping shape without any of its contents. Two bodies with the same skeleton
/// have the same control-flow structure even if every identifier differs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cfg {
    /// `if` / conditional branch points.
    pub branch_count: u32,
    /// Total `match`/`switch` arms across the body.
    pub match_arms: u32,
    /// Loops (`for`/`while`/`loop`).
    pub loop_count: u32,
    /// `?`/try operators (error-propagation points).
    pub try_count: u32,
    /// Explicit `return` statements.
    pub return_points: u32,
    /// Maximum block-nesting depth.
    pub max_nesting: u32,
}

impl Cfg {
    /// The skeleton as an ordered vector, for distance math.
    fn vector(&self) -> [f64; 6] {
        [
            self.branch_count as f64,
            self.match_arms as f64,
            self.loop_count as f64,
            self.try_count as f64,
            self.return_points as f64,
            self.max_nesting as f64,
        ]
    }
}

/// The structural shape of a signature's types, with identifiers erased. A
/// nominal leaf type erases to `_`; constructors are kept (`Result<_,_>`,
/// `Vec<_>`, `&_`), so two signatures with the same shape but different concrete
/// types match — the strong "same shape, different types" signal.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeShape {
    /// The erased parameter type shapes, in order.
    pub params: Vec<String>,
    /// The erased result type shape (`"_"` for unit).
    pub result: String,
}

/// One function/def's language-neutral structural fingerprint. Produced by an
/// extractor, consumed by [`analyze`]. Every field is structure only — no
/// identifiers, no literals, no semantics — so it is comparable across bodies
/// and (by design) across languages.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StructuralSignature {
    /// A stable id. For Rust, the file-and-item-qualified path.
    pub symbol_id: String,
    /// The human name.
    pub display: String,
    /// The source language (`"rust"`, `"typescript"`, …).
    pub language: String,
    /// The def kind (`"function"`, `"impl_method"`, `"trait_method"`,
    /// `"closure"`, …).
    pub kind: String,
    /// The defining file.
    pub file: String,
    /// First source line.
    pub line_start: u32,
    /// Last source line.
    pub line_end: u32,
    /// Parameter/result/generic arity.
    pub arity: Arity,
    /// The control-flow skeleton.
    pub cfg: Cfg,
    /// Node-kind counts (`let`, `call`, `if`, `match`, `loop`, `return`,
    /// `assign`, `macro`, `await`, …).
    pub stmt_histogram: BTreeMap<String, u32>,
    /// The ordered, normalized callee simple-names (generics/paths stripped).
    pub call_sequence: Vec<String>,
    /// The structural type shape of the signature.
    pub type_shape: TypeShape,
    /// k-gram (k=[`SHINGLE_K`]) hashes over the normalized AST-node-kind
    /// sequence, for Jaccard / MinHash.
    pub shingles: Vec<u64>,
    /// The normalized size (node-kind sequence length), for length filtering.
    pub token_len: u32,
    /// Optional language-specific extras (`has_macro`, `is_async`, …).
    #[serde(default)]
    pub features: BTreeMap<String, String>,
}

impl StructuralSignature {
    /// The distinct shingle set (Jaccard/MinHash treat shingles as a set).
    fn shingle_set(&self) -> BTreeSet<u64> {
        self.shingles.iter().copied().collect()
    }

    /// The number of statement-ish nodes (histogram total) — the min-statements
    /// gate reads this.
    fn stmt_total(&self) -> u32 {
        self.stmt_histogram.values().copied().sum()
    }

    /// Whether a feature flag is set to `"true"`.
    fn feature_true(&self, key: &str) -> bool {
        self.features.get(key).map(String::as_str) == Some("true")
    }
}

/// The document an extractor emits: a language/extractor stamp plus the
/// signatures. `hinzu similar --structural` reads exactly this shape.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignatureDoc {
    /// The language these signatures are for.
    pub language: String,
    /// The extractor that produced them.
    pub extractor: String,
    /// The signatures.
    pub signatures: Vec<StructuralSignature>,
}

// ---------------------------------------------------------------------------
// The finding (engine output).
// ---------------------------------------------------------------------------

/// One member of a candidate cluster: where a similar implementation lives.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Member {
    /// The member's stable symbol id.
    pub symbol_id: String,
    /// The human name.
    pub display: String,
    /// The source language.
    pub language: String,
    /// The defining file.
    pub file: String,
    /// First source line.
    pub line_start: u32,
    /// Last source line.
    pub line_end: u32,
}

/// The shared structural pattern of a cluster: a human summary, the concrete
/// features the members share, the aggregate similarity, and its breakdown.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pattern {
    /// A one-line human summary of what the members share.
    pub summary: String,
    /// The concrete features that are ~identical across members.
    pub shared_features: Vec<String>,
    /// The aggregate similarity, 0..1.
    pub similarity: f64,
    /// The per-signal similarity breakdown (`shingle_jaccard`, `cfg`,
    /// `type_shape`, `call_seq`, `histogram`).
    pub similarity_breakdown: BTreeMap<String, f64>,
}

/// The abstraction a cluster likely wants, named honestly with its rationale and
/// the language mechanisms that could express it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LikelyAbstraction {
    /// The abstraction family (from the profile's `abstraction_families`).
    pub family: String,
    /// Why this family, in prose.
    pub rationale: String,
    /// The concrete language mechanisms that could express it.
    pub language_mechanisms: Vec<String>,
}

/// The subset of a profile's capabilities and limitations that bear on a
/// specific finding — the fidelity block, scoped to this candidate.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FindingProfile {
    /// The capability grades that shaped this finding (e.g.
    /// `"types_resolved=syntactic"`).
    pub capabilities_used: Vec<String>,
    /// The limitations that bear on this finding.
    pub limitations: Vec<String>,
}

/// A candidate cluster: a place worth investigating for a shared abstraction.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Finding {
    /// The candidate id (`"cand-1"`, …).
    pub id: String,
    /// The cluster members (always >= 2).
    pub members: Vec<Member>,
    /// The shared structural pattern.
    pub pattern: Pattern,
    /// What *varies* across members — the axes an abstraction must range over.
    pub differences: Vec<String>,
    /// The likely abstraction family, with rationale.
    pub likely_abstraction: LikelyAbstraction,
    /// The confidence, 0..1 — bounded by how resolved the inputs are.
    pub confidence: f64,
    /// One line explaining how the confidence was arrived at.
    pub confidence_basis: String,
    /// Reasons **not** to consolidate — the honest counter-case.
    pub counter_evidence: Vec<String>,
    /// The capability/limitation block relevant to this finding.
    pub profile: FindingProfile,
}

// ---------------------------------------------------------------------------
// The output document.
// ---------------------------------------------------------------------------

/// The parameters a run was executed with, echoed into the output.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SimilarityParams {
    /// The clustering threshold: a pair at or above this similarity is an edge.
    pub min_similarity: f64,
    /// The minimum mean pairwise similarity a cluster must reach to be reported
    /// (the cohesion gate). A loose, transitively-chained cluster below this is
    /// split at this higher bar or rejected — never emitted as a mega-blob.
    pub min_cohesion: f64,
    /// The minimum normalized size (`token_len`) a signature must have to be
    /// considered — trivial defs are filtered out.
    pub min_size: u32,
    /// The minimum statement count a signature must have to be considered.
    pub min_statements: u32,
    /// The language filter applied, if any.
    pub language_filter: Option<String>,
}

impl Default for SimilarityParams {
    fn default() -> Self {
        SimilarityParams {
            min_similarity: 0.55,
            min_cohesion: 0.6,
            min_size: 12,
            min_statements: 2,
            language_filter: None,
        }
    }
}

/// The knobs [`analyze`] takes. Kept separate from the echoed
/// [`SimilarityParams`] so a caller constructs it directly.
#[derive(Clone, Debug)]
pub struct AnalyzeParams {
    /// The clustering threshold (default 0.55).
    pub min_similarity: f64,
    /// The cohesion gate: the minimum mean pairwise similarity a reported cluster
    /// must reach (default 0.6). Loose clusters below it are split at this higher
    /// bar or rejected.
    pub min_cohesion: f64,
    /// The minimum normalized size gate (default 12).
    pub min_size: u32,
    /// The minimum statement count gate (default 2).
    pub min_statements: u32,
    /// Only analyze signatures in this language, if set.
    pub language_filter: Option<String>,
}

impl Default for AnalyzeParams {
    fn default() -> Self {
        let p = SimilarityParams::default();
        AnalyzeParams {
            min_similarity: p.min_similarity,
            min_cohesion: p.min_cohesion,
            min_size: p.min_size,
            min_statements: p.min_statements,
            language_filter: p.language_filter,
        }
    }
}

/// Aggregate counts for a run, reported honestly (including how many pairs were
/// actually scored — the bucketing/LSH cost, not a hidden O(N²)).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SimilarityStats {
    /// Signatures handed to the engine.
    pub signatures_analyzed: usize,
    /// Signatures that passed the trivial-def filter.
    pub signatures_after_filter: usize,
    /// Distinct candidate pairs actually scored.
    pub pairs_compared: usize,
    /// Pairs that scored at or above `min_similarity`.
    pub pairs_over_threshold: usize,
    /// Loose clusters dropped by the cohesion gate: a transitively-chained
    /// component whose mean pairwise similarity stayed below `min_cohesion` and
    /// that could not be split into a tighter sub-cluster. Reported honestly
    /// rather than emitted as a low-cohesion mega-blob.
    pub clusters_rejected_low_cohesion: usize,
    /// Clusters of >= 2 reported.
    pub candidates_found: usize,
}

/// The complete similarity document, ready to serialize as JSON. Mirrors the
/// `graph` convention: version, root, languages, a fidelity/capability block
/// (the profiles), the params, the stats, and the candidates.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SimilarityOutput {
    /// The schema version ([`HINZU_SIMILARITY_VERSION`]).
    pub hinzu_similarity_version: u32,
    /// The analyzed target (a label — usually the project path).
    pub root: String,
    /// The languages present in the analyzed signatures.
    pub languages: Vec<String>,
    /// The capability/limitation blocks — one per language present that has a
    /// shipped profile. The fidelity block for this analysis.
    pub profiles: Vec<LanguageProfile>,
    /// The parameters this run used.
    pub params: SimilarityParams,
    /// Aggregate counts.
    pub stats: SimilarityStats,
    /// The candidate clusters, sorted by confidence descending.
    pub candidates: Vec<Finding>,
}

// ---------------------------------------------------------------------------
// The analysis.
// ---------------------------------------------------------------------------

/// Analyze a set of structural signatures for clusters worth investigating.
///
/// Pure: it reads no files and has no effects. `root` is a free-form label for
/// the analyzed target (usually the project path). The pipeline is:
/// 1. filter out trivial defs (`token_len < min_size`, or too few statements);
/// 2. generate candidate pairs by coarse bucketing **and** a MinHash/LSH pass
///    over the shingles (so cross-bucket structural matches are caught), scoring
///    each distinct pair once (`pairs_compared` counts them honestly);
/// 3. union-find over the pairs at or above `min_similarity` into clusters;
/// 4. explain each cluster of >= 2: shared features, differences, likely
///    abstraction, confidence (capped by the profile's resolution), and
///    counter-evidence.
pub fn analyze(
    root: &str,
    signatures: Vec<StructuralSignature>,
    params: &AnalyzeParams,
) -> SimilarityOutput {
    let signatures_analyzed = signatures.len();

    // Language filter (honest: a filter that matches nothing yields no findings,
    // never a faked result).
    let signatures: Vec<StructuralSignature> = match &params.language_filter {
        Some(lang) => signatures
            .into_iter()
            .filter(|s| &s.language == lang)
            .collect(),
        None => signatures,
    };

    // Languages present + their shipped profiles (the fidelity block).
    let mut languages: Vec<String> = signatures.iter().map(|s| s.language.clone()).collect();
    languages.sort();
    languages.dedup();
    let profiles: Vec<LanguageProfile> = languages
        .iter()
        .filter_map(|l| profile_for_language(l))
        .collect();

    // Step 1: filter trivial defs.
    let kept: Vec<StructuralSignature> = signatures
        .into_iter()
        .filter(|s| s.token_len >= params.min_size && s.stmt_total() >= params.min_statements)
        .collect();
    let signatures_after_filter = kept.len();

    // Step 2: candidate pairs (bucketing + LSH), scored once each.
    let candidate_pairs = candidate_pairs(&kept);
    let pairs_compared = candidate_pairs.len();

    let mut scored: Vec<(usize, usize, Score)> = Vec::new();
    for &(i, j) in &candidate_pairs {
        let score = score_pair(&kept[i], &kept[j]);
        scored.push((i, j, score));
    }
    let over: Vec<&(usize, usize, Score)> = scored
        .iter()
        .filter(|(_, _, s)| s.aggregate >= params.min_similarity)
        .collect();
    let pairs_over_threshold = over.len();

    // Step 3: union-find into primary components over the over-threshold edges.
    let mut uf = UnionFind::new(kept.len());
    for (i, j, _) in &over {
        uf.union(*i, *j);
    }

    // A quick lookup of pairwise scores for cluster-level aggregation.
    let mut pair_score: BTreeMap<(usize, usize), Score> = BTreeMap::new();
    for (i, j, s) in &scored {
        pair_score.insert((*i, *j), s.clone());
    }

    // Group the primary components and the aggregate edges internal to each,
    // keyed by the component root. The cohesion metric divides the edge-weight
    // sum by *all* possible member pairs, so a transitively-chained component
    // (few strong edges spread over many members) reads as low-cohesion even when
    // every edge it does have is strong — which is exactly the mega-blob shape.
    let mut components: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for idx in 0..kept.len() {
        components.entry(uf.find(idx)).or_default().push(idx);
    }
    let mut component_edges: BTreeMap<usize, Vec<(usize, usize, f64)>> = BTreeMap::new();
    for (i, j, s) in &over {
        let root = uf.find(*i);
        component_edges
            .entry(root)
            .or_default()
            .push((*i, *j, s.aggregate));
    }

    // Step 4: refine each primary component into cohesive clusters. A dense
    // component is emitted whole; a loose one is split at the higher
    // `min_cohesion` bar into tighter sub-clusters; a loose component with no weak
    // link to cut is rejected honestly (counted, never silently trimmed).
    let mut final_clusters: Vec<Vec<usize>> = Vec::new();
    let mut clusters_rejected_low_cohesion = 0usize;
    for (root, members) in &components {
        if members.len() < 2 {
            continue;
        }
        let edges = component_edges.get(root).cloned().unwrap_or_default();
        refine_cohesive(
            members,
            &edges,
            params.min_cohesion,
            &mut final_clusters,
            &mut clusters_rejected_low_cohesion,
        );
    }

    // Step 5: explain each cohesive cluster of >= 2.
    let mut findings: Vec<Finding> = Vec::new();
    for members in &final_clusters {
        if members.len() < 2 {
            continue;
        }
        if let Some(finding) = explain_cluster(members, &kept, &pair_score, &profiles) {
            findings.push(finding);
        }
    }

    // Sort by confidence desc, then by member count desc, then by id for
    // determinism, and mint stable ids.
    sort_and_number_findings(&mut findings);

    SimilarityOutput {
        hinzu_similarity_version: HINZU_SIMILARITY_VERSION,
        root: root.to_string(),
        languages,
        profiles,
        params: SimilarityParams {
            min_similarity: params.min_similarity,
            min_cohesion: params.min_cohesion,
            min_size: params.min_size,
            min_statements: params.min_statements,
            language_filter: params.language_filter.clone(),
        },
        stats: SimilarityStats {
            signatures_analyzed,
            signatures_after_filter,
            pairs_compared,
            pairs_over_threshold,
            clusters_rejected_low_cohesion,
            candidates_found: findings.len(),
        },
        candidates: findings,
    }
}

/// Order findings deterministically and mint their stable ids: confidence
/// descending, then member count descending, then the first member's symbol id,
/// numbering the result `cand-1`, `cand-2`, … in place. Shared by [`analyze`] and
/// the CLI's multi-language merge so a single-language run and a merged run order
/// and number candidates identically.
pub fn sort_and_number_findings(findings: &mut [Finding]) {
    findings.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.members.len().cmp(&a.members.len()))
            .then(a.members[0].symbol_id.cmp(&b.members[0].symbol_id))
    });
    for (n, f) in findings.iter_mut().enumerate() {
        f.id = format!("cand-{}", n + 1);
    }
}

// ---------------------------------------------------------------------------
// Candidate-pair generation: coarse bucketing + MinHash/LSH.
// ---------------------------------------------------------------------------

/// The number of MinHash functions used for the LSH pass.
const MINHASH_K: usize = 32;
/// The number of LSH bands (`MINHASH_K` must be divisible by this). More, wider
/// bands catch looser matches; `8 x 4` is a middle ground.
const LSH_BANDS: usize = 8;

/// Generate the distinct candidate pairs to score: the union of coarse-bucket
/// pairs and MinHash/LSH pairs. A pair is `(i, j)` with `i < j`. This keeps the
/// comparison off the full O(N^2) surface while still catching cross-bucket
/// structural matches, and the returned count is what `pairs_compared` reports.
fn candidate_pairs(sigs: &[StructuralSignature]) -> Vec<(usize, usize)> {
    let mut pairs: BTreeSet<(usize, usize)> = BTreeSet::new();

    // Coarse buckets: signatures that share a coarse key are compared. The key is
    // deliberately loose (param band, cfg-shape band, size band) so genuinely
    // similar code lands together without exploding the buckets.
    let mut buckets: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (idx, s) in sigs.iter().enumerate() {
        buckets.entry(coarse_key(s)).or_default().push(idx);
    }
    for members in buckets.values() {
        add_all_pairs(members, &mut pairs);
    }

    // MinHash/LSH: signatures sharing any band-bucket are compared. This catches
    // structurally similar bodies that landed in different coarse buckets.
    let minhashes: Vec<Option<[u64; MINHASH_K]>> = sigs.iter().map(minhash).collect();
    let rows = MINHASH_K / LSH_BANDS;
    for band in 0..LSH_BANDS {
        let mut band_buckets: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
        for (idx, mh) in minhashes.iter().enumerate() {
            let Some(mh) = mh else { continue };
            let start = band * rows;
            let key = fnv1a64_words(&mh[start..start + rows]);
            band_buckets.entry(key).or_default().push(idx);
        }
        for members in band_buckets.values() {
            add_all_pairs(members, &mut pairs);
        }
    }

    pairs.into_iter().collect()
}

/// Add every `i < j` pair among `members` to `pairs`.
fn add_all_pairs(members: &[usize], pairs: &mut BTreeSet<(usize, usize)>) {
    for a in 0..members.len() {
        for b in (a + 1)..members.len() {
            let (i, j) = (members[a], members[b]);
            pairs.insert(if i < j { (i, j) } else { (j, i) });
        }
    }
}

/// The coarse bucket key: a loose banding of param arity, control-flow shape, and
/// size, so similar signatures collide without over-splitting.
fn coarse_key(s: &StructuralSignature) -> String {
    let param_band = band(s.arity.params, 2);
    let branch_band = band(s.cfg.branch_count + s.cfg.match_arms, 3);
    let loop_band = s.cfg.loop_count.min(3);
    let size_band = band(s.token_len, 20);
    format!("{param_band}:{branch_band}:{loop_band}:{size_band}")
}

/// Band a count into buckets of width `width`.
fn band(value: u32, width: u32) -> u32 {
    value / width.max(1)
}

/// The MinHash signature over a signature's shingle set, or `None` when it has no
/// shingles. Deterministic: seeded xorshift mixes each shingle per hash function.
fn minhash(s: &StructuralSignature) -> Option<[u64; MINHASH_K]> {
    let shingles = s.shingle_set();
    if shingles.is_empty() {
        return None;
    }
    let mut mh = [u64::MAX; MINHASH_K];
    for &sh in &shingles {
        for (k, slot) in mh.iter_mut().enumerate() {
            let h = mix64(sh ^ SEEDS[k]);
            if h < *slot {
                *slot = h;
            }
        }
    }
    Some(mh)
}

/// Fixed per-hash seeds for MinHash, derived from a splitmix walk so they are
/// well-spread and deterministic across runs.
static SEEDS: [u64; MINHASH_K] = {
    let mut seeds = [0u64; MINHASH_K];
    let mut state: u64 = 0x9E3779B97F4A7C15;
    let mut i = 0;
    while i < MINHASH_K {
        // splitmix64 step.
        state = state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^= z >> 31;
        seeds[i] = z;
        i += 1;
    }
    seeds
};

/// A 64-bit avalanche mix (splitmix64 finalizer), for MinHash slot hashing.
fn mix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E3779B97F4A7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

/// FNV-1a over a word slice, for hashing an LSH band into a bucket.
fn fnv1a64_words(words: &[u64]) -> u64 {
    let mut hash: u64 = 0xCBF29CE484222325;
    for &w in words {
        for b in w.to_le_bytes() {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001B3);
        }
    }
    hash
}

// ---------------------------------------------------------------------------
// Pairwise scoring.
// ---------------------------------------------------------------------------

/// A pairwise similarity score with its per-signal breakdown.
#[derive(Clone, Debug)]
struct Score {
    aggregate: f64,
    shingle_jaccard: f64,
    cfg: f64,
    type_shape: f64,
    call_seq: f64,
    histogram: f64,
}

/// The signal weights in the aggregate score. Shingles are the primary signal;
/// type-shape is weighted highly because "same shape, different types" is the
/// strongest generic-abstraction cue. They sum to 1.
const W_SHINGLE: f64 = 0.40;
const W_TYPE: f64 = 0.20;
const W_CALL: f64 = 0.15;
const W_CFG: f64 = 0.15;
const W_HIST: f64 = 0.10;

/// Score a pair of signatures: a weighted combination of shingle Jaccard,
/// control-flow-skeleton closeness, type-shape structural match, ordered
/// call-sequence overlap, and statement-histogram cosine. Every signal is
/// exposed in the breakdown.
fn score_pair(a: &StructuralSignature, b: &StructuralSignature) -> Score {
    let shingle_jaccard = jaccard(&a.shingle_set(), &b.shingle_set());
    let cfg = cfg_similarity(&a.cfg, &b.cfg);
    let type_shape = type_shape_similarity(&a.type_shape, &b.type_shape);
    let call_seq = call_sequence_similarity(&a.call_sequence, &b.call_sequence);
    let histogram = histogram_cosine(&a.stmt_histogram, &b.stmt_histogram);
    let aggregate = W_SHINGLE * shingle_jaccard
        + W_TYPE * type_shape
        + W_CALL * call_seq
        + W_CFG * cfg
        + W_HIST * histogram;
    Score {
        aggregate,
        shingle_jaccard,
        cfg,
        type_shape,
        call_seq,
        histogram,
    }
}

/// Jaccard similarity of two sets: `|A n B| / |A u B|` (1.0 for two empty sets).
fn jaccard(a: &BTreeSet<u64>, b: &BTreeSet<u64>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Control-flow-skeleton similarity: `1 - normalized L1 distance` over the six
/// cfg counts, so identical skeletons score 1 and wildly different ones score
/// near 0.
fn cfg_similarity(a: &Cfg, b: &Cfg) -> f64 {
    let (va, vb) = (a.vector(), b.vector());
    let mut num = 0.0;
    let mut den = 0.0;
    for k in 0..va.len() {
        num += (va[k] - vb[k]).abs();
        den += va[k].max(vb[k]);
    }
    if den == 0.0 {
        1.0
    } else {
        1.0 - (num / den)
    }
}

/// Type-shape structural similarity: the fraction of positionally-matching
/// parameter shapes plus the result-shape match, averaged. Identifiers are
/// already erased in a [`TypeShape`], so this is 1.0 exactly when two signatures
/// have the same shape regardless of concrete types.
fn type_shape_similarity(a: &TypeShape, b: &TypeShape) -> f64 {
    let max_params = a.params.len().max(b.params.len());
    let param_score = if max_params == 0 {
        1.0
    } else {
        let matches = a
            .params
            .iter()
            .zip(b.params.iter())
            .filter(|(x, y)| x == y)
            .count();
        matches as f64 / max_params as f64
    };
    let result_score = if a.result == b.result { 1.0 } else { 0.0 };
    // Weight params and result together; a nullary function is all-result.
    0.7 * param_score + 0.3 * result_score
}

/// Ordered call-sequence similarity via the longest common subsequence:
/// `2*LCS / (|A| + |B|)`. Ordered so a reordered call list scores lower than an
/// identical one (1.0 for two empty sequences).
fn call_sequence_similarity(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let lcs = lcs_len(a, b);
    (2.0 * lcs as f64) / (a.len() + b.len()) as f64
}

/// Longest-common-subsequence length over two string sequences (classic DP).
fn lcs_len(a: &[String], b: &[String]) -> usize {
    let mut prev = vec![0usize; b.len() + 1];
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            cur[j] = if a[i - 1] == b[j - 1] {
                prev[j - 1] + 1
            } else {
                prev[j].max(cur[j - 1])
            };
        }
        std::mem::swap(&mut prev, &mut cur);
        for v in cur.iter_mut() {
            *v = 0;
        }
    }
    prev[b.len()]
}

/// Cosine similarity of two statement histograms over their shared key space.
fn histogram_cosine(a: &BTreeMap<String, u32>, b: &BTreeMap<String, u32>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    let mut dot = 0.0;
    let mut na = 0.0;
    let mut nb = 0.0;
    for k in keys {
        let x = *a.get(k).unwrap_or(&0) as f64;
        let y = *b.get(k).unwrap_or(&0) as f64;
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

// ---------------------------------------------------------------------------
// Union-find.
// ---------------------------------------------------------------------------

/// A disjoint-set forest with path compression + union by size, for clustering
/// the over-threshold pairs.
struct UnionFind {
    parent: Vec<usize>,
    size: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        UnionFind {
            parent: (0..n).collect(),
            size: vec![1; n],
        }
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }

    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return;
        }
        let (big, small) = if self.size[ra] >= self.size[rb] {
            (ra, rb)
        } else {
            (rb, ra)
        };
        self.parent[small] = big;
        self.size[big] += self.size[small];
    }
}

// ---------------------------------------------------------------------------
// Cohesion refinement: keep dense clusters, split or reject loose ones.
// ---------------------------------------------------------------------------

/// Refine a primary component into cohesive clusters. A component whose mean
/// pairwise similarity meets `min_cohesion` is emitted whole; a looser one is
/// split by keeping only its strong (`>= min_cohesion`) edges and recursing on
/// each resulting sub-component; a loose component with no weak link to cut is
/// rejected honestly (counted in `rejected`, never silently trimmed to a smaller
/// list). This is what stops a transitively-chained mega-blob — many members
/// linked by a sparse web of just-over-threshold edges — from being reported as a
/// single junk cluster.
///
/// `edges` are the over-threshold aggregate similarities *internal* to `members`;
/// a member pair absent from `edges` contributes 0 to cohesion, so a sparse
/// component is correctly seen as loose.
fn refine_cohesive(
    members: &[usize],
    edges: &[(usize, usize, f64)],
    min_cohesion: f64,
    out: &mut Vec<Vec<usize>>,
    rejected: &mut usize,
) {
    if members.len() < 2 {
        return;
    }
    if cluster_cohesion(members.len(), edges) >= min_cohesion {
        out.push(members.to_vec());
        return;
    }
    // Loose: cut the weak links by re-clustering at the higher cohesion bar.
    let subs = split_by_threshold(members, edges, min_cohesion);
    if subs.len() <= 1 {
        // The higher bar separated nothing — a solid low-cohesion blob. Reject it
        // rather than emit it or trim it silently. (Also guarantees progress, so
        // the recursion terminates.)
        *rejected += 1;
        return;
    }
    let mut progressed = false;
    for sub in subs {
        if sub.len() < 2 {
            continue; // a peeled-off singleton is simply not a candidate
        }
        progressed = true;
        let sub_set: BTreeSet<usize> = sub.iter().copied().collect();
        let sub_edges: Vec<(usize, usize, f64)> = edges
            .iter()
            .filter(|(i, j, _)| sub_set.contains(i) && sub_set.contains(j))
            .copied()
            .collect();
        refine_cohesive(&sub, &sub_edges, min_cohesion, out, rejected);
    }
    if !progressed {
        // The component dissolved entirely into singletons at the higher bar — no
        // dense sub-cluster survived, so report it rejected rather than dropped.
        *rejected += 1;
    }
}

/// The cohesion of a cluster: the sum of its edge similarities divided by the
/// number of *all* possible member pairs. A clique of near-duplicates scores near
/// 1; a sparse chain of the same member count scores near 0.
fn cluster_cohesion(n: usize, edges: &[(usize, usize, f64)]) -> f64 {
    if n < 2 {
        return 1.0;
    }
    let possible = (n * (n - 1) / 2) as f64;
    let sum: f64 = edges.iter().map(|(_, _, s)| *s).sum();
    sum / possible
}

/// Split a member set into connected sub-components, keeping only edges at or
/// above `threshold` — a local union-find over the member indices.
fn split_by_threshold(
    members: &[usize],
    edges: &[(usize, usize, f64)],
    threshold: f64,
) -> Vec<Vec<usize>> {
    let index: BTreeMap<usize, usize> = members.iter().enumerate().map(|(k, &m)| (m, k)).collect();
    let mut uf = UnionFind::new(members.len());
    for (i, j, s) in edges {
        if *s >= threshold {
            if let (Some(&a), Some(&b)) = (index.get(i), index.get(j)) {
                uf.union(a, b);
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (k, &m) in members.iter().enumerate() {
        groups.entry(uf.find(k)).or_default().push(m);
    }
    groups.into_values().collect()
}

// ---------------------------------------------------------------------------
// Cluster explanation — see explain.rs.
// ---------------------------------------------------------------------------

/// The confidence ceiling for a syntactic-only profile: a syntactic extractor
/// can never be fully certain two signatures share types or behaviour, so
/// confidence is capped here regardless of how high the structural similarity
/// runs. A fully type-resolved profile would lift this. Lives here (not in
/// `explain.rs`) so both the explanation layer and the tests reach it.
const SYNTACTIC_CONFIDENCE_CAP: f64 = 0.85;

mod explain;
use explain::explain_cluster;

#[cfg(test)]
mod tests;
