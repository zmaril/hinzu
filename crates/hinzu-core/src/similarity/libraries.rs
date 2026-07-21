//! The curated-library "adopt the library" tier of `hinzu similar`.
//!
//! The base engine (`super`) compares LOCAL signatures to each other. This tier
//! compares local structure to **external shapes** — the shapes a library the
//! user likes already exposes, and the boilerplate its derives eliminate —
//! expressed in the *same* structural vocabulary and consumed here as **data**,
//! exactly like local signatures. The core stays pure: reading rustdoc JSON,
//! invoking `cargo`, and parsing the config all live in the CLI layer, which
//! hands this module the already-reduced external shapes.
//!
//! Two tiers over one external-shape vocabulary:
//!
//! * **Tier A — function/trait/combinator.** A library item becomes a *virtual*
//!   [`StructuralSignature`] scored against local signatures with the engine's
//!   own shape scorers. A `rustdoc`-sourced virtual signature carries only the
//!   exposed type-shape (no body), so it is matched on `type_shape` + arity; a
//!   `curated`-sourced virtual signature encodes the hand-written *body shape*
//!   the combinator replaces (loop + `?` + push), so it is matched on
//!   control-flow + call shape too.
//! * **Tier B — derive/macro.** A derive has no runtime signature — what it has
//!   is the boilerplate it removes. So Tier B is a hand-authored catalog of
//!   structural **predicates** over local `impl`/`enum` blocks
//!   ([`TypeImplFacts`]): `thiserror::Error`, `derive_more::From`,
//!   `derive_more::Display`, `strum::Display`/`EnumString`.
//!
//! Every match is **advisory and honest**: `confidence = user_trust ×
//! structural_similarity × source_fidelity`, and the counter-evidence always
//! records that a shape match is not a behaviour match, that adopting adds a
//! dependency, that versions may skew, and (for curated) that the pattern is
//! incomplete. A tier that can produce no real match produces no finding
//! (fail-closed).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::profile::{curated_pattern_profile, rustdoc_source_profile};
use super::{
    call_sequence_similarity, cfg_similarity, to_member, type_shape_similarity, Arity,
    FindingProfile, LanguageProfile, LikelyAbstraction, Member, StructuralSignature,
};

// ---------------------------------------------------------------------------
// External-shape vocabulary (the fixed side of the match).
// ---------------------------------------------------------------------------

/// What kind of library item an external shape names. Mirrors the config's
/// `kinds` values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExternalKind {
    /// A free/associated generic function or combinator.
    Function,
    /// A trait whose shape a local impl matches.
    Trait,
    /// A `#[derive(...)]` that eliminates hand-written boilerplate.
    Derive,
    /// A function-like macro.
    Macro,
}

impl ExternalKind {
    /// The lowercase spelling used in config and output.
    pub fn as_str(self) -> &'static str {
        match self {
            ExternalKind::Function => "function",
            ExternalKind::Trait => "trait",
            ExternalKind::Derive => "derive",
            ExternalKind::Macro => "macro",
        }
    }
}

/// Where an external shape came from — the honest provenance of the match.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExternalSource {
    /// Read from the crate's public API via `cargo rustdoc --output-format json`
    /// (signature-shape only — no body, no semantics).
    Rustdoc,
    /// A hand-authored entry in the shipped curated catalog (may miss variants /
    /// be version-skewed).
    Curated,
}

impl ExternalSource {
    /// The lowercase spelling used in output.
    pub fn as_str(self) -> &'static str {
        match self {
            ExternalSource::Rustdoc => "rustdoc",
            ExternalSource::Curated => "curated",
        }
    }

    /// The source-fidelity factor in the confidence formula: rustdoc reads the
    /// crate's real exposed signatures, so it is more faithful than a hand
    /// transcription; a curated pattern is a deliberate subset.
    fn fidelity(self) -> f64 {
        match self {
            ExternalSource::Rustdoc => 0.9,
            ExternalSource::Curated => 0.7,
        }
    }
}

/// A reference to the external library item a finding points at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalRef {
    /// The crate the item belongs to (`"thiserror"`, `"itertools"`).
    pub library: String,
    /// The item name (`"Error"`, `"process_results"`).
    pub item: String,
    /// The item kind.
    pub kind: ExternalKind,
    /// Where the shape came from.
    pub source: ExternalSource,
    /// The advisory version the shape was drawn from, if the config pinned one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

// ---------------------------------------------------------------------------
// Tier A input: virtual signatures.
// ---------------------------------------------------------------------------

/// How a virtual signature is matched against local signatures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchMode {
    /// Signature-shape only (rustdoc): score `type_shape` + arity. There is no
    /// body, so control-flow and call signals are not used — never faked.
    Signature,
    /// Body-shape (curated): score the control-flow skeleton + call sequence +
    /// type shape, so a hand-written loop/accumulation body can be recognized.
    BodyShape,
}

/// A library item reduced to a virtual signature in the structural space, plus
/// the trust the config assigned its crate and how it should be matched. Built
/// by the CLI (from rustdoc JSON or a hand-authored descriptor) and handed here
/// as data.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VirtualSignature {
    /// The library item this virtual signature stands for.
    pub external: ExternalRef,
    /// The `user_trust` factor for the item's crate (0..1).
    pub trust: f64,
    /// How to match it.
    pub match_mode: MatchMode,
    /// The virtual signature itself — the same shape a local extractor emits.
    pub signature: StructuralSignature,
    /// A human line describing the boilerplate/shape adopting the item replaces.
    pub eliminates: String,
}

// ---------------------------------------------------------------------------
// Tier B input: local impl/enum facts.
// ---------------------------------------------------------------------------

/// One trait impl on a local type, reduced to the few facts the curated
/// predicates read. Produced by the CLI's syn walker.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TraitImpl {
    /// The last path segment of the implemented trait (`"Display"`, `"Error"`,
    /// `"From"`, `"FromStr"`).
    pub trait_name: String,
    /// The trait as written (`"std::fmt::Display"`), for evidence.
    pub trait_full: String,
    /// For `From<T>`, the erased shape of `T`; `None` for other traits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_arg_shape: Option<String>,
    /// Whether the impl's primary method body is a `match self { … }` — the
    /// per-variant dispatch shape a `Display`/`FromStr` derive generates.
    #[serde(default)]
    pub body_is_match_self: bool,
    /// Whether a `From` body simply wraps its argument into a single-field
    /// variant/newtype (`E::V(x)` / `E(x)`) — the `derive_more::From` shape.
    #[serde(default)]
    pub is_wrapping: bool,
    /// The impl block's first line, for the finding location.
    pub line_start: u32,
    /// The impl block's last line.
    pub line_end: u32,
}

/// The impl/enum facts of one local type, the unit Tier B matches over.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TypeImplFacts {
    /// The type's name (`"PromptError"`).
    pub type_name: String,
    /// The defining file.
    pub file: String,
    /// The type declaration's first line.
    pub line_start: u32,
    /// The type declaration's last line.
    pub line_end: u32,
    /// Whether the type is an `enum` (vs a struct).
    pub is_enum: bool,
    /// The number of enum variants (0 for a struct), for evidence.
    #[serde(default)]
    pub variant_count: u32,
    /// The trait impls found on this type.
    pub traits: Vec<TraitImpl>,
}

impl TypeImplFacts {
    /// The first trait impl whose last segment is `name`, if any.
    fn trait_by(&self, name: &str) -> Option<&TraitImpl> {
        self.traits.iter().find(|t| t.trait_name == name)
    }
}

// ---------------------------------------------------------------------------
// The finding.
// ---------------------------------------------------------------------------

/// One curated-library candidate: local code that has the shape of an external
/// library item worth investigating adopting. The sibling of
/// [`super::Finding`], pointed outward.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LibraryFinding {
    /// The candidate id (`"lib-1"`, …).
    pub id: String,
    /// The local code that matched (a function, or the impls a derive replaces).
    pub local: Vec<Member>,
    /// The external library item.
    pub external: ExternalRef,
    /// What structurally matched — the concrete evidence.
    pub match_basis: Vec<String>,
    /// What differs between the local code and the library item — why it might
    /// not be a drop-in.
    pub differences: Vec<String>,
    /// The abstraction family, always `adopt_library`, with the mechanism.
    pub likely_abstraction: LikelyAbstraction,
    /// `user_trust × structural_similarity × source_fidelity`, 0..1.
    pub confidence: f64,
    /// One line explaining the confidence.
    pub confidence_basis: String,
    /// The honest reasons NOT to adopt.
    pub counter_evidence: Vec<String>,
    /// The source profile's capability/limitation block, scoped to this finding.
    pub profile: FindingProfile,
}

// ---------------------------------------------------------------------------
// The matcher entry point.
// ---------------------------------------------------------------------------

/// The knobs the library matcher takes.
#[derive(Clone, Debug)]
pub struct LibraryParams {
    /// A crate name → `user_trust` (0..1) for every crate the config activated,
    /// keyed also by which kinds are active. Tier B (curated) reads this to know
    /// which shipped patterns to run and at what trust.
    pub curated_crates: BTreeMap<String, CuratedSelection>,
    /// The minimum Tier-A structural similarity for a virtual-signature match.
    pub min_virtual_match: f64,
}

/// Which kinds of a curated crate are active, and the trust to score them at.
#[derive(Clone, Debug)]
pub struct CuratedSelection {
    /// The `user_trust` factor (0..1).
    pub trust: f64,
    /// Whether `derive` patterns for this crate are active.
    pub derive: bool,
    /// Whether `function` (combinator) patterns for this crate are active.
    pub function: bool,
}

impl Default for LibraryParams {
    fn default() -> Self {
        LibraryParams {
            curated_crates: BTreeMap::new(),
            min_virtual_match: 0.6,
        }
    }
}

/// Match local structure against the external shapes and return the library
/// candidates, sorted by confidence descending and numbered `lib-1`, `lib-2`, …
///
/// Pure: it reads no files. `local_sigs` are the base run's signatures (both
/// singletons and cluster members are eligible — a caller passes them all);
/// `impl_facts` are the local `impl`/`enum` blocks the CLI extracted;
/// `virtual_sigs` are the Tier-A external signatures the CLI built.
pub fn match_libraries(
    local_sigs: &[StructuralSignature],
    impl_facts: &[TypeImplFacts],
    virtual_sigs: &[VirtualSignature],
    params: &LibraryParams,
) -> Vec<LibraryFinding> {
    let mut findings: Vec<LibraryFinding> = Vec::new();

    // Tier A: score every local signature against every virtual signature.
    for vsig in virtual_sigs {
        for local in local_sigs {
            if let Some(f) = match_virtual(local, vsig, params.min_virtual_match) {
                findings.push(f);
            }
        }
    }

    // Tier B: run the active curated predicates over the local impl/enum facts,
    // and the active curated body patterns over the local function signatures.
    for (crate_name, sel) in &params.curated_crates {
        if sel.derive {
            for pat in curated_derive_patterns(crate_name) {
                for facts in impl_facts {
                    if let Some(f) = pat.match_type(facts, sel.trust) {
                        findings.push(f);
                    }
                }
            }
        }
        if sel.function {
            for pat in curated_body_patterns(crate_name) {
                for local in local_sigs {
                    if let Some(f) = pat.match_body(local, sel.trust) {
                        findings.push(f);
                    }
                }
            }
        }
    }

    sort_and_number(&mut findings);
    findings
}

/// Sort library findings by confidence desc, then local-symbol id, and mint
/// stable `lib-N` ids.
fn sort_and_number(findings: &mut [LibraryFinding]) {
    findings.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.first_id().cmp(b.first_id()))
            .then_with(|| a.external.item.cmp(&b.external.item))
    });
    for (n, f) in findings.iter_mut().enumerate() {
        f.id = format!("lib-{}", n + 1);
    }
}

impl LibraryFinding {
    fn first_id(&self) -> &str {
        self.local
            .first()
            .map(|m| m.symbol_id.as_str())
            .unwrap_or("")
    }
}

/// The confidence from the honest formula, rounded to two decimals and clamped
/// to `[0, 0.95]` — a curated-library match is advisory and never asserts near
/// certainty.
fn confidence(trust: f64, structural: f64, source: ExternalSource) -> f64 {
    let raw = trust.clamp(0.0, 1.0) * structural.clamp(0.0, 1.0) * source.fidelity();
    (raw.min(0.95) * 100.0).round() / 100.0
}

/// Build a [`LibraryFinding`] from the parts every tier shares: the honest
/// confidence formula, its basis line, the source profile, and the universal +
/// source-specific counter-evidence. The `id` is minted later by
/// [`sort_and_number`]. Shared by all three matchers so the finding shape and the
/// confidence wording live in exactly one place.
#[allow(clippy::too_many_arguments)]
fn make_finding(
    local: Vec<Member>,
    external: ExternalRef,
    match_basis: Vec<String>,
    differences: Vec<String>,
    mechanism: String,
    trust: f64,
    structural: f64,
    extra_counter: Vec<String>,
) -> LibraryFinding {
    let source = external.source;
    let profile = match source {
        ExternalSource::Rustdoc => rustdoc_source_profile(),
        ExternalSource::Curated => curated_pattern_profile(),
    };
    LibraryFinding {
        id: String::new(),
        local,
        external,
        match_basis,
        differences,
        likely_abstraction: adopt(mechanism),
        confidence: confidence(trust, structural, source),
        confidence_basis: format!(
            "trust {:.2} × structural {:.2} × {} fidelity {:.2}",
            trust,
            structural,
            source.as_str(),
            source.fidelity()
        ),
        counter_evidence: base_counter_evidence(source, extra_counter),
        profile: source_finding_profile(&profile),
    }
}

/// The `adopt_library` abstraction with a mechanism line.
fn adopt(mechanism: String) -> LikelyAbstraction {
    LikelyAbstraction {
        family: "adopt_library".to_string(),
        rationale: "The local code has the structural shape of a library item you already trust; \
                    investigate adopting it instead of maintaining the hand-written version — \
                    weighing the differences and the cost of the dependency below."
            .to_string(),
        language_mechanisms: vec![mechanism],
    }
}

/// The universal counter-evidence every library finding carries, plus any
/// source-specific extras. Order: shape≠behaviour, dependency, version skew,
/// then source extras.
fn base_counter_evidence(source: ExternalSource, extras: Vec<String>) -> Vec<String> {
    let mut out = vec![
        "semantics unverified: this is a structural shape match, not a proof of equivalent \
         behaviour — the local code may do something the library item does not (or vice versa)."
            .to_string(),
        "adopting the item adds a dependency (and its transitive tree); a one-off local \
         implementation may be cheaper than the coupling and the supply-chain surface."
            .to_string(),
        "possible version skew: the matched shape is drawn from one version of the crate; a \
         different pinned version may behave or be shaped differently."
            .to_string(),
    ];
    if source == ExternalSource::Curated {
        out.push(
            "curated-pattern incompleteness: the pattern encodes a subset of what the real \
             derive/combinator does, so what it claims to eliminate may be approximate."
                .to_string(),
        );
    }
    out.extend(extras);
    out
}

/// The finding-profile block for a source profile, scoped to a finding: its
/// capability grades and its limitations (the umbrella caveat always; the rest
/// as shipped).
fn source_finding_profile(profile: &LanguageProfile) -> FindingProfile {
    let keys = [
        "types_resolved",
        "macro_expansion_visible",
        "generics_visible",
        "control_flow_available",
        "suggestion_scope",
    ];
    let mut capabilities_used: Vec<String> = keys
        .iter()
        .filter_map(|k| {
            let v = profile.capability(k);
            (v != "unknown").then(|| format!("{k}={v}"))
        })
        .collect();
    capabilities_used.sort();
    FindingProfile {
        capabilities_used,
        limitations: profile.limitations.clone(),
    }
}

// ---------------------------------------------------------------------------
// Tier A: virtual-signature matching.
// ---------------------------------------------------------------------------

/// Score one local signature against one virtual signature and, if it clears the
/// bar, build the finding. `Signature` mode (rustdoc) uses type-shape + arity
/// only; `BodyShape` mode (curated) also uses the control-flow skeleton and the
/// call sequence, and requires the tell-tale accumulation callee to be present.
fn match_virtual(
    local: &StructuralSignature,
    vsig: &VirtualSignature,
    min_match: f64,
) -> Option<LibraryFinding> {
    let ts = type_shape_similarity(&local.type_shape, &vsig.signature.type_shape);
    let ar = arity_similarity(&local.arity, &vsig.signature.arity);

    let (structural, mut basis, extras) = match vsig.match_mode {
        MatchMode::Signature => {
            // Precision guard: a generic library combinator (2+ type params) is
            // only "reinvented" by a LOCAL generic function. A concrete,
            // monomorphic local fn that merely happens to return the same erased
            // shape (e.g. any 2-arg `-> Result<_,_>`) is not a reimplementation —
            // erased signatures are too coarse to claim otherwise. Reject it
            // rather than emit a low-signal match.
            if vsig.signature.arity.generics >= 2 && local.arity.generics == 0 {
                return None;
            }
            // Require the result shapes to agree exactly — a body-free match has
            // little else to stand on.
            if local.type_shape.result != vsig.signature.type_shape.result {
                return None;
            }
            let structural = 0.7 * ts + 0.3 * ar;
            let basis = vec![
                format!(
                    "same signature shape ({}) -> {} as `{}::{}`",
                    local.type_shape.params.join(", "),
                    local.type_shape.result,
                    vsig.external.library,
                    vsig.external.item,
                ),
                format!(
                    "comparable arity (params={}, generics={})",
                    local.arity.params, local.arity.generics
                ),
            ];
            (structural, basis, Vec::new())
        }
        MatchMode::BodyShape => {
            // The curated combinator shape: a loop that accumulates with `?`.
            // Require the shape to actually be loop+try, else no match — never
            // faked from an unrelated body.
            if local.cfg.loop_count == 0 || local.cfg.try_count == 0 {
                return None;
            }
            let has_accum = local
                .call_sequence
                .iter()
                .any(|c| vsig.signature.call_sequence.iter().any(|w| c == w));
            if !has_accum {
                return None;
            }
            let cfgs = cfg_similarity(&local.cfg, &vsig.signature.cfg);
            let calls =
                call_sequence_similarity(&local.call_sequence, &vsig.signature.call_sequence);
            let structural = 0.4 * cfgs + 0.3 * calls + 0.3 * ts;
            let basis = vec![format!(
                "a loop that accumulates with `?` ({} loop(s), {} `?`/try, calls [{}]) — the \
                     hand-written shape `{}::{}` replaces",
                local.cfg.loop_count,
                local.cfg.try_count,
                local.call_sequence.join(", "),
                vsig.external.library,
                vsig.external.item,
            )];
            let extras = vec![
                "the `?` may not sit inside the loop (the flat control-flow skeleton records body \
                 totals, not nesting) — confirm the accumulation is fallible before adopting."
                    .to_string(),
            ];
            (structural, basis, extras)
        }
    };

    if structural < min_match {
        return None;
    }

    basis.push(format!("eliminates: {}", vsig.eliminates));
    let differences = virtual_differences(local, vsig);
    Some(make_finding(
        vec![to_member(local)],
        vsig.external.clone(),
        basis,
        differences,
        format!(
            "replace the hand-written body with `{}::{}`",
            vsig.external.library, vsig.external.item
        ),
        vsig.trust,
        structural,
        extras,
    ))
}

/// The axes on which the local body and the virtual signature differ.
fn virtual_differences(local: &StructuralSignature, vsig: &VirtualSignature) -> Vec<String> {
    let mut out = Vec::new();
    if local.type_shape.result != vsig.signature.type_shape.result {
        out.push(format!(
            "result shape differs: local `{}` vs library `{}`",
            local.type_shape.result, vsig.signature.type_shape.result
        ));
    }
    if local.arity.params != vsig.signature.arity.params {
        out.push(format!(
            "parameter count differs: local {} vs library {}",
            local.arity.params, vsig.signature.arity.params
        ));
    }
    if out.is_empty() {
        out.push(
            "no structural axis diverges materially — but adopting still changes the code's \
             dependency surface and its exact behaviour on edge cases."
                .to_string(),
        );
    }
    out
}

/// Arity closeness: 1.0 for identical param/generic counts, decaying with the
/// gap. A coarse but honest size signal for the body-free Tier-A match.
fn arity_similarity(a: &Arity, b: &Arity) -> f64 {
    let pd = (a.params as f64 - b.params as f64).abs();
    let gd = (a.generics as f64 - b.generics as f64).abs();
    (1.0 - (pd + gd) / 6.0).max(0.0)
}

// ---------------------------------------------------------------------------
// Tier B: curated derive predicates over impl/enum facts.
// ---------------------------------------------------------------------------

/// A curated Tier-B pattern: a structural predicate over a local type's impls
/// that recognizes boilerplate a specific derive eliminates.
struct DerivePattern {
    external: ExternalRef,
    /// The mechanism line for the finding (`"#[derive(thiserror::Error)]"`).
    mechanism: String,
    /// Which curated predicate recognizes this pattern. A closed enum rather than
    /// a stored `fn` pointer, so every call the engine makes stays statically
    /// resolvable — the functional-core self-check rejects an unresolved indirect
    /// call, and an indirect `fn`-pointer dispatch reads as exactly that.
    kind: DeriveKind,
    /// The structural-similarity value a match scores (predicate matches are
    /// near-certain structurally; behavioural doubt lives in source_fidelity +
    /// counter-evidence).
    structural: f64,
}

/// The closed set of curated derive predicates. [`DerivePattern::match_type`]
/// dispatches on this by `match` (a direct call per arm) instead of an indirect
/// `fn`-pointer call, keeping the whole call graph resolvable for the self-check.
#[derive(Clone, Copy)]
enum DeriveKind {
    ThiserrorError,
    DeriveMoreFrom,
    DeriveMoreDisplay,
    StrumDisplay,
    StrumEnumString,
}

impl DeriveKind {
    /// Run the predicate: `Some(DeriveMatch)` on a match, `None` otherwise.
    fn check(self, facts: &TypeImplFacts) -> Option<DeriveMatch> {
        match self {
            DeriveKind::ThiserrorError => thiserror_error_check(facts),
            DeriveKind::DeriveMoreFrom => derive_more_from_check(facts),
            DeriveKind::DeriveMoreDisplay => derive_more_display_check(facts),
            DeriveKind::StrumDisplay => strum_display_check(facts),
            DeriveKind::StrumEnumString => strum_enum_string_check(facts),
        }
    }
}

/// The result of a derive predicate matching: the evidence and the concrete
/// local members (impl blocks) the derive replaces.
struct DeriveMatch {
    match_basis: Vec<String>,
    differences: Vec<String>,
    members: Vec<Member>,
}

impl DerivePattern {
    fn match_type(&self, facts: &TypeImplFacts, trust: f64) -> Option<LibraryFinding> {
        let m = self.kind.check(facts)?;
        Some(make_finding(
            m.members,
            self.external.clone(),
            m.match_basis,
            m.differences,
            self.mechanism.clone(),
            trust,
            self.structural,
            Vec::new(),
        ))
    }
}

/// Build a [`Member`] for a type's own declaration.
fn type_member(facts: &TypeImplFacts, suffix: &str) -> Member {
    Member {
        symbol_id: format!("{}::{}{}", facts.file, facts.type_name, suffix),
        display: format!("{}{}", facts.type_name, suffix),
        language: "rust".to_string(),
        file: facts.file.clone(),
        line_start: facts.line_start,
        line_end: facts.line_end,
    }
}

/// A [`Member`] for one impl block on a type.
fn impl_member(facts: &TypeImplFacts, tr: &TraitImpl) -> Member {
    Member {
        symbol_id: format!(
            "{}::{}::impl {}",
            facts.file, facts.type_name, tr.trait_name
        ),
        display: format!("impl {} for {}", tr.trait_name, facts.type_name),
        language: "rust".to_string(),
        file: facts.file.clone(),
        line_start: tr.line_start,
        line_end: tr.line_end,
    }
}

/// The first `Display` impl on a type whose body dispatches on `self` — the
/// per-variant formatting shape a `derive_more::Display`/`strum::Display` derive
/// generates. Shared by both patterns.
fn find_display_match(facts: &TypeImplFacts) -> Option<&TraitImpl> {
    facts
        .traits
        .iter()
        .find(|t| t.trait_name == "Display" && t.body_is_match_self)
}

/// The curated derive patterns active for a crate. An unknown crate has none.
fn curated_derive_patterns(crate_name: &str) -> Vec<DerivePattern> {
    match crate_name {
        "thiserror" => vec![thiserror_error_pattern()],
        "derive_more" => vec![derive_more_from_pattern(), derive_more_display_pattern()],
        "strum" => vec![strum_display_pattern(), strum_enum_string_pattern()],
        _ => Vec::new(),
    }
}

fn ext(library: &str, item: &str, kind: ExternalKind) -> ExternalRef {
    ExternalRef {
        library: library.to_string(),
        item: item.to_string(),
        kind,
        source: ExternalSource::Curated,
        version: None,
    }
}

/// `thiserror::Error`: a type with BOTH a hand-written `impl Display` and an
/// `impl std::error::Error`. The derive replaces both impls with one attribute.
fn thiserror_error_pattern() -> DerivePattern {
    DerivePattern {
        external: ext("thiserror", "Error", ExternalKind::Derive),
        mechanism: "#[derive(thiserror::Error)] with #[error(\"…\")] per variant/field".to_string(),
        structural: 0.9,
        kind: DeriveKind::ThiserrorError,
    }
}

/// The `thiserror::Error` predicate body: a type with BOTH a hand-written
/// `Display` impl and an `Error` impl.
fn thiserror_error_check(facts: &TypeImplFacts) -> Option<DeriveMatch> {
    let display = facts.trait_by("Display")?;
    let error = facts.trait_by("Error")?;
    let mut basis = vec![format!(
        "hand-written `impl {}` at {}:{}-{} and `impl {}` at {}:{}-{} — exactly the \
                     boilerplate `#[derive(thiserror::Error)]` generates",
        display.trait_full,
        facts.file,
        display.line_start,
        display.line_end,
        error.trait_full,
        facts.file,
        error.line_start,
        error.line_end,
    )];
    if facts.is_enum {
        basis.push(format!(
            "on an enum ({} variant(s)) whose `Display` dispatches on `self` — thiserror's \
                     per-variant `#[error]` is the idiomatic replacement",
            facts.variant_count
        ));
    }
    let mut differences = vec![
        "thiserror derives a single `#[error(\"…\")]` format per variant; a hand-written \
                 `Display` with conditionals (e.g. an optional appended `cause`) may not reduce to \
                 one format string without behaviour change."
            .to_string(),
    ];
    if facts.trait_by("From").is_some() {
        differences.push(
            "the type also has a `From` impl — thiserror's `#[from]` can subsume it, but \
                     only for a single-field source variant."
                .to_string(),
        );
    }
    Some(DeriveMatch {
        members: vec![
            type_member(facts, ""),
            impl_member(facts, display),
            impl_member(facts, error),
        ],
        match_basis: basis,
        differences,
    })
}

/// `derive_more::From`: an `impl From<T> for E` whose body wraps `T` into a
/// single-field variant/newtype.
fn derive_more_from_pattern() -> DerivePattern {
    DerivePattern {
        external: ext("derive_more", "From", ExternalKind::Derive),
        mechanism: "#[derive(derive_more::From)] on the wrapping variant/newtype".to_string(),
        structural: 0.85,
        kind: DeriveKind::DeriveMoreFrom,
    }
}

/// The `derive_more::From` predicate body.
fn derive_more_from_check(facts: &TypeImplFacts) -> Option<DeriveMatch> {
    let from = facts
        .traits
        .iter()
        .find(|t| t.trait_name == "From" && t.is_wrapping)?;
    let arg = from
        .from_arg_shape
        .clone()
        .unwrap_or_else(|| "_".to_string());
    Some(DeriveMatch {
        members: vec![type_member(facts, ""), impl_member(facts, from)],
        match_basis: vec![format!(
            "`impl From<{}> for {}` at {}:{}-{} that just wraps its argument into a \
                     single-field variant/newtype — the shape `#[derive(derive_more::From)]` \
                     generates",
            arg, facts.type_name, facts.file, from.line_start, from.line_end
        )],
        differences: vec![
            "derive_more generates one `From` per single-field variant; a hand-written \
                     `From` that transforms its argument (not a plain wrap) is NOT equivalent."
                .to_string(),
        ],
    })
}

/// `derive_more::Display`: an `impl Display` whose body is a `match self` with a
/// `write!`/`f.write_str` per arm, WITHOUT an accompanying `Error` impl (that
/// case belongs to thiserror).
fn derive_more_display_pattern() -> DerivePattern {
    DerivePattern {
        external: ext("derive_more", "Display", ExternalKind::Derive),
        mechanism: "#[derive(derive_more::Display)] with #[display(\"…\")] per variant".to_string(),
        structural: 0.8,
        kind: DeriveKind::DeriveMoreDisplay,
    }
}

/// The `derive_more::Display` predicate body.
fn derive_more_display_check(facts: &TypeImplFacts) -> Option<DeriveMatch> {
    if facts.trait_by("Error").is_some() {
        return None; // thiserror covers Display+Error together
    }
    let display = find_display_match(facts)?;
    Some(DeriveMatch {
        members: vec![type_member(facts, ""), impl_member(facts, display)],
        match_basis: vec![format!(
            "`impl {}` at {}:{}-{} whose body is a `match self` with a formatting arm per \
                     variant — the shape `#[derive(derive_more::Display)]` generates",
            display.trait_full, facts.file, display.line_start, display.line_end
        )],
        differences: vec![
            "derive_more::Display expresses each arm as a `#[display(\"…\")]` format; arms \
                     with non-trivial logic (loops, branches, computed values) do not reduce to a \
                     format attribute."
                .to_string(),
        ],
    })
}

/// `strum::Display`: same enum `Display`-via-`match self` shape, offered when the
/// user's config lists `strum` rather than `derive_more`.
fn strum_display_pattern() -> DerivePattern {
    DerivePattern {
        external: ext("strum", "Display", ExternalKind::Derive),
        mechanism: "#[derive(strum::Display)] (+ #[strum(serialize = \"…\")] where names differ)"
            .to_string(),
        structural: 0.75,
        kind: DeriveKind::StrumDisplay,
    }
}

/// The `strum::Display` predicate body.
fn strum_display_check(facts: &TypeImplFacts) -> Option<DeriveMatch> {
    if !facts.is_enum {
        return None;
    }
    // An enum that also implements `Error` is a thiserror candidate, not a
    // strum::Display one — defer so an error enum gets the better-fitting
    // suggestion rather than a redundant second one.
    if facts.trait_by("Error").is_some() {
        return None;
    }
    let display = find_display_match(facts)?;
    Some(DeriveMatch {
        members: vec![type_member(facts, ""), impl_member(facts, display)],
        match_basis: vec![format!(
            "enum `{}` with a `Display` at {}:{}-{} that maps each variant to a string via \
                     `match self` — `#[derive(strum::Display)]` generates exactly this",
            facts.type_name, facts.file, display.line_start, display.line_end
        )],
        differences: vec![
            "strum::Display maps a variant to a fixed string (its name or a \
                     `#[strum(serialize)]`); an arm that formats fields or computes the string is \
                     not expressible as a strum attribute."
                .to_string(),
        ],
    })
}

/// `strum::EnumString`: an `impl FromStr` on an enum whose body matches string
/// literals to variants.
fn strum_enum_string_pattern() -> DerivePattern {
    DerivePattern {
        external: ext("strum", "EnumString", ExternalKind::Derive),
        mechanism: "#[derive(strum::EnumString)]".to_string(),
        structural: 0.75,
        kind: DeriveKind::StrumEnumString,
    }
}

/// The `strum::EnumString` predicate body.
fn strum_enum_string_check(facts: &TypeImplFacts) -> Option<DeriveMatch> {
    if !facts.is_enum {
        return None;
    }
    let fromstr = facts.trait_by("FromStr")?;
    Some(DeriveMatch {
        members: vec![type_member(facts, ""), impl_member(facts, fromstr)],
        match_basis: vec![format!(
            "enum `{}` with a hand-written `FromStr` at {}:{}-{} mapping strings to \
                     variants — `#[derive(strum::EnumString)]` generates this parser",
            facts.type_name, facts.file, fromstr.line_start, fromstr.line_end
        )],
        differences: vec![
            "strum::EnumString derives an exact/serialize-keyed parser; a hand-written \
                     `FromStr` with fuzzy matching, aliases, or normalization needs \
                     `#[strum(...)]` attributes or does not map cleanly."
                .to_string(),
        ],
    })
}

// ---------------------------------------------------------------------------
// Tier B (curated) body patterns over local function signatures.
// ---------------------------------------------------------------------------

/// The result of a body-pattern predicate matching: `(match_basis, differences)`.
type BodyMatch = (Vec<String>, Vec<String>);

/// A curated body pattern: a predicate over a local function's structural
/// signature that recognizes a hand-written body a combinator replaces. This is
/// the robust form of Tier A's `BodyShape` match, expressed as a shipped
/// predicate so it needs no external descriptor.
struct BodyPattern {
    external: ExternalRef,
    mechanism: String,
    structural: f64,
    /// Which curated body predicate this is. A closed enum rather than a stored
    /// `fn` pointer, so the engine's dispatch stays statically resolvable for the
    /// functional-core self-check (see [`DeriveKind`]).
    kind: BodyKind,
}

/// The closed set of curated body predicates. [`BodyPattern::match_body`]
/// dispatches on this by `match` (a direct call) rather than an indirect
/// `fn`-pointer call.
#[derive(Clone, Copy)]
enum BodyKind {
    ItertoolsProcessResults,
}

impl BodyKind {
    fn check(self, sig: &StructuralSignature) -> Option<BodyMatch> {
        match self {
            BodyKind::ItertoolsProcessResults => itertools_process_results_check(sig),
        }
    }
}

impl BodyPattern {
    fn match_body(&self, sig: &StructuralSignature, trust: f64) -> Option<LibraryFinding> {
        let (basis, differences) = self.kind.check(sig)?;
        Some(make_finding(
            vec![to_member(sig)],
            self.external.clone(),
            basis,
            differences,
            self.mechanism.clone(),
            trust,
            self.structural,
            vec![
                "the `?` may not sit inside the loop (the flat control-flow skeleton records body \
                 totals, not nesting) — confirm the accumulation is fallible before adopting the \
                 combinator."
                    .to_string(),
            ],
        ))
    }
}

/// The curated body patterns active for a crate.
fn curated_body_patterns(crate_name: &str) -> Vec<BodyPattern> {
    match crate_name {
        "itertools" => vec![itertools_process_results_pattern()],
        _ => Vec::new(),
    }
}

/// `itertools::process_results` / `Iterator::collect::<Result<_,_>>()`: a loop
/// that accumulates fallible items with `?` into a container. Recognized by a
/// body with at least one loop, at least one `?`, and a `push`/`insert`/`extend`
/// callee.
fn itertools_process_results_pattern() -> BodyPattern {
    BodyPattern {
        external: ExternalRef {
            library: "itertools".to_string(),
            item: "process_results".to_string(),
            kind: ExternalKind::Function,
            source: ExternalSource::Curated,
            version: None,
        },
        mechanism: "`iter.map(f).collect::<Result<C, _>>()` (std) or `itertools::process_results` \
                    for a fallible reduction"
            .to_string(),
        structural: 0.7,
        kind: BodyKind::ItertoolsProcessResults,
    }
}

/// The `itertools::process_results` predicate body.
fn itertools_process_results_check(sig: &StructuralSignature) -> Option<BodyMatch> {
    if sig.cfg.loop_count == 0 || sig.cfg.try_count == 0 {
        return None;
    }
    let accum: Vec<&str> = sig
        .call_sequence
        .iter()
        .map(String::as_str)
        .filter(|c| matches!(*c, "push" | "insert" | "extend" | "push_str"))
        .collect();
    if accum.is_empty() {
        return None;
    }
    let basis = vec![format!(
        "a loop that accumulates with `?` into a container ({} loop(s), {} `?`/try, \
                 accumulator call(s): {}) — the manual shape `collect::<Result<_,_>>()` / \
                 `itertools::process_results` replaces",
        sig.cfg.loop_count,
        sig.cfg.try_count,
        accum.join(", "),
    )];
    let differences = vec![
                "`collect::<Result<_,_>>()` short-circuits on the first `Err` and needs the item \
                 to be a `Result`; a loop doing extra work per item (side effects, multiple pushes, \
                 filtering) is not a one-liner."
                    .to_string(),
            ];
    Some((basis, differences))
}

#[cfg(test)]
mod tests;
