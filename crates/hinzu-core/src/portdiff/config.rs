//! Configuration for [`super::port_diff`] — the language pair, naming rules,
//! band thresholds, and the optional conformance oracle. Everything the
//! matcher keys on is data here, so the algorithm carries no hardcoded
//! project knowledge.

use serde::{Deserialize, Serialize};

/// The naming ruleset that lowers source and target symbol ids into a common
/// `(module_path, leaf)` spelling. Every rule is data, so a different
/// language-pair could supply its own ruleset without touching the code — though
/// only the TypeScript→Rust ruleset is exercised today.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NamingRules {
    /// How a source **file path segment** is normalized. Only `"kebab_to_snake"`
    /// (`google-shared` → `google_shared`) is implemented.
    pub file_segment_case: String,
    /// Compound file suffixes stripped before the language extension is removed,
    /// e.g. `[".lazy"]` folds `anthropic-messages.lazy.ts` onto
    /// `anthropic-messages`.
    pub strip_suffixes: Vec<String>,
    /// How a **function / method leaf** is normalized. Only `"camel_to_snake"`
    /// (`convertMessages` → `convert_messages`) is implemented.
    pub fn_case: String,
    /// Keep PascalCase type names verbatim (both languages spell types the same).
    pub keep_pascal_types: bool,
    /// Keep SCREAMING_SNAKE constants verbatim (both languages spell them same).
    pub keep_screaming_consts: bool,
    /// The target crate prefixes on target ids (`[atilla_ai]`). Retained for
    /// id-based fallback and documentation; module anchoring uses
    /// [`Self::target_src_prefix`] (the defining file), which is more reliable than
    /// the id. One entry per target crate a source package maps to (usually one).
    pub strip_crate_prefix: Vec<String>,
    /// The workspace-relative source directories of the **target** crates
    /// (`[crates/atilla-ai/src]`); a target file under any of them is anchored to a
    /// module by stripping the matching prefix. Falls back to the generic
    /// `crates/<x>/src/` shape. One entry per target crate a source package maps to
    /// (usually one); merging several crates' graphs is how a source package ported
    /// across crates stays visible to the matcher.
    pub target_src_prefix: Vec<String>,
    /// The leading directory of the **source** package (`src`) stripped before a
    /// source file becomes a module path.
    pub source_src_prefix: String,
}

/// How the conformance manifest supplies the test-verified DONE oracle. A native
/// module's `src` path (e.g. `packages/ai/src/api/anthropic-messages.ts`) is
/// mapped to a source file path by stripping [`Self::src_prefix_strip`]
/// (`packages/ai/`), yielding `src/api/anthropic-messages.ts` — the same spelling
/// the source graph uses.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConformanceConfig {
    /// Filesystem path to the conformance `manifest.json`. Read best-effort: if it
    /// cannot be read or parsed, the DONE oracle is empty and a note is recorded.
    pub manifest_path: String,
    /// The `status` value that marks a module test-verified (`"native"`).
    pub native_status: String,
    /// The manifest `package` to filter on (`"ai"`).
    pub package: String,
    /// Prefix stripped from a manifest `src` to recover the source file path
    /// (`"packages/ai/"`).
    pub src_prefix_strip: String,
}

/// The full configuration for [`port_diff`]. Everything the matcher keys on —
/// the language pair, the naming rules, the band thresholds, and the optional
/// conformance oracle — is here, so the algorithm carries no hardcoded
/// project knowledge.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortDiffConfig {
    /// The source language / ecosystem tag (`"ts"`). Selects the normalization
    /// ruleset; only `"ts"` → `"rust"` is implemented.
    pub source_kind: String,
    /// The target language / ecosystem tag (`"rust"`).
    pub target_kind: String,
    /// The naming ruleset lowering ids into the common spelling.
    pub naming: NamingRules,
    /// Coverage at or above which a (non-native) file is banded PORTED. `0.6` in
    /// the prototype.
    pub ported_threshold: f64,
    /// Fraction of the winning weighted-vote mass a clustered subtree must retain
    /// to be selected. `0.6` in the prototype.
    pub cluster_vote_retain: f64,
    /// The optional test-verified DONE oracle. When `None`, no file is banded
    /// DONE (all would-be-DONE files fall to their structural band).
    pub conformance: Option<ConformanceConfig>,
    /// The package name this config diffs (`ai`, `coding-agent`, …), used to tag
    /// merge contributions so the whole-port rollup can tell which package a
    /// contested target file drew each source file from. `None` in synthetic /
    /// single-package uses where the package split is irrelevant.
    #[serde(default)]
    pub package: Option<String>,
}

impl PortDiffConfig {
    /// The TypeScript→Rust configuration matching the `pi` → `atilla` prototype.
    /// The conformance oracle points at atilla's committed manifest.
    pub fn default_ts_rust() -> Self {
        PortDiffConfig {
            source_kind: "ts".to_string(),
            target_kind: "rust".to_string(),
            naming: NamingRules {
                file_segment_case: "kebab_to_snake".to_string(),
                strip_suffixes: vec![".lazy".to_string()],
                fn_case: "camel_to_snake".to_string(),
                keep_pascal_types: true,
                keep_screaming_consts: true,
                strip_crate_prefix: vec!["atilla_ai".to_string()],
                target_src_prefix: vec!["crates/atilla-ai/src".to_string()],
                source_src_prefix: "src".to_string(),
            },
            ported_threshold: 0.6,
            cluster_vote_retain: 0.6,
            conformance: Some(ConformanceConfig {
                manifest_path: "/workspace/atilla/conformance/manifest.json".to_string(),
                native_status: "native".to_string(),
                package: "ai".to_string(),
                src_prefix_strip: "packages/ai/".to_string(),
            }),
            package: Some("ai".to_string()),
        }
    }
}
