//! A self-contained HTML dashboard for a [`PortDiffReport`]. Ports the layout of
//! `scripts/port-graph-html.mjs` to Rust: a header with the overall match %, a
//! band legend and the source/target identity; a file-band bar; a naive-vs-graph
//! recovery panel; the conformance cross-check; the per-wave band view; the
//! ready-frontier list; and a graph-confirmed-vs-name-only table. All CSS is
//! inline and no asset is fetched, so the file renders offline.

use hinzu_core::portdiff::{
    Band, BandCounts, FileEntry, MergeEntry, MergeReport, Misplacement, MultiPackageReport,
    PackageRollup, PortDiffReport, RollupTotals,
};

/// Presentation metadata that is not in the report itself.
pub struct HtmlMeta {
    /// The `--package` name.
    pub package: String,
    /// A short label for the source codebase (e.g. its extraction dir).
    pub source_label: String,
    /// A short label for the target codebase.
    pub target_label: String,
    /// The `--from` closure roots, if the source was scoped (else empty).
    pub scoped_from: Vec<String>,
    /// How the graphs were obtained ("extracted live" / "pre-extracted graphs").
    pub input_mode: String,
}

/// The band's dashboard color.
fn band_color(band: Band) -> &'static str {
    match band {
        Band::Done => "#3fb950",
        Band::Ported => "#58a6ff",
        Band::Relocated => "#a371f7",
        Band::Started => "#d29922",
        Band::NotStarted => "#6e7681",
    }
}

/// The five band header colors in report order: DONE, PORTED, RELOCATED,
/// STARTED, NOT-STARTED. Shared by every band-column table header so the color
/// legend is spelled once and inline-captured by the header templates.
#[allow(clippy::type_complexity)]
fn band_header_colors() -> (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
) {
    (
        band_color(Band::Done),
        band_color(Band::Ported),
        band_color(Band::Relocated),
        band_color(Band::Started),
        band_color(Band::NotStarted),
    )
}

/// The band's display label (matches the JSON `serde` rename).
fn band_name(band: Band) -> &'static str {
    match band {
        Band::Done => "DONE",
        Band::Ported => "PORTED",
        Band::Relocated => "RELOCATED",
        Band::Started => "STARTED",
        Band::NotStarted => "NOT-STARTED",
    }
}

/// `0.42` → `"42%"`, `None` → `"–"`.
fn pct_opt(x: Option<f64>) -> String {
    match x {
        Some(v) => format!("{}%", (v * 100.0).round() as i64),
        None => "–".to_string(),
    }
}

/// `0.42` → `"42%"`.
fn pct(x: f64) -> String {
    format!("{}%", (x * 100.0).round() as i64)
}

/// HTML-escape `&`, `<`, `>`.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Strip a leading `src/` for compact display.
fn short_path(p: &str) -> &str {
    p.strip_prefix("src/").unwrap_or(p)
}

/// A horizontal band bar (`done`→`not_started`) over `total`.
fn band_bar(bands: &BandCounts, total: usize) -> String {
    if total == 0 {
        return String::from(r#"<div class="bar"></div>"#);
    }
    let segs = [
        (Band::Done, bands.done),
        (Band::Ported, bands.ported),
        (Band::Relocated, bands.relocated),
        (Band::Started, bands.started),
        (Band::NotStarted, bands.not_started),
    ];
    let mut s = String::from(r#"<div class="bar">"#);
    for (band, v) in segs {
        if v == 0 {
            continue;
        }
        let w = v as f64 / total as f64 * 100.0;
        s.push_str(&format!(
            r#"<span class="seg" style="width:{w:.3}%;background:{c}" title="{name}: {v}"></span>"#,
            c = band_color(band),
            name = band_name(band),
        ));
    }
    s.push_str("</div>");
    s
}

/// A band pill (`DONE` / `PORTED` / …).
fn band_pill(band: Band) -> String {
    format!(
        r#"<span class="pill" style="--pc:{c}">{name}</span>"#,
        c = band_color(band),
        name = band_name(band),
    )
}

/// Render the full dashboard.
pub fn render_html(report: &PortDiffReport, meta: &HtmlMeta) -> String {
    let o = &report.overall;
    let b = &o.bands;
    let total_files = o.source_files_total;
    let by_path: std::collections::HashMap<&str, &FileEntry> =
        report.files.iter().map(|f| (f.path.as_str(), f)).collect();

    let mut h = String::with_capacity(48 * 1024);
    h.push_str(&head(report, meta));
    h.push_str(&cards(report));
    h.push_str(&bands_panel(b, total_files));
    h.push_str(r#"<div class="two">"#);
    h.push_str(&naive_panel(report, &by_path));
    h.push_str(&conformance_panel(report));
    h.push_str("</div>");
    h.push_str(&merges_panel(&report.merges));
    h.push_str(&waves_panel(report));
    h.push_str(&frontier_panel(report));
    h.push_str(&graph_confirm_panel(report));
    h.push_str("</body></html>");
    h
}

/// The split-not-merge panel. Two violation types, high-severity first:
/// **misplacements** (a source file ported into a crate owned by another package)
/// and **cross-package file-merges** (a target file drawing substantial content
/// from source files of ≥ 2 packages), then the lower-severity same-package
/// file-merges. Renders nothing when the report is clean.
fn merges_panel(merges: &MergeReport) -> String {
    if merges.file_merges.is_empty() && merges.misplacements.is_empty() {
        return String::new();
    }
    let cross: Vec<&MergeEntry> = merges
        .file_merges
        .iter()
        .filter(|e| e.cross_package)
        .collect();
    let same: Vec<&MergeEntry> = merges
        .file_merges
        .iter()
        .filter(|e| !e.cross_package)
        .collect();

    let misplace_rows: String = merges.misplacements.iter().map(misplacement_row).collect();
    let cross_rows: String = cross.iter().copied().map(merge_row).collect();
    let same_rows: String = same.iter().copied().map(merge_row).collect();

    let misplace_section = if merges.misplacements.is_empty() {
        String::new()
    } else {
        format!(
            r#"<div class="dim sm" style="margin:6px 0 4px">MISPLACEMENTS <span class="q">high severity — a source file ported into a crate owned by another package</span></div>
    <table><thead><tr><th>source file → target file</th><th>package → owning package</th><th>strong / total</th></tr></thead><tbody>{misplace_rows}</tbody></table>"#,
        )
    };
    let cross_section = if cross.is_empty() {
        String::new()
    } else {
        format!(
            r#"<div class="dim sm" style="margin:14px 0 4px">CROSS-PACKAGE FILE-MERGES <span class="q">high severity — a target file drew substantial content from source files of ≥ 2 packages</span></div>
    <table><thead><tr><th>target file</th><th>packages</th><th>contributing source files → strong / total</th></tr></thead><tbody>{cross_rows}</tbody></table>"#,
        )
    };
    let same_section = if same.is_empty() {
        String::new()
    } else {
        format!(
            r#"<div class="dim sm" style="margin:14px 0 4px">FILE-MERGES <span class="q">≥ 2 source files of one package merged into one target file</span></div>
    <table><thead><tr><th>target file</th><th>packages</th><th>contributing source files → strong / total</th></tr></thead><tbody>{same_rows}</tbody></table>"#,
        )
    };
    format!(
        r#"<div class="panel">
    <h2>Split-not-merge violations <span class="q">graph-derived: substantial-contributor file-merges + package misplacements</span></h2>
    <div class="callout">A faithful port keeps each source file's identity and each package's boundary. {nm} misplacement(s) and {nf} file-merge(s) ({nc} cross-package) collapse a boundary — a source file landed in another package's crate, or ≥ 2 source files were merged into one target file. Misplacements and cross-package file-merges are the high-severity ones.</div>
    {misplace_section}
    {cross_section}
    {same_section}
  </div>
"#,
        nm = merges.misplacements.len(),
        nf = merges.file_merges.len(),
        nc = cross.len(),
    )
}

/// One file-merge table row: the merged target file, the packages it spans, and
/// each contributing source file with its strong / total matched-symbol counts.
fn merge_row(e: &MergeEntry) -> String {
    let contribs: String = e
        .contributors
        .iter()
        .map(|c| {
            let pkg = if c.package.is_empty() {
                String::new()
            } else {
                format!(r#"<span class="dim">[{}]</span> "#, esc(&c.package))
            };
            format!(
                r#"<div class="mono sm">{pkg}{sf} <span class="dim">→ {s} / {t}</span></div>"#,
                sf = esc(short_path(&c.source_file)),
                s = c.strong_matched,
                t = c.total_matched,
            )
        })
        .collect();
    format!(
        r#"<tr><td class="mono sm">{tf}</td><td class="sm">{pkgs}</td><td>{contribs}</td></tr>"#,
        tf = esc(&e.target_file),
        pkgs = esc(&e.packages.join(", ")),
    )
}

/// One misplacement table row: the misplaced source file → target file, the
/// source package → the crate's owning package, and the strong / total weight.
fn misplacement_row(m: &Misplacement) -> String {
    format!(
        r#"<tr><td class="mono sm">{sf} <span class="dim">→</span> {tf}</td><td class="sm">{sp} <span class="dim">→</span> {op} <span class="dim">({cr})</span></td><td class="sm">{s} / {t}</td></tr>"#,
        sf = esc(short_path(&m.source_file)),
        tf = esc(&m.target_file),
        sp = esc(&m.source_package),
        op = esc(&m.owning_package),
        cr = esc(&m.target_crate),
        s = m.strong_matched,
        t = m.total_matched,
    )
}

// ===========================================================================
// Combined whole-port dashboard (`--all`)
// ===========================================================================

/// Presentation metadata for the combined (`--all`) dashboard.
pub struct MultiHtmlMeta {
    /// A short label for the source ecosystem (e.g. its language + base dir).
    pub source_label: String,
    /// A short label for the target ecosystem.
    pub target_label: String,
    /// How the graphs were obtained ("extracted live per package" / cache note).
    pub input_mode: String,
}

/// `(done + ported) / files` as a rounded percent — the structural upper bound.
fn done_ported_pct(bands: &BandCounts, files: usize) -> String {
    if files == 0 {
        return "0%".to_string();
    }
    let v = (bands.done + bands.ported) as f64 / files as f64;
    format!("{}%", (v * 100.0).round() as i64)
}

/// Render the combined whole-port dashboard: a top rollup with an overall
/// completion bar and per-package band bars, a summary table with a TOTAL row,
/// and a per-package section reusing the single-package band + wave view.
pub fn render_multi_html(report: &MultiPackageReport, meta: &MultiHtmlMeta) -> String {
    let t = &report.totals;
    let mut h = String::with_capacity(64 * 1024);
    h.push_str(&multi_head(report, meta));
    h.push_str(&multi_hero(report));
    h.push_str(&multi_overall_bar(t));
    h.push_str(&multi_pkg_bars(&report.packages));
    h.push_str(&multi_summary_table(report));
    h.push_str(&merges_panel(&report.merges));
    for pkg in &report.packages {
        h.push_str(&multi_pkg_section(pkg));
    }
    h.push_str("</body></html>");
    h
}

/// The combined dashboard `<head>` + header line.
fn multi_head(report: &MultiPackageReport, meta: &MultiHtmlMeta) -> String {
    let n = report.packages.len();
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Whole-port progress: {src} → {tgt}</title>
<style>{css}{extra}</style></head><body>
<h1>Whole-port progress: <span style="color:var(--acc)">{src}</span> → <span style="color:var(--acc)">{tgt}</span>, graph-matched</h1>
<div class="sub">{n} packages · source <b>{sl}</b> · target <b>{tl}</b> · {mode} · {tf} source files · symbol-graph matching that survives file decomposition &amp; relocation</div>
"#,
        src = esc(&report.source_kind),
        tgt = esc(&report.target_kind),
        sl = esc(&meta.source_label),
        tl = esc(&meta.target_label),
        mode = esc(&meta.input_mode),
        tf = report.totals.source_files_total,
        css = CSS,
        extra = MULTI_CSS,
    )
}

/// The combined hero card row.
fn multi_hero(report: &MultiPackageReport) -> String {
    let t = &report.totals;
    let n = report.packages.len();
    format!(
        r#"<div class="grid cards">
  <div class="card"><div class="big">{pct}</div><div class="lbl">symbols name-matched</div><div class="note">{m}/{tot} matchable across {n} pkgs</div></div>
  <div class="card"><div class="big" style="color:{done_c}">{done}</div><div class="lbl">DONE (test-verified)</div><div class="note">= {native} conformance native ✓</div></div>
  <div class="card"><div class="big">{pd}</div><div class="lbl">PORTED + DONE files</div><div class="note">structural upper bound, of {tf}</div></div>
  <div class="card"><div class="big">{tf}</div><div class="lbl">source files</div><div class="note">{n} packages</div></div>
</div>
"#,
        pct = pct(t.symbols_matched_pct),
        m = t.symbols_matched,
        tot = t.symbols_total,
        done_c = band_color(Band::Done),
        done = t.bands.done,
        native = t.conformance_native,
        pd = t.bands.done + t.bands.ported,
        tf = t.source_files_total,
    )
}

/// The overall completion bar panel.
fn multi_overall_bar(t: &RollupTotals) -> String {
    bar_legend_panel(
        r#"Overall port completion <span class="q">DONE test-verified · PORTED ≥ threshold symbols · RELOCATED moved to secondary crate · STARTED some match · NOT-STARTED none</span>"#,
        &t.bands,
        t.source_files_total,
        true,
    )
}

/// The per-package file-band bars.
fn multi_pkg_bars(packages: &[PackageRollup]) -> String {
    let mut rows = String::new();
    for p in packages {
        rows.push_str(&format!(
            r#"<div class="pkgbar"><div class="nm">{nm}</div>{bar}<div class="pc">{pc}</div></div>"#,
            nm = esc(&p.package),
            bar = band_bar(&p.bands, p.source_files_total),
            pc = done_ported_pct(&p.bands, p.source_files_total),
        ));
    }
    format!(
        r#"<div class="panel">
  <h2>Per-package file bands <span class="q">bar % = (DONE + PORTED) / files, the structural upper bound</span></h2>
  {rows}
</div>
"#,
    )
}

/// The summary table with a TOTAL row.
fn multi_summary_table(report: &MultiPackageReport) -> String {
    let mut rows = String::new();
    for p in &report.packages {
        rows.push_str(&summary_row(
            "",
            &format!(r#"<td class="mono">{}</td>"#, esc(&p.package)),
            p.source_files_total,
            &p.bands,
            p.symbols_matched,
            p.symbols_total,
            p.symbols_matched_pct,
            p.conformance_native,
            &p.wave_count.to_string(),
        ));
    }
    let t = &report.totals;
    let total_row = summary_row(
        r#" class="total""#,
        "<td>TOTAL</td>",
        t.source_files_total,
        &t.bands,
        t.symbols_matched,
        t.symbols_total,
        t.symbols_matched_pct,
        t.conformance_native,
        "—",
    );
    let (dc, pc, rc, sc, nc) = band_header_colors();
    format!(
        r#"<div class="panel">
  <h2>Summary table</h2>
  <table>
    <thead><tr><th>package</th><th class="num">files</th><th class="num" style="color:{dc}">DONE</th><th class="num" style="color:{pc}">PORTED</th><th class="num" style="color:{rc}">RELOCATED</th><th class="num" style="color:{sc}">STARTED</th><th class="num" style="color:{nc}">NOT-STARTED</th><th class="num">symbols matched</th><th class="num">conf. native</th><th class="num">waves</th></tr></thead>
    <tbody>{rows}{total_row}</tbody>
  </table>
  <div class="callout">DONE band == conformance native modules per package — the structural matcher and the test manifest agree on the test-verified floor. STARTED / PORTED / RELOCATED are structural (graph-derived) and under-count by design; RELOCATED marks a port that moved to a secondary target crate.</div>
</div>
"#,
    )
}

/// One summary-table row: `row_attr` is the `<tr>` attributes (`""` or
/// ` class="total"`), `name_cell` the first `<td>`, `waves` the last cell's text.
#[allow(clippy::too_many_arguments)]
fn summary_row(
    row_attr: &str,
    name_cell: &str,
    files: usize,
    bands: &BandCounts,
    sym_matched: usize,
    sym_total: usize,
    pct_val: f64,
    native: usize,
    waves: &str,
) -> String {
    format!(
        r#"<tr{row_attr}>
    {name_cell}
    <td class="num">{files}</td>
    <td class="num" style="color:{dc}">{d}</td>
    <td class="num" style="color:{pc}">{p}</td>
    <td class="num" style="color:{rc}">{r}</td>
    <td class="num" style="color:{sc}">{s}</td>
    <td class="num dim">{ns}</td>
    <td class="num {cls}">{sm}/{st} ({smp})</td>
    <td class="num">{native}</td>
    <td class="num">{waves}</td>
  </tr>"#,
        dc = band_color(Band::Done),
        d = bands.done,
        pc = band_color(Band::Ported),
        p = bands.ported,
        rc = band_color(Band::Relocated),
        r = bands.relocated,
        sc = band_color(Band::Started),
        s = bands.started,
        ns = bands.not_started,
        cls = pct_class(pct_val),
        sm = sym_matched,
        st = sym_total,
        smp = pct(pct_val),
    )
}

/// A per-package section reusing the single-package band + wave view.
fn multi_pkg_section(pkg: &PackageRollup) -> String {
    format!(
        r#"<div class="sec"><h2 style="font-size:17px">{nm} <span class="q">{files} files · {sm}/{st} symbols matched ({smp})</span></h2>
{bands}{waves}</div>
"#,
        nm = esc(&pkg.package),
        files = pkg.source_files_total,
        sm = pkg.symbols_matched,
        st = pkg.symbols_total,
        smp = pct(pkg.symbols_matched_pct),
        bands = bands_panel(&pkg.report.overall.bands, pkg.source_files_total),
        waves = waves_panel(&pkg.report),
    )
}

/// The five-band legend row.
fn band_legend(b: &BandCounts) -> String {
    [
        (Band::Done, b.done),
        (Band::Ported, b.ported),
        (Band::Relocated, b.relocated),
        (Band::Started, b.started),
        (Band::NotStarted, b.not_started),
    ]
    .iter()
    .map(|(band, v)| {
        format!(
            r#"<span><i style="background:{c}"></i>{name} — {v}</span>"#,
            c = band_color(*band),
            name = band_name(*band),
        )
    })
    .collect()
}

/// The name-match % → cell color class.
fn pct_class(p: f64) -> &'static str {
    if p >= 0.7 {
        "hi"
    } else if p >= 0.45 {
        "mid"
    } else {
        "lo"
    }
}

/// Extra styles for the combined view, layered over [`CSS`].
const MULTI_CSS: &str = r#"
.pkgbar{display:flex;align-items:center;gap:14px;margin:9px 0}
.pkgbar .nm{width:130px;font-weight:600;font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:13px}
.pkgbar .bar{flex:1}
.pkgbar .pc{width:52px;text-align:right;font-variant-numeric:tabular-nums;color:var(--dim);font-size:12px}
.big-bar{height:22px}
.sec{margin-top:34px;border-top:1px solid var(--line);padding-top:12px}
tr.total{border-top:2px solid var(--line);font-weight:700}
a{color:var(--acc)}"#;

/// The `<head>`, style block, and header line.
fn head(report: &PortDiffReport, meta: &HtmlMeta) -> String {
    let o = &report.overall;
    let scope = if meta.scoped_from.is_empty() {
        "full package".to_string()
    } else {
        format!("scoped to closure of {}", esc(&meta.scoped_from.join(", ")))
    };
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Port-diff: {pkg} ({src}) → ({tgt})</title>
<style>{css}</style></head><body>
<h1>Port progress: <span style="color:var(--acc)">{pkg}</span> — {src} → {tgt}, graph-matched</h1>
<div class="sub">source <b>{sl}</b> · target <b>{tl}</b> · {scope} · {mode} · {sf} source files · symbol-graph matching that survives file decomposition &amp; relocation</div>
"#,
        pkg = esc(&meta.package),
        src = esc(&report.source_kind),
        tgt = esc(&report.target_kind),
        sl = esc(&meta.source_label),
        tl = esc(&meta.target_label),
        mode = esc(&meta.input_mode),
        sf = o.source_files_total,
        css = CSS,
    )
}

/// The headline card row.
fn cards(report: &PortDiffReport) -> String {
    let o = &report.overall;
    let b = &o.bands;
    let g = &o.graph;
    let nvg = &report.naive_vs_graph;
    format!(
        r#"<div class="grid cards">
  <div class="card"><div class="big">{matched_pct}</div><div class="lbl">symbols name-matched</div><div class="note">{matched}/{total} matchable · {synth} synthetic excluded</div></div>
  <div class="card"><div class="big" style="color:{ported_c}">{gc_pct}</div><div class="lbl">graph-confirmed</div><div class="note">{gc}/{ge} evaluable · mean overlap {mo}</div></div>
  <div class="card"><div class="big" style="color:{done_c}">{done}</div><div class="lbl">DONE (test-verified)</div><div class="note">= {native} conformance native ✓</div></div>
  <div class="card"><div class="big">{pd}</div><div class="lbl">PORTED + DONE files</div><div class="note">structural, of {tf}</div></div>
  <div class="card"><div class="big">{gfm}<span class="dim" style="font-size:18px"> / {tf}</span></div><div class="lbl">files with ≥1 match</div><div class="note">naive exact-path: {nfm}/{tf}</div></div>
</div>
"#,
        matched_pct = pct(o.symbols_matched_pct),
        matched = o.symbols_matched,
        total = o.symbols_total,
        synth = o.symbols_synthetic_excluded,
        ported_c = band_color(Band::Ported),
        gc_pct = pct(g.confirmed_pct_of_evaluable),
        gc = g.confirmed,
        ge = g.evaluable,
        mo = g.mean_edge_overlap,
        done_c = band_color(Band::Done),
        done = b.done,
        native = report.conformance_crosscheck.native_modules,
        pd = b.ported + b.done,
        tf = o.source_files_total,
        gfm = nvg.graph_files_matched,
        nfm = nvg.naive_files_matched,
    )
}

/// The file-band bar + legend panel.
fn bands_panel(b: &BandCounts, total: usize) -> String {
    bar_legend_panel(
        r#"File bands <span class="q">DONE = test-verified · PORTED ≥ threshold symbols · RELOCATED moved to secondary crate · STARTED some match · NOT-STARTED none</span>"#,
        b,
        total,
        false,
    )
}

/// A `panel` wrapping a heading, a full-width band bar, and the band legend.
/// Shared by the single-package file-band panel and the combined overall-
/// completion panel; `big` swaps in the taller `big-bar` variant.
fn bar_legend_panel(heading: &str, bands: &BandCounts, total: usize, big: bool) -> String {
    let bar = if big {
        band_bar(bands, total).replace(r#"class="bar""#, r#"class="bar big-bar""#)
    } else {
        band_bar(bands, total)
    };
    format!(
        r#"<div class="panel">
  <h2>{heading}</h2>
  {bar}
  <div class="legend">{legend}</div>
</div>
"#,
        legend = band_legend(bands),
    )
}

/// The naive-vs-graph recovery panel, with the recovered-files spotlight table.
fn naive_panel(
    report: &PortDiffReport,
    by_path: &std::collections::HashMap<&str, &FileEntry>,
) -> String {
    let nvg = &report.naive_vs_graph;
    let mut rows = String::new();
    for path in nvg.recovered_files.iter().take(14) {
        let Some(fe) = by_path.get(path.as_str()) else {
            continue;
        };
        rows.push_str(&format!(
            r#"<tr>
    <td class="mono">{p}</td>
    <td><span class="tag no">naive-missed</span></td>
    <td>→</td>
    <td>{pill}</td>
    <td class="num">{m}/{t}</td>
    <td class="mono dim">{mt} <span class="dim">({mm})</span></td>
  </tr>"#,
            p = esc(short_path(path)),
            pill = band_pill(fe.band),
            m = fe.matched_symbols,
            t = fe.total_symbols,
            mt = esc(fe.mapped_target.as_deref().unwrap_or("—")),
            mm = esc(fe.map_method.as_deref().unwrap_or("—")),
        ));
    }
    format!(
        r#"<div class="panel">
    <h2>Naive file-existence vs graph-matched</h2>
    <div class="cmp">
      <div class="box"><div class="n" style="color:var(--dim)">{naive}</div><div class="dim">naive: exact normalized-path only</div></div>
      <div class="arrow">→</div>
      <div class="box"><div class="n" style="color:var(--acc)">{graph}</div><div class="dim">graph: files with real symbol matches</div></div>
    </div>
    <div class="callout">Graph clustering recovers <b>{rec}</b> relocated / decomposed files a naive exact-path pass misses — their symbols were split or moved into a differently-named target subtree, but the distinctive-leaf vote still lands them.</div>
    <table style="margin-top:12px">
      <thead><tr><th>recovered file</th><th>naive</th><th></th><th>graph band</th><th class="num">sym</th><th>mapped target</th></tr></thead>
      <tbody>{rows}</tbody>
    </table>
  </div>
"#,
        naive = nvg.naive_files_matched,
        graph = nvg.graph_files_matched,
        rec = nvg.recovered_count,
    )
}

/// The conformance cross-check panel.
fn conformance_panel(report: &PortDiffReport) -> String {
    let cc = &report.conformance_crosscheck;
    format!(
        r#"<div class="panel">
    <h2>Conformance cross-check <span class="q">honesty oracle</span></h2>
    <table>
      <tbody>
      <tr><td>DONE band (my count)</td><td class="num" style="color:{done_c};font-weight:700">{done}</td><td class="dim">conformance native modules</td><td class="num">{native} ✓</td></tr>
      <tr><td>PORTED + DONE (structural)</td><td class="num" style="font-weight:700">{pd}</td><td class="dim">file-level upper bound on "might pass"</td><td class="num">—</td></tr>
      </tbody>
    </table>
    <div class="callout">{note}</div>
  </div>
"#,
        done_c = band_color(Band::Done),
        done = cc.done_band,
        native = cc.native_modules,
        pd = cc.ported_plus_done,
        note = esc(&cc.note),
    )
}

/// The per-wave band-mix table.
fn waves_panel(report: &PortDiffReport) -> String {
    let mut rows = String::new();
    for w in &report.waves {
        rows.push_str(&format!(
            r#"<tr>
    <td class="mono">wave {wave}</td>
    <td class="num">{files}</td>
    <td style="min-width:180px">{bar}</td>
    <td class="num">{d}</td><td class="num">{p}</td><td class="num">{r}</td>
    <td class="num">{s}</td><td class="num">{n}</td>
    <td class="num">{sym}</td>
  </tr>"#,
            wave = w.wave,
            files = w.files,
            bar = band_bar(&w.bands, w.files),
            d = w.bands.done,
            p = w.bands.ported,
            r = w.bands.relocated,
            s = w.bands.started,
            n = w.bands.not_started,
            sym = pct(w.symbols_pct),
        ));
    }
    let (dc, pc, rc, sc, nc) = band_header_colors();
    format!(
        r#"<div class="panel">
  <h2>Waves <span class="q">from the source port plan — band mix + symbol coverage per wave</span></h2>
  <table>
    <thead><tr><th>wave</th><th class="num">files</th><th>band mix</th><th class="num" style="color:{dc}">D</th><th class="num" style="color:{pc}">P</th><th class="num" style="color:{rc}">R</th><th class="num" style="color:{sc}">S</th><th class="num" style="color:{nc}">N</th><th class="num">sym%</th></tr></thead>
    <tbody>{rows}</tbody>
  </table>
</div>
"#,
    )
}

/// The ready-frontier table.
fn frontier_panel(report: &PortDiffReport) -> String {
    let mut rows = String::new();
    for r in &report.ready_frontier {
        rows.push_str(&format!(
            r#"<tr>
    <td class="num">{fan}</td>
    <td class="mono">{p}</td>
    <td>{pill}</td>
    <td class="num">{m}/{t}</td>
    <td class="num">{cov}</td>
    <td class="mono dim">{mt}</td>
  </tr>"#,
            fan = r.fan_in,
            p = esc(short_path(&r.path)),
            pill = band_pill(r.band),
            m = r.matched_symbols,
            t = r.total_symbols,
            cov = pct_opt(r.coverage),
            mt = esc(r.mapped_target.as_deref().unwrap_or("—")),
        ));
    }
    format!(
        r#"<div class="panel">
  <h2>Ready frontier <span class="q">unported files whose src-deps are all PORTED/DONE — sorted by fan_in · {total} total</span></h2>
  <table>
    <thead><tr><th class="num">fan_in</th><th>file</th><th>band</th><th class="num">sym</th><th class="num">cov</th><th>mapped target subtree</th></tr></thead>
    <tbody>{rows}</tbody>
  </table>
</div>
"#,
        total = report.ready_frontier_total,
    )
}

/// The graph-confirmed vs name-only panel: the two ends of the per-file
/// edge-overlap distribution, so a reader can see structure-preserving ports
/// against likely name coincidences.
fn graph_confirm_panel(report: &PortDiffReport) -> String {
    let g = &report.overall.graph;
    // Files with a defined graph-confirmed coverage, sorted by it.
    let mut scored: Vec<&FileEntry> = report
        .files
        .iter()
        .filter(|f| f.graph_confirmed_coverage.is_some() && f.matched_symbols > 0)
        .collect();
    scored.sort_by(|a, b| {
        b.graph_confirmed_coverage
            .partial_cmp(&a.graph_confirmed_coverage)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.fan_in.cmp(&a.fan_in))
            .then_with(|| a.path.cmp(&b.path))
    });
    let high: Vec<&FileEntry> = scored.iter().take(10).copied().collect();
    let low: Vec<&FileEntry> = scored.iter().rev().take(10).copied().collect();
    let row = |f: &FileEntry| -> String {
        let ov = f.graph_confirmed_coverage.unwrap_or(0.0);
        let cls = if ov >= 0.7 {
            "hi"
        } else if ov >= 0.4 {
            "mid"
        } else {
            "lo"
        };
        format!(
            r#"<tr><td class="mono sm">{p}</td><td class="mono sm dim">{mt}</td><td class="num {cls}">{ov:.2}</td><td class="num">{m}/{t}</td></tr>"#,
            p = esc(short_path(&f.path)),
            mt = esc(f.mapped_target.as_deref().unwrap_or("—")),
            ov = ov,
            m = f.matched_symbols,
            t = f.total_symbols,
        )
    };
    let high_rows: String = high.iter().map(|f| row(f)).collect();
    let low_rows: String = low.iter().map(|f| row(f)).collect();
    format!(
        r#"<div class="panel">
  <h2>Graph-confirm: signal &amp; false-positive risk <span class="q">edge-overlap = fraction of a file's matched symbols whose target counterpart preserves the call structure</span></h2>
  <div class="two">
    <div>
      <div class="dim sm" style="margin-bottom:4px">HIGH graph-confirmed coverage — structure preserved</div>
      <table><thead><tr><th>file</th><th>mapped target</th><th class="num">gc-cov</th><th class="num">sym</th></tr></thead><tbody>{high}</tbody></table>
    </div>
    <div>
      <div class="dim sm" style="margin-bottom:4px">LOW graph-confirmed coverage — name coincidence / rewired risk</div>
      <table><thead><tr><th>file</th><th>mapped target</th><th class="num">gc-cov</th><th class="num">sym</th></tr></thead><tbody>{low}</tbody></table>
    </div>
  </div>
  <div class="callout">Name-match alone is noisy: generic leaves recur across sibling modules. Graph-confirm (mean overlap {mo}, {cp} of evaluable ≥ 0.5) separates a genuine port from a lexical collision — it labels confidence, it never silently drops a match.</div>
</div>
"#,
        high = high_rows,
        low = low_rows,
        mo = g.mean_edge_overlap,
        cp = pct(g.confirmed_pct_of_evaluable),
    )
}

/// The inline stylesheet, ported from the reference dashboard.
const CSS: &str = r#":root{--bg:#0d1117;--panel:#161b22;--line:#21262d;--fg:#e6edf3;--dim:#8b949e;--acc:#58a6ff}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:14px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,Helvetica,Arial,sans-serif;padding:28px 32px 60px}
h1{font-size:22px;margin:0 0 2px}
.sub{color:var(--dim);font-size:13px;margin-bottom:22px}
.mono{font-family:ui-monospace,SFMono-Regular,Menlo,monospace}
.sm{font-size:11.5px}
.dim{color:var(--dim)}
.grid{display:grid;gap:16px}
.cards{grid-template-columns:repeat(auto-fit,minmax(150px,1fr));margin-bottom:20px}
.card{background:var(--panel);border:1px solid var(--line);border-radius:10px;padding:14px 16px}
.card .big{font-size:28px;font-weight:650;line-height:1.1}
.card .lbl{color:var(--dim);font-size:12px;margin-top:4px}
.card .note{color:var(--dim);font-size:11px;margin-top:6px}
.panel{background:var(--panel);border:1px solid var(--line);border-radius:10px;padding:18px 20px;margin-bottom:20px}
.panel h2{font-size:15px;margin:0 0 12px;font-weight:600}
.panel h2 .q{color:var(--dim);font-weight:400;font-size:12px;margin-left:8px}
.bar{display:flex;height:14px;border-radius:4px;overflow:hidden;background:#0b0f14}
.seg{display:block;height:100%}
.legend{display:flex;gap:16px;flex-wrap:wrap;margin:10px 0 0;font-size:12px}
.legend span{display:inline-flex;align-items:center;gap:6px;color:var(--dim)}
.legend i{width:11px;height:11px;border-radius:3px;display:inline-block}
table{width:100%;border-collapse:collapse;font-size:12.5px}
th,td{text-align:left;padding:6px 9px;border-bottom:1px solid var(--line);vertical-align:top}
th{color:var(--dim);font-weight:500;font-size:11px;text-transform:uppercase;letter-spacing:.03em}
td.num,th.num{text-align:right;font-variant-numeric:tabular-nums}
.pill{display:inline-block;padding:1px 8px;border-radius:20px;font-size:11px;font-weight:600;color:#0d1117;background:var(--pc)}
.tag{padding:1px 7px;border-radius:5px;font-size:11px}
.tag.ok{background:#1f6f3011;color:#3fb950;border:1px solid #3fb95044}
.tag.no{background:#f8514911;color:#f85149;border:1px solid #f8514944}
.num.hi{color:#3fb950}.num.mid{color:#d29922}.num.lo{color:#f85149}
.two{display:grid;grid-template-columns:1fr 1fr;gap:20px}
@media(max-width:840px){.two{grid-template-columns:1fr}}
.cmp{display:flex;align-items:center;gap:20px;flex-wrap:wrap}
.cmp .box{flex:1;min-width:200px}
.cmp .n{font-size:34px;font-weight:700}
.arrow{font-size:26px;color:var(--dim)}
.callout{font-size:12px;color:var(--dim);border-left:2px solid var(--acc);padding-left:12px;margin-top:12px}"#;
