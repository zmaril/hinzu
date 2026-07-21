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

/// The profile for a language spelling, or `None` when no extractor profile is
/// shipped for it yet (the honest capability edge — an unshipped language is
/// reported as absent, never faked). Ships Rust/syn and TypeScript/tsc-checker.
pub fn profile_for_language(language: &str) -> Option<LanguageProfile> {
    match language {
        "rust" => Some(rust_syn_profile()),
        "typescript" => Some(ts_tsc_profile()),
        _ => None,
    }
}
