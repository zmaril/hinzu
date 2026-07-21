//! CLI orchestration for the curated-library "adopt the library" tier of
//! `hinzu similar`. Split out of `main.rs` so the subcommand file stays under the
//! straitjacket size limit; the pure matching lives in
//! [`hinzu_core::similarity::libraries`], and this module only reads the config,
//! extracts local `impl`/`enum` facts, and threads the data across the seam.

use std::path::Path;

use anyhow::{Context, Result};
use hinzu_core::similarity::{
    curated_pattern_profile, match_libraries, rustdoc_source_profile, ExternalSource,
    LibraryFinding, SimilarityOutput, StructuralSignature,
};

use crate::library_config::LibraryConfig;
use crate::{library_extract, rust_adapter};

/// Run the curated-library tier and attach its findings to `output`. Reads the
/// `--libraries` config, extracts the local `impl`/`enum` facts (over a live
/// cargo project — the offline `--structural` path has no project to walk, stated
/// honestly), lowers the config into the core's inputs, and calls the pure
/// matcher. The core stays pure: every external shape crosses the seam as data.
///
/// `project` is the analyzed path; `offline` is true when the run used
/// `--structural` pre-extracted signatures (so there is no project to read impl
/// blocks from).
pub fn run(
    cfg_path: &Path,
    project: &Path,
    offline: bool,
    local_rust_sigs: &[StructuralSignature],
    output: &mut SimilarityOutput,
) -> Result<()> {
    let cfg = LibraryConfig::load(cfg_path)?;
    let base = cfg_path.parent().unwrap_or(Path::new("."));
    let lowered = cfg.lower(base)?;
    for note in &lowered.notes {
        eprintln!("{note}");
    }

    // Local impl/enum facts drive the derive tier. They need a real project to
    // walk; the offline `--structural` path cannot supply them.
    let impl_facts = if offline {
        eprintln!(
            "libraries: --structural is offline signatures only — the derive tier needs a live \
             project to read impl/enum blocks, so it is skipped (the combinator/rustdoc tiers still \
             run over the signatures)"
        );
        Vec::new()
    } else if rust_adapter::is_cargo_project(project) {
        library_extract::extract(project)
            .with_context(|| format!("extracting impl/enum facts from {}", project.display()))?
    } else {
        Vec::new()
    };

    let findings = match_libraries(
        local_rust_sigs,
        &impl_facts,
        &lowered.virtual_sigs,
        &lowered.params,
    );

    // Record the source profiles that produced findings, so the fidelity block
    // covers the library tier too (mirrors how base findings carry a profile).
    attach_profiles(output, &findings);
    output.library_candidates = findings;
    Ok(())
}

/// Print the adopt-a-library summary section to stderr (nothing when the tier
/// produced no candidates). Kept next to the tier so the subcommand file stays
/// small; mirrors the base similarity summary's `=== … ===` convention.
pub fn print_summary(output: &SimilarityOutput) {
    if output.library_candidates.is_empty() {
        return;
    }
    eprintln!(
        "=== adopt-a-library: {} candidate(s) ===",
        output.library_candidates.len()
    );
    for c in &output.library_candidates {
        eprintln!(
            "  {} [{:.2} confidence] adopt {}::{} ({}, {}) — {}",
            c.id,
            c.confidence,
            c.external.library,
            c.external.item,
            c.external.kind.as_str(),
            c.external.source.as_str(),
            c.likely_abstraction
                .language_mechanisms
                .first()
                .map(String::as_str)
                .unwrap_or(""),
        );
        for m in &c.local {
            eprintln!(
                "      {} ({}:{}-{})",
                m.display, m.file, m.line_start, m.line_end
            );
        }
    }
}

/// Add the `rustdoc` / `curated-pattern` source profiles to the output's
/// `profiles` block for whichever sources actually produced a library finding —
/// so the capability edges the library tier relied on are reported next to it.
fn attach_profiles(output: &mut SimilarityOutput, findings: &[LibraryFinding]) {
    let mut want_rustdoc = false;
    let mut want_curated = false;
    for f in findings {
        match f.external.source {
            ExternalSource::Rustdoc => want_rustdoc = true,
            ExternalSource::Curated => want_curated = true,
        }
    }
    let has = |ext: &str, out: &SimilarityOutput| out.profiles.iter().any(|p| p.extractor == ext);
    if want_rustdoc && !has("rustdoc", output) {
        output.profiles.push(rustdoc_source_profile());
    }
    if want_curated && !has("curated-pattern", output) {
        output.profiles.push(curated_pattern_profile());
    }
}
