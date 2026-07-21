//! Cluster explanation for `hinzu similar`: turning a cohesive cluster of
//! structural signatures into a human-readable [`Finding`]. Split out of
//! `mod.rs` (the analyzer core) so neither file carries the whole subsystem;
//! everything here reaches the shared types, weights, and `Score` in the parent
//! module. `explain_cluster` is the sole entry point the analyzer calls.

use super::*;
use std::collections::{BTreeMap, BTreeSet};

/// Turn a cluster of member indices into a [`Finding`]: compute the aggregate
/// similarity and breakdown, the shared features, the differences, the likely
/// abstraction, the confidence (capped by profile resolution), and the
/// counter-evidence. Returns `None` only for a degenerate (<2) cluster.
pub(super) fn explain_cluster(
    member_idx: &[usize],
    sigs: &[StructuralSignature],
    pair_score: &BTreeMap<(usize, usize), Score>,
    profiles: &[LanguageProfile],
) -> Option<Finding> {
    if member_idx.len() < 2 {
        return None;
    }
    let members: Vec<&StructuralSignature> = member_idx.iter().map(|&i| &sigs[i]).collect();

    // Aggregate similarity + breakdown: the mean over every member pair that was
    // scored (a pair inside a transitively-linked cluster may not have a direct
    // score; those are simply not averaged in).
    let mut agg = Accum::default();
    for a in 0..member_idx.len() {
        for b in (a + 1)..member_idx.len() {
            let (i, j) = ordered(member_idx[a], member_idx[b]);
            if let Some(s) = pair_score.get(&(i, j)) {
                agg.add(s);
            }
        }
    }
    let (similarity, breakdown) = agg.finish();

    // Feature comparison across the cluster.
    let cmp = ClusterFeatures::of(&members);

    let shared_features = cmp.shared_features(&members);
    let differences = cmp.differences(&members);
    let likely_abstraction = cmp.classify(&members);
    let summary = cmp.summary(&members, &similarity_breakdown_key(&breakdown));

    // Confidence: start from similarity, cap by profile resolution, and dock for
    // the honest doubts (small size, macro opacity, superficiality).
    let types_resolved = profiles.iter().all(|p| p.types_are_resolved());
    let cap = if types_resolved {
        1.0
    } else {
        SYNTACTIC_CONFIDENCE_CAP
    };
    let mut confidence = similarity.min(cap);
    let mut basis_parts: Vec<String> = vec![format!("structural similarity {similarity:.2}")];
    if !types_resolved {
        basis_parts.push(format!("capped at {cap:.2} (syntactic extractor)"));
    }

    let counter_evidence = cmp.counter_evidence(&members, &breakdown, types_resolved);
    // Each counter-evidence class docks confidence a little, fail-closed.
    if cmp.min_token_len < cmp.max_token_len.min(24) {
        confidence *= 0.9;
        basis_parts.push("small members".to_string());
    }
    if cmp.any_macro {
        confidence *= 0.9;
        basis_parts.push("opaque macro bodies".to_string());
    }
    if cmp.file_count > 1 {
        confidence *= 0.95;
        basis_parts.push(format!("spans {} files", cmp.file_count));
    }
    if breakdown.get("call_seq").copied().unwrap_or(0.0) < 0.34
        && breakdown.get("shingle_jaccard").copied().unwrap_or(0.0) > 0.6
    {
        confidence *= 0.9;
        basis_parts.push("shells match but calls diverge".to_string());
    }
    let confidence = (confidence * 100.0).round() / 100.0;

    // Profile capabilities/limitations that bear on this finding.
    let profile = finding_profile(profiles, &cmp);

    Some(Finding {
        id: String::new(), // minted after sorting
        members: members.iter().map(|s| to_member(s)).collect(),
        pattern: Pattern {
            summary,
            shared_features,
            similarity: round2(similarity),
            similarity_breakdown: breakdown
                .iter()
                .map(|(k, v)| (k.clone(), round2(*v)))
                .collect(),
        },
        differences,
        likely_abstraction,
        confidence,
        confidence_basis: basis_parts.join("; "),
        counter_evidence,
        profile,
    })
}

/// Order a pair so the smaller index is first (the key convention).
fn ordered(a: usize, b: usize) -> (usize, usize) {
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Round to two decimals for stable JSON.
fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// A running mean of the per-signal breakdown across a cluster's scored pairs.
#[derive(Default)]
struct Accum {
    n: usize,
    aggregate: f64,
    shingle: f64,
    cfg: f64,
    type_shape: f64,
    call_seq: f64,
    histogram: f64,
}

impl Accum {
    fn add(&mut self, s: &Score) {
        self.n += 1;
        self.aggregate += s.aggregate;
        self.shingle += s.shingle_jaccard;
        self.cfg += s.cfg;
        self.type_shape += s.type_shape;
        self.call_seq += s.call_seq;
        self.histogram += s.histogram;
    }

    fn finish(&self) -> (f64, BTreeMap<String, f64>) {
        let n = self.n.max(1) as f64;
        let mut m = BTreeMap::new();
        m.insert("shingle_jaccard".to_string(), self.shingle / n);
        m.insert("cfg".to_string(), self.cfg / n);
        m.insert("type_shape".to_string(), self.type_shape / n);
        m.insert("call_seq".to_string(), self.call_seq / n);
        m.insert("histogram".to_string(), self.histogram / n);
        (self.aggregate / n, m)
    }
}

/// The dominant signal name in a breakdown, for the summary line.
fn similarity_breakdown_key(breakdown: &BTreeMap<String, f64>) -> String {
    breakdown
        .iter()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(k, _)| k.clone())
        .unwrap_or_default()
}

/// The cross-member feature comparison a cluster's explanation is built from:
/// which structural features are identical across every member and which vary.
struct ClusterFeatures {
    cfg_identical: bool,
    types_identical: bool,
    calls_identical: bool,
    arity_identical: bool,
    /// Call sequences all have the same length (same call-shape) even if the
    /// callees differ — the enum-dispatch / higher-order cue.
    calls_same_shape: bool,
    any_macro: bool,
    file_count: usize,
    min_token_len: u32,
    max_token_len: u32,
    /// One representative cfg (they may not be identical; used for the summary).
    rep_cfg: Cfg,
}

impl ClusterFeatures {
    fn of(members: &[&StructuralSignature]) -> Self {
        let first = members[0];
        let cfg_identical = members.iter().all(|m| m.cfg == first.cfg);
        let types_identical = members.iter().all(|m| m.type_shape == first.type_shape);
        let calls_identical = members
            .iter()
            .all(|m| m.call_sequence == first.call_sequence);
        let arity_identical = members.iter().all(|m| m.arity == first.arity);
        let calls_same_shape = members
            .iter()
            .all(|m| m.call_sequence.len() == first.call_sequence.len());
        let any_macro = members.iter().any(|m| m.feature_true("has_macro"));
        let files: BTreeSet<&str> = members.iter().map(|m| m.file.as_str()).collect();
        let min_token_len = members.iter().map(|m| m.token_len).min().unwrap_or(0);
        let max_token_len = members.iter().map(|m| m.token_len).max().unwrap_or(0);
        ClusterFeatures {
            cfg_identical,
            types_identical,
            calls_identical,
            arity_identical,
            calls_same_shape,
            any_macro,
            file_count: files.len(),
            min_token_len,
            max_token_len,
            rep_cfg: first.cfg.clone(),
        }
    }

    /// The concrete features that are ~identical across every member.
    fn shared_features(&self, members: &[&StructuralSignature]) -> Vec<String> {
        let mut out = Vec::new();
        if self.cfg_identical {
            out.push(format!(
                "identical control-flow skeleton ({} branch(es), {} match arm(s), {} loop(s), {} ?/try, {} return(s), max nesting {})",
                self.rep_cfg.branch_count,
                self.rep_cfg.match_arms,
                self.rep_cfg.loop_count,
                self.rep_cfg.try_count,
                self.rep_cfg.return_points,
                self.rep_cfg.max_nesting,
            ));
        }
        if self.types_identical {
            let ts = &members[0].type_shape;
            out.push(format!(
                "same type shape ({}) -> {}",
                ts.params.join(", "),
                ts.result
            ));
        }
        if self.calls_identical && !members[0].call_sequence.is_empty() {
            out.push(format!(
                "same call sequence [{}]",
                members[0].call_sequence.join(", ")
            ));
        }
        if self.arity_identical {
            let a = &members[0].arity;
            out.push(format!(
                "same arity (params={}, results={}, generics={})",
                a.params, a.results, a.generics
            ));
        }
        if out.is_empty() {
            out.push("closely matching AST-node-kind fingerprint".to_string());
        }
        out
    }

    /// What varies across members — the axes an abstraction must range over.
    fn differences(&self, members: &[&StructuralSignature]) -> Vec<String> {
        let mut out = Vec::new();
        if !self.types_identical {
            let shapes = distinct_type_shapes(members);
            out.push(format!(
                "type shapes vary across members: {}",
                shapes.join("  |  ")
            ));
        }
        if !self.calls_identical {
            if self.calls_same_shape {
                out.push(format!(
                    "same call shape but differing callees in matching positions: {}",
                    positional_call_diff(members)
                ));
            } else {
                out.push(format!(
                    "call sequences vary in length/content: {}",
                    distinct_call_seqs(members).join("  |  ")
                ));
            }
        }
        if !self.arity_identical {
            let arities: BTreeSet<String> = members
                .iter()
                .map(|m| {
                    format!(
                        "p{}/r{}/g{}",
                        m.arity.params, m.arity.results, m.arity.generics
                    )
                })
                .collect();
            out.push(format!(
                "arity varies: {}",
                arities.into_iter().collect::<Vec<_>>().join(", ")
            ));
        }
        if self.min_token_len != self.max_token_len {
            out.push(format!(
                "normalized size ranges {}..{} tokens",
                self.min_token_len, self.max_token_len
            ));
        }
        if out.is_empty() {
            out.push(
                "no structural axis varies materially — the members look near-duplicated"
                    .to_string(),
            );
        }
        out
    }

    /// The likely abstraction family for the cluster, with rationale and the
    /// language mechanisms that could express it.
    ///
    /// The structural reasoning about **what** differs (types vs callees vs
    /// boilerplate) is language-neutral; the family label and mechanisms are then
    /// routed per language through [`family_for`], keyed on the cluster's language
    /// (a v1 candidate is single-language, so `members[0].language` is the
    /// cluster's language). The routing NEVER names a family absent from that
    /// language's profile `abstraction_families` — e.g. a TypeScript boilerplate
    /// cluster is labelled `object_driven_definition`, never Rust's `macro_rules`.
    fn classify(&self, members: &[&StructuralSignature]) -> LikelyAbstraction {
        let n = members.len();
        let language = members[0].language.as_str();
        let case = self.abstraction_case(n);
        let (family, language_mechanisms) = family_for(language, &case, n);
        debug_assert!(
            family_in_profile(language, &family),
            "classify emitted family `{family}`, absent from the `{language}` profile's \
             abstraction_families"
        );
        LikelyAbstraction {
            family,
            rationale: case.rationale(n),
            language_mechanisms,
        }
    }

    /// Which structural case this cluster falls into — **what** varies across its
    /// members, decided language-neutrally. The priority order mirrors the
    /// original classifier: near-duplicate, then types-only, then callees-only,
    /// then boilerplate, then a diffuse fallback.
    fn abstraction_case(&self, n: usize) -> AbstractionCase {
        if self.calls_identical && self.types_identical && self.cfg_identical {
            AbstractionCase::NearDuplicate
        } else if self.calls_identical && self.cfg_identical && !self.types_identical {
            AbstractionCase::TypesVary
        } else if self.cfg_identical
            && self.arity_identical
            && self.calls_same_shape
            && !self.calls_identical
        {
            AbstractionCase::CalleesVary
        } else if n >= 3 && self.cfg_identical {
            AbstractionCase::Boilerplate
        } else {
            AbstractionCase::Diffuse
        }
    }

    /// The one-line summary of the cluster.
    fn summary(&self, members: &[&StructuralSignature], dominant_signal: &str) -> String {
        let n = members.len();
        let kind = plural_kind(members);
        let shell = if self.cfg_identical && self.rep_cfg.match_arms > 0 {
            format!(
                "the same {}-arm match/error-handling shell",
                self.rep_cfg.match_arms
            )
        } else if self.cfg_identical && self.rep_cfg.loop_count > 0 {
            "the same loop-shaped shell".to_string()
        } else if self.types_identical {
            "the same signature shape and structure".to_string()
        } else {
            "a closely matching structure".to_string()
        };
        format!(
            "{n} {kind} with {shell} (dominant signal: {})",
            humanize_signal(dominant_signal)
        )
    }

    /// The honest reasons *not* to consolidate.
    fn counter_evidence(
        &self,
        members: &[&StructuralSignature],
        breakdown: &BTreeMap<String, f64>,
        types_resolved: bool,
    ) -> Vec<String> {
        let mut out = Vec::new();
        if self.min_token_len < 16 {
            out.push(format!(
                "the smallest member is only {} normalized tokens — it may be too small to justify \
                 a shared abstraction",
                self.min_token_len
            ));
        }
        if self.file_count > 1 {
            let files: BTreeSet<&str> = members.iter().map(|m| m.file.as_str()).collect();
            out.push(format!(
                "members span {} files ({}) — consolidating would introduce a cross-module \
                 dependency that may not be worth the coupling",
                self.file_count,
                files.into_iter().collect::<Vec<_>>().join(", ")
            ));
        }
        if !self.types_identical {
            out.push(
                "the signature types differ, so only a generic/trait abstraction would fit — a \
                 plain shared function would not"
                    .to_string(),
            );
        }
        if self.any_macro {
            out.push(
                "one or more bodies contain macro invocations the syntactic extractor cannot see \
                 into — hidden logic may differ between members"
                    .to_string(),
            );
        }
        let call_seq = breakdown.get("call_seq").copied().unwrap_or(0.0);
        let shingle = breakdown.get("shingle_jaccard").copied().unwrap_or(0.0);
        if call_seq < 0.34 && shingle > 0.6 {
            out.push(
                "the structural shells match but the calls the bodies make differ substantially — \
                 the similarity may be superficial (same shape, unrelated work)"
                    .to_string(),
            );
        }
        // The always-true structural caveat, stated as counter-evidence too. Its
        // wording tracks the profile: a syntactic extractor cannot even confirm
        // the types match, while a resolved one can — but structural sameness
        // still never implies behavioural sameness, and two identically-shaped
        // (leaf-erased) type slots may be genuinely different types either way.
        if types_resolved {
            out.push(
                "structural match only: resolved types were compared, but sameness of structure \
                 does not imply sameness of behaviour, and two identically-shaped type slots may \
                 be genuinely different types"
                    .to_string(),
            );
        } else {
            out.push(
                "syntactic match only: sameness of structure does not imply sameness of \
                 behaviour, and two identically-shaped type slots may be different types"
                    .to_string(),
            );
        }
        out
    }
}

/// The structural case a cluster falls into — **what** differs across its
/// members — decided language-neutrally. The label and mechanisms for each case
/// are then routed per language in [`family_for`], so the same structural finding
/// is named in Rust terms for a Rust cluster and TypeScript terms for a TS one.
enum AbstractionCase {
    /// Same calls, same types, same skeleton — the members look near-duplicated.
    NearDuplicate,
    /// Only the signature types vary; calls and skeleton are constant.
    TypesVary,
    /// Same skeleton and same call *shape*, but different callees in matching
    /// positions — each looks like one case of a dispatch.
    CalleesVary,
    /// 3+ bodies sharing one skeleton with the variation confined to a few slots
    /// — a boilerplate skeleton begging to be generated from one source.
    Boilerplate,
    /// Structurally similar, but the variation is not confined to a single clean
    /// axis — the weaker, catch-all case.
    Diffuse,
}

impl AbstractionCase {
    /// The language-neutral rationale for this case — it describes *what* is
    /// shared and *what* varies, without naming a language-specific mechanism
    /// (those live in the routed `language_mechanisms`).
    fn rationale(&self, n: usize) -> String {
        match self {
            AbstractionCase::NearDuplicate => format!(
                "{n} bodies with the same control flow, the same signature shape, and the same \
                 call sequence — they look near-duplicated, so a single shared function would \
                 cover them."
            ),
            AbstractionCase::TypesVary => format!(
                "{n} bodies with the same control flow and the same call sequence, differing only \
                 in the types in their signatures — the classic shape of one abstraction \
                 parameterized over the varying type."
            ),
            AbstractionCase::CalleesVary => format!(
                "{n} bodies with the same skeleton and the same call *shape*, but different callees \
                 in matching positions — each looks like one case of a dispatch, so parameterizing \
                 over the varying operation would unify them."
            ),
            AbstractionCase::Boilerplate => format!(
                "{n} bodies sharing one control-flow skeleton with the varying parts confined to a \
                 few slots — generating the skeleton from a single source is often the \
                 lowest-friction way to remove the repetition."
            ),
            AbstractionCase::Diffuse => format!(
                "{n} structurally similar bodies; the shared part could likely be pulled into a \
                 helper, though the variation is not confined to a single clean axis."
            ),
        }
    }
}

/// Route a structural [`AbstractionCase`] to the abstraction family and the
/// concrete mechanisms appropriate for `language`. This is the language-aware
/// table: the same structural case yields a Rust family for a Rust cluster and a
/// TypeScript family for a TS cluster.
///
/// It **guarantees** it never returns a family absent from the language's profile
/// `abstraction_families`: any language/case that would fall through, plus any
/// unshipped language, resolves to `helper_function` (which every profile ships).
fn family_for(language: &str, case: &AbstractionCase, n: usize) -> (String, Vec<String>) {
    let (family, mechanisms): (&str, Vec<&str>) = match (language, case) {
        // Near-duplicate: a shared helper, in every language.
        ("rust", AbstractionCase::NearDuplicate) => {
            let mut m = vec!["extract a shared helper function"];
            if n >= 3 {
                m.push("a `macro_rules!` if the repetition is item-level boilerplate");
            }
            ("helper_function", m)
        }
        ("typescript", AbstractionCase::NearDuplicate) => {
            let mut m = vec!["extract a shared function"];
            if n >= 3 {
                m.push("a data-driven table if the repetition is declaration boilerplate");
            }
            ("helper_function", m)
        }

        // Types vary → a generic (both languages), plus the language's type-level
        // knob (a trait bound in Rust, a mapped type in TS).
        ("rust", AbstractionCase::TypesVary) => (
            "generic_function",
            vec![
                "a generic function `fn f<T>(...)`",
                "a trait bound expressing the operation over the varying type",
            ],
        ),
        ("typescript", AbstractionCase::TypesVary) => (
            "generic_function",
            vec![
                "a generic function `function f<T>(...)`",
                "a mapped type where the variation ranges over a key set",
            ],
        ),

        // Callees vary in matching slots → dispatch. Rust reaches for a
        // trait/enum; TS for a higher-order function or a dispatch table.
        ("rust", AbstractionCase::CalleesVary) => (
            "enum_dispatch",
            vec![
                "a trait method with one impl per case",
                "an enum plus a `match` that dispatches",
                "a higher-order function taking the varying operation as a parameter",
            ],
        ),
        ("typescript", AbstractionCase::CalleesVary) => (
            "higher_order_function",
            vec![
                "a higher-order function taking the varying operation as a callback parameter",
                "an object/dispatch table keyed on the varying case",
            ],
        ),

        // 3+ boilerplate bodies → Rust generates with a macro; TS has no macros,
        // so it reaches for a data-driven definition or codegen (NEVER macros).
        ("rust", AbstractionCase::Boilerplate) => (
            "macro_rules",
            vec![
                "a `macro_rules!` generating the repeated item",
                "a generic function if the variation is only in types",
            ],
        ),
        ("typescript", AbstractionCase::Boilerplate) => (
            "object_driven_definition",
            vec![
                "a data-driven definition table the code iterates over",
                "code generation (a generated declaration) for the mechanical boilerplate",
            ],
        ),

        // Diffuse, or any unshipped language: a shared helper — listed by every
        // shipped profile and a safe honest default otherwise.
        _ => (
            "helper_function",
            vec!["a shared helper function for the common part"],
        ),
    };
    let mechanisms: Vec<String> = mechanisms.into_iter().map(String::from).collect();
    // Belt-and-braces: never emit a family the language's profile does not list.
    if family_in_profile(language, family) {
        (family.to_string(), mechanisms)
    } else {
        (
            "helper_function".to_string(),
            vec!["a shared helper function for the common part".to_string()],
        )
    }
}

/// Whether `family` is one the `language`'s shipped profile is willing to name.
/// A language with no shipped profile has no families to violate, so this is
/// vacuously `true` for it (the finding carries no profile block in that case).
fn family_in_profile(language: &str, family: &str) -> bool {
    match profile_for_language(language) {
        Some(p) => p.abstraction_families.iter().any(|f| f == family),
        None => true,
    }
}

/// Assemble the per-finding profile block: the capabilities that shaped the
/// finding and the limitations that bear on it, drawn from the shipped profiles.
fn finding_profile(profiles: &[LanguageProfile], cmp: &ClusterFeatures) -> FindingProfile {
    let mut capabilities_used: Vec<String> = Vec::new();
    let mut limitations: Vec<String> = Vec::new();
    let keys = [
        "types_resolved",
        "control_flow_available",
        "generics_visible",
        "call_targets_known",
        "macro_expansion_visible",
    ];
    for p in profiles {
        for k in keys {
            let v = p.capability(k);
            if v != "unknown" {
                capabilities_used.push(format!("{k}={v}"));
            }
        }
        // Always cite the type-comparison caveat when types matter to the finding.
        for (i, lim) in p.limitations.iter().enumerate() {
            let touches_types = lim.contains("type") && !cmp.types_identical;
            let touches_macro = lim.contains("Macro") && cmp.any_macro;
            let touches_calls = lim.contains("Call") && !cmp.calls_identical;
            // Each profile's first limitation is its umbrella caveat (the
            // syntactic-only caveat for Rust, the structural-typing caveat for
            // TypeScript) and always applies.
            let is_umbrella = i == 0;
            if is_umbrella || touches_types || touches_macro || touches_calls {
                limitations.push(lim.clone());
            }
        }
    }
    capabilities_used.sort();
    capabilities_used.dedup();
    limitations.dedup();
    FindingProfile {
        capabilities_used,
        limitations,
    }
}

/// The pluralized def kind for a summary, e.g. `"functions"` / `"methods"`.
fn plural_kind(members: &[&StructuralSignature]) -> String {
    let kinds: BTreeSet<&str> = members.iter().map(|m| m.kind.as_str()).collect();
    let base = if kinds.len() == 1 {
        match *kinds.iter().next().unwrap() {
            "impl_method" | "trait_method" | "method" => "method",
            "closure" => "closure",
            _ => "function",
        }
    } else {
        "callable"
    };
    format!("{base}s")
}

/// A reader-friendly name for a breakdown signal.
fn humanize_signal(signal: &str) -> &str {
    match signal {
        "shingle_jaccard" => "AST-fingerprint overlap",
        "cfg" => "control-flow skeleton",
        "type_shape" => "signature shape",
        "call_seq" => "call sequence",
        "histogram" => "statement mix",
        other => other,
    }
}

/// The distinct erased type shapes across a cluster, as `"(p1, p2) -> r"` lines.
fn distinct_type_shapes(members: &[&StructuralSignature]) -> Vec<String> {
    let mut set: BTreeSet<String> = BTreeSet::new();
    for m in members {
        set.insert(format!(
            "({}) -> {}",
            m.type_shape.params.join(", "),
            m.type_shape.result
        ));
    }
    set.into_iter().collect()
}

/// The distinct call sequences across a cluster, as `"[a, b, c]"` lines.
fn distinct_call_seqs(members: &[&StructuralSignature]) -> Vec<String> {
    let mut set: BTreeSet<String> = BTreeSet::new();
    for m in members {
        set.insert(format!("[{}]", m.call_sequence.join(", ")));
    }
    set.into_iter().collect()
}

/// For a same-shape call divergence, describe which positions vary, e.g.
/// `pos 1: {validate|check}`.
fn positional_call_diff(members: &[&StructuralSignature]) -> String {
    let len = members[0].call_sequence.len();
    let mut parts: Vec<String> = Vec::new();
    for pos in 0..len {
        let variants: BTreeSet<&str> = members
            .iter()
            .filter_map(|m| m.call_sequence.get(pos).map(String::as_str))
            .collect();
        if variants.len() > 1 {
            parts.push(format!(
                "pos {}: {{{}}}",
                pos,
                variants.into_iter().collect::<Vec<_>>().join("|")
            ));
        }
    }
    if parts.is_empty() {
        "(callees differ)".to_string()
    } else {
        parts.join(", ")
    }
}
