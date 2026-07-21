//! The language-profile model: what a given (language, extractor) pairing can
//! and cannot see, shipped as *data* in the core so every finding can cite the
//! capability edges that bear on it. A profile is the fidelity block for
//! structural similarity, the exact analogue of [`crate::graph::Fidelity`].
//!
//! Profiles are honest by construction. The Rust/syn profile says plainly that
//! it is *syntactic*: it compares types by their written form (never resolving
//! an alias to its underlying type), it cannot see through a macro invocation,
//! and it does not monomorphize — so it can never confirm that two generic
//! functions instantiate to the same concrete shape. Those limits lower
//! confidence and surface as explicit counter-evidence rather than being hidden.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A capability/limitation card for one `(language, extractor)` pairing. This is
/// the fidelity block a similarity run reports, so a consumer sees what the
/// structural analysis captured and what it could not, next to the findings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageProfile {
    /// The language this profile describes (`"rust"`, `"typescript"`, …).
    pub language: String,
    /// The extractor that produced the signatures (`"syn"`, `"ts-morph"`, …).
    pub extractor: String,
    /// Capability grades, keyed by capability. Values are `"yes"`, `"no"`,
    /// `"partial"`, or `"syntactic"` — the last meaning "observed from syntax,
    /// not resolved by a type checker". Keys: `types_resolved`,
    /// `call_targets_known`, `macro_expansion_visible`, `control_flow_available`,
    /// `generics_visible`, `dynamic_dispatch_understood`, `suggestion_scope`.
    pub capabilities: BTreeMap<String, String>,
    /// The abstraction families this profile can reasonably suggest — the only
    /// families a finding from this profile will name.
    pub abstraction_families: Vec<String>,
    /// Honest prose limitations, carried into every finding whose reasoning they
    /// touch.
    pub limitations: Vec<String>,
}

impl LanguageProfile {
    /// Look up a capability grade, or `"unknown"` when the profile does not
    /// carry that key.
    pub fn capability(&self, key: &str) -> &str {
        self.capabilities
            .get(key)
            .map(String::as_str)
            .unwrap_or("unknown")
    }

    /// Whether this profile resolves types (as opposed to reading them
    /// syntactically). Drives the confidence cap: a syntactic-only profile can
    /// never be fully certain two signatures share a type.
    pub fn types_are_resolved(&self) -> bool {
        self.capability("types_resolved") == "yes"
    }
}

/// Build a capabilities map from ordered `(key, value)` pairs.
fn caps(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// The Rust/syn profile: a **syntactic** structural extractor built on `syn`.
///
/// It reads control flow, generics, and call *names* straight from the AST, but
/// it resolves nothing: types are compared by their written form (so a type and
/// its alias look different, and two unrelated types spelled the same look
/// identical), macro invocation bodies are opaque, and there is no
/// monomorphization — it cannot confirm two generic instantiations are the same
/// concrete shape. Those limits are stated here so every finding can cite them.
pub fn rust_syn_profile() -> LanguageProfile {
    LanguageProfile {
        language: "rust".to_string(),
        extractor: "syn".to_string(),
        capabilities: caps(&[
            ("types_resolved", "syntactic"),
            ("call_targets_known", "syntactic"),
            ("macro_expansion_visible", "no"),
            ("control_flow_available", "yes"),
            ("generics_visible", "yes"),
            ("dynamic_dispatch_understood", "no"),
            ("suggestion_scope", "language_specific"),
        ]),
        abstraction_families: vec![
            "helper_function".to_string(),
            "generic_function".to_string(),
            "trait".to_string(),
            "macro_rules".to_string(),
            "proc_macro_derive".to_string(),
            "builder".to_string(),
            "enum_dispatch".to_string(),
            "generated_declaration".to_string(),
        ],
        limitations: vec![
            "Syntactic only: types are compared by their written form, not resolved to a \
             canonical identity — a type and its alias look different, and two distinct types \
             spelled the same look identical."
                .to_string(),
            "Macro invocations are opaque: the extractor sees a macro call but not the code it \
             expands to, so logic hidden inside a macro is invisible to the comparison."
                .to_string(),
            "No monomorphization: generic functions are compared as written, so the analysis \
             cannot confirm that two generic instantiations are structurally identical at the \
             type level."
                .to_string(),
            "Call targets are matched by name, not resolved to definitions: two same-named \
             callees in different modules are treated as the same call."
                .to_string(),
            "Dynamic dispatch through trait objects and function pointers is not understood; the \
             call sequence records the syntactic callee only."
                .to_string(),
        ],
    }
}

/// The Rust/`stablemir` profile: a structural extractor built on the compiler's
/// own **monomorphized MIR**, via the StableMIR (`rustc_public`) driver.
///
/// It is honestly *richer* than the Rust/syn profile, and says exactly where. MIR
/// is post-type-resolution and post-monomorphization, so: types are the
/// compiler's resolved types (`types_resolved` is `yes`, not `syntactic` — a type
/// alias collapses to its underlying constructor, and a concrete function's slots
/// are the real types); call targets are resolved callees (`call_targets_known`
/// is `yes`); macro-generated logic is already expanded into the body
/// (`macro_expansion_visible` is `yes` — the syn opacity caveat is lifted). The
/// costs are stated as limitations, not hidden: source-level structured nesting is
/// lowered to flat basic blocks (so `control_flow_available` is `partial` and
/// `max_nesting` is 0), and indirect dispatch through fn-pointers / trait objects
/// is still unresolved (`dynamic_dispatch_understood` is `partial`). Its
/// abstraction families are the same as the Rust/syn profile, so a finding from
/// either Rust extractor names the same Rust mechanisms.
pub fn rust_stablemir_profile() -> LanguageProfile {
    let syn = rust_syn_profile();
    LanguageProfile {
        language: "rust".to_string(),
        extractor: "stablemir".to_string(),
        capabilities: caps(&[
            ("types_resolved", "yes"),
            ("call_targets_known", "yes"),
            ("macro_expansion_visible", "yes"),
            ("control_flow_available", "partial"),
            ("generics_visible", "yes"),
            ("dynamic_dispatch_understood", "partial"),
            ("suggestion_scope", "language_specific"),
        ]),
        // Same families as the syn Rust profile: the resolved path changes what is
        // seen, not which Rust mechanisms a finding may suggest.
        abstraction_families: syn.abstraction_families,
        limitations: vec![
            // The umbrella caveat FIRST (finding_profile treats limitation[0] as
            // the always-applies umbrella). Unlike syn's, it states the resolution
            // win: types and macro bodies ARE resolved/visible; structural
            // sameness still does not imply behavioural sameness.
            "Resolved but structural: types are the compiler's resolved types and macro bodies are \
             expanded (the syntactic caveats are lifted), but structural sameness still does not \
             imply behavioural sameness — two identically-shaped type slots may be genuinely \
             different types."
                .to_string(),
            "MIR is post-monomorphization and post-expansion: type aliases collapse to their \
             underlying constructor and macro-generated logic is visible in the body — the syn \
             extractor's alias and macro-opacity caveats do NOT apply here."
                .to_string(),
            "Source-level control flow is lowered to flat basic blocks: structured nesting is not \
             recoverable (`max_nesting` reads 0), and `if` vs `match` and loops are read \
             best-effort from `SwitchInt` fan-out and CFG back-edges rather than from source \
             syntax."
                .to_string(),
            "Dynamic dispatch through trait objects and function pointers is still unresolved: an \
             indirect call has no resolved callee, so it does not appear in the call sequence \
             (surfaced, not faked)."
                .to_string(),
            "A generic function that is never monomorphized at the item level is signed from its \
             polymorphic body, so its type parameters appear as `_` (unresolved, like syn); the \
             resolution win lands on concrete functions and on aliases in any signature."
                .to_string(),
        ],
    }
}

/// The TypeScript/`tsc-checker` profile: a structural extractor built on the
/// TypeScript compiler API, driving the **type checker**.
///
/// It is honestly *richer* than the Rust/syn profile: because the checker
/// resolves parameter/return types before they are erased, two functions with the
/// same shape but different concrete types (`Promise<User>` vs `Promise<Order>`)
/// are seen as the same shape — `types_resolved` is `yes`, not `syntactic`. Call
/// targets are likewise resolved through the checker where they are statically
/// resolvable, and generics are visible. But the asymmetry cuts both ways and the
/// profile says so: `any`/`unknown` collapse type-shape distinctions, structural
/// typing means two nominally-different types can share a shape (a source of
/// over-merging), and dynamic dispatch / duck typing is not modeled. This
/// contrast with Rust is the whole point of the language-profile concept — the
/// capability edges are made visible, per `(language, extractor)`.
pub fn ts_tsc_profile() -> LanguageProfile {
    LanguageProfile {
        language: "typescript".to_string(),
        extractor: "tsc-checker".to_string(),
        capabilities: caps(&[
            ("types_resolved", "yes"),
            ("call_targets_known", "partial"),
            ("macro_expansion_visible", "n/a"),
            ("control_flow_available", "yes"),
            ("generics_visible", "yes"),
            ("dynamic_dispatch_understood", "no"),
            ("suggestion_scope", "language_specific"),
        ]),
        abstraction_families: vec![
            "helper_function".to_string(),
            "generic_function".to_string(),
            "higher_order_function".to_string(),
            "mapped_type".to_string(),
            "conditional_type".to_string(),
            "shared_schema".to_string(),
            "decorator".to_string(),
            "object_driven_definition".to_string(),
            "generated_client".to_string(),
            "generated_declaration".to_string(),
        ],
        limitations: vec![
            "Structural typing: two nominally-different types can share the same erased shape, so \
             signatures that look identical may model unrelated domains — the analysis may \
             over-merge."
                .to_string(),
            "`any` and `unknown` collapse type-shape distinctions: a parameter typed `any` erases \
             to the same `_` as a precise type, so type resolution silently degrades where the \
             project is loosely typed."
                .to_string(),
            "Call targets are resolved by the checker only where they are statically resolvable; \
             dynamic dispatch and duck typing (a call through an `any`-typed or structurally-typed \
             receiver) are not modeled, so the call sequence records the syntactic callee only."
                .to_string(),
            "Declaration merging and ambient types can distort call resolution: a name may resolve \
             to a merged or ambient declaration rather than the intended one."
                .to_string(),
            "No macros (`n/a`), but code generation and decorators can produce structurally \
             identical bodies whose origin (a generator or a decorator) is not visible in the \
             signature."
                .to_string(),
        ],
    }
}

/// The `rustdoc` **source** profile for the curated-library tier: what reading a
/// crate's public API via `cargo rustdoc --output-format json` can and cannot
/// see. It sees the exposed generic signatures, the generic params, and the
/// where-bounds — enough to reduce a combinator to a virtual `type_shape`. It
/// does **not** see macro expansion, private impls, or any semantics, so a match
/// from it is a *signature-shape* match, never a behaviour match. Shipped as data
/// so every Tier-A (function/combinator) library finding can cite these edges.
pub fn rustdoc_source_profile() -> LanguageProfile {
    LanguageProfile {
        language: "rust".to_string(),
        extractor: "rustdoc".to_string(),
        capabilities: caps(&[
            ("types_resolved", "partial"),
            ("call_targets_known", "no"),
            ("macro_expansion_visible", "no"),
            ("control_flow_available", "no"),
            ("generics_visible", "yes"),
            ("dynamic_dispatch_understood", "no"),
            ("suggestion_scope", "adopt_library"),
        ]),
        abstraction_families: vec!["adopt_library".to_string()],
        limitations: vec![
            // Umbrella caveat FIRST.
            "Signature-shape only: rustdoc exposes a library item's public signature and bounds, \
             not its body or semantics, so a match means the local code has the SAME SHAPE as the \
             library item — never that it does the same thing. Adopting the item is a suggestion to \
             investigate, not a verified equivalence."
                .to_string(),
            "No body and no call graph: the match scores the erased type-shape and arity of the \
             exposed signature; it cannot compare what the library item does internally."
                .to_string(),
            "Adopting a library item adds a dependency (and its transitive tree); a one-off local \
             implementation may be cheaper than the coupling."
                .to_string(),
            "Version skew: the doc'd signature is whatever version was documented; a different \
             pinned version may expose a different shape."
                .to_string(),
        ],
    }
}

/// The `curated-pattern` **source** profile for the curated-library tier: what a
/// hand-authored known-pattern catalog can and cannot match. It only recognizes
/// the boilerplate shapes explicitly encoded in the shipped catalog (the impls a
/// derive eliminates, the loop a combinator replaces), matched syntactically over
/// local `impl`/`enum`/function structure. It is honest that it may miss variants
/// a real derive handles and may be skewed against the crate's actual version.
pub fn curated_pattern_profile() -> LanguageProfile {
    LanguageProfile {
        language: "rust".to_string(),
        extractor: "curated-pattern".to_string(),
        capabilities: caps(&[
            ("types_resolved", "syntactic"),
            ("call_targets_known", "syntactic"),
            ("macro_expansion_visible", "no"),
            ("control_flow_available", "partial"),
            ("generics_visible", "partial"),
            ("dynamic_dispatch_understood", "no"),
            ("suggestion_scope", "adopt_library"),
        ]),
        abstraction_families: vec!["adopt_library".to_string()],
        limitations: vec![
            // Umbrella caveat FIRST.
            "Curated and structural: this match recognizes a hand-authored boilerplate shape the \
             library's derive/combinator would replace — it is a shape match, not a behaviour \
             match, so the local code may do more than the derive expresses."
                .to_string(),
            "Curated-pattern incompleteness: the catalog encodes a SUBSET of what each derive does; \
             a real derive may handle variants, attributes, or edge cases the pattern does not, so \
             the finding may over- or under-claim what adopting it removes."
                .to_string(),
            "Version skew: the pattern is pinned to no exact crate version; the crate's real \
             derive/API may differ from what is transcribed here."
                .to_string(),
            "Adopting a library adds a dependency (and its transitive tree); a one-off local \
             implementation may be cheaper than the coupling."
                .to_string(),
            "Syntactic extraction: the pattern reads trait impls by their written path, so a \
             re-exported or aliased trait can be missed (a false negative), never faked."
                .to_string(),
        ],
    }
}

/// The profile for a language spelling, or `None` when no extractor profile is
/// shipped for it yet (the honest capability edge — an unshipped language is
/// reported as absent, never faked). This is the language-keyed default, which
/// resolves Rust to its *syntactic* (`syn`) profile; a caller that knows the
/// producing extractor should use [`profile_for`] so a resolved run reports the
/// resolved capabilities. Ships Rust/syn and TypeScript/tsc-checker.
pub fn profile_for_language(language: &str) -> Option<LanguageProfile> {
    match language {
        "rust" => Some(rust_syn_profile()),
        "typescript" => Some(ts_tsc_profile()),
        _ => None,
    }
}

/// The profile for a `(language, extractor)` pairing, or `None` when no profile is
/// shipped. This is the extractor-aware lookup: a Rust run reports the `stablemir`
/// (resolved-type) profile when that extractor produced it, and the `syn`
/// (syntactic) profile otherwise — the difference that lifts or keeps the
/// confidence cap. An unrecognized Rust extractor falls back to the syn profile
/// (the conservative default). A non-Rust language ignores the extractor and uses
/// its single shipped profile.
pub fn profile_for(language: &str, extractor: &str) -> Option<LanguageProfile> {
    match (language, extractor) {
        ("rust", "stablemir") => Some(rust_stablemir_profile()),
        ("rust", _) => Some(rust_syn_profile()),
        ("typescript", _) => Some(ts_tsc_profile()),
        _ => None,
    }
}
