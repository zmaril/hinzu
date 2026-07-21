//! The `hinzu api-fluessig` I/O path: read a hinzu API report JSON off disk,
//! run the pure [`hinzu_core::fluessig_api::build_fluessig`] transform, and write
//! the `api.json` + `catalog.json` the fluessig binding generator consumes (plus
//! an optional coverage-stats sidecar). All filesystem effects live here; core
//! only transforms the parsed value.

use std::path::Path;

use anyhow::{Context, Result};
use hinzu_core::api::ApiReport;
use hinzu_core::fluessig_api::{build_fluessig, Stats};

/// Read and parse one hinzu API report off disk.
fn read_report(path: &Path) -> Result<ApiReport> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading the API report from {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("parsing {} as a hinzu API report", path.display()))
}

/// Read `apireport` (primary) plus any `context` sibling-package reports,
/// convert, and write the two documents. Returns the coverage [`Stats`] so the
/// CLI can print/persist the feasibility evidence. All filesystem effects live
/// here; core only transforms the parsed values.
pub fn run(
    apireport: &Path,
    context: &[std::path::PathBuf],
    out_api: &Path,
    out_catalog: &Path,
    out_stats: Option<&Path>,
) -> Result<Stats> {
    let report = read_report(apireport)?;
    let context_reports: Vec<ApiReport> = context
        .iter()
        .map(|p| read_report(p))
        .collect::<Result<_>>()?;

    let out = build_fluessig(&report, &context_reports);

    let api_json =
        serde_json::to_string_pretty(&out.api).context("serializing the fluessig api.json")?;
    std::fs::write(out_api, format!("{api_json}\n"))
        .with_context(|| format!("writing api.json to {}", out_api.display()))?;

    let catalog_json = serde_json::to_string_pretty(&out.catalog)
        .context("serializing the fluessig catalog.json")?;
    std::fs::write(out_catalog, format!("{catalog_json}\n"))
        .with_context(|| format!("writing catalog.json to {}", out_catalog.display()))?;

    if let Some(sp) = out_stats {
        let stats_json =
            serde_json::to_string_pretty(&out.stats).context("serializing coverage stats")?;
        std::fs::write(sp, format!("{stats_json}\n"))
            .with_context(|| format!("writing coverage stats to {}", sp.display()))?;
    }

    Ok(out.stats)
}

/// A short human summary of the coverage stats, for stderr.
pub fn summary(stats: &Stats) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "hinzu api-fluessig: {} source items → {} models, {} enums, {} interfaces, {} unions ({} lifted from aliases)\n",
        stats.items_in,
        stats.models_emitted,
        stats.enums_emitted,
        stats.interfaces_emitted,
        stats.unions_synthesized,
        stats.unions_lifted,
    ));
    if stats.context_reports > 0 {
        s.push_str(&format!(
            "  cross-package: {} context report(s), {} sibling type(s) pulled in (transitively referenced)\n",
            stats.context_reports, stats.context_types_pulled,
        ));
    }
    s.push_str(&format!(
        "  ops: {} total, {} cleanly typed, {} degraded (a param/return fell back to Json)\n",
        stats.ops_total, stats.ops_clean, stats.ops_degraded,
    ));
    s.push_str(&format!(
        "  fields: {} total, {} degraded · params: {} total, {} degraded · returns: {} degraded\n",
        stats.fields_total,
        stats.fields_degraded,
        stats.params_total,
        stats.params_degraded,
        stats.returns_degraded,
    ));
    if !stats.dropped.is_empty() {
        s.push_str("  dropped items:\n");
        for (k, n) in &stats.dropped {
            s.push_str(&format!("    {n:>3}  {k}\n"));
        }
    }
    if !stats.degradation_reasons.is_empty() {
        s.push_str("  Json fallbacks by cause:\n");
        for (k, n) in &stats.degradation_reasons {
            s.push_str(&format!("    {n:>3}  {k}\n"));
        }
    }
    s
}
