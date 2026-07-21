//! Cross-language **public-surface conformance diff**: given a SOURCE package's
//! [`ApiReport`](crate::api::ApiReport) (its declared public interface) and a
//! TARGET package's [`ApiReport`](crate::api::ApiReport) — typically the port in
//! another language — grade, item by item, whether the target reproduces the
//! source's contract. For every source public item: is there a matching target
//! item, does its shape line up, or is it missing? And which target items have no
//! source counterpart at all?
//!
//! Where [`crate::portdiff`] bands *files* by how much of the source's internal
//! dependency **graph** has a target counterpart — measuring porting *progress* —
//! this module grades the *public surface* — whether the thing that was ported
//! exposes the **same contract**. Progress and conformance are different
//! questions; the two compose (see `notes/api-diff.md`).
//!
//! ## The pure boundary
//!
//! [`build_api_diff`] is a **pure** transform over two already-extracted
//! [`ApiReport`]s plus a [`NamingRules`] passed in as data: it reads no files and
//! spawns no processes. Running `hinzu api` to produce the two reports, and
//! loading the port config to build the [`NamingRules`], are the CLI's job; this
//! module only matches and grades what it is handed, so it stays inside
//! hinzu-core's functional-core region.
//!
//! ## Naming rules, reused from port-diff
//!
//! Names are normalized with the **same** [`NamingRules`] the port-diff matcher
//! uses (camelCase→snake_case functions, PascalCase types and SCREAMING consts
//! kept verbatim, kebab→snake file segments), so a convention rename
//! (`streamText` ↔ `stream_text`) never reads as a *missing* item. Item module
//! paths are anchored on the defining file with the same source/target
//! file→module lowering, used as a match-quality preference and as evidence.
//!
//! ## Honesty stance
//!
//! Types are **rendered strings** and cross-language type equivalence is
//! *approximate* (`string` ↔ `String`, `number` ↔ `f64`). So a `signatureMismatch`
//! is a **signal, not a proof** — exactly like port-diff's non-DONE bands. The
//! classification is driven by **structural** facts that survive rendering:
//! parameter arity for callables, the field-name set for aggregates, the
//! variant-name set for enums. Rendered type strings are compared only when the
//! two sides share a language; cross-language they ride along as advisory
//! evidence, never as the reason an item is graded a mismatch.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::api::{ApiItem, ApiReport, PackageInfo, HINZU_API_VERSION};
use crate::portdiff::{norm_leaf, source_file_to_module, target_file_to_module, NamingRules};

/// The conformance grade of one source public item against the target surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiffStatus {
    /// A target item matches by name + kind-class and its shape is compatible.
    Matched,
    /// Matched by name + kind-class, but the signature/shape differs (a signal,
    /// not a proof — see the module docs). The specific differences are in
    /// [`ApiDiffItem::mismatch`].
    SignatureMismatch,
    /// No target item matches this source item by name + kind-class.
    Missing,
    /// A target item with no source counterpart (target-only surface).
    Extra,
}

impl DiffStatus {
    /// A stable ordinal for deterministic `(status, kind, name)` sorting.
    fn rank(self) -> u8 {
        match self {
            DiffStatus::Matched => 0,
            DiffStatus::SignatureMismatch => 1,
            DiffStatus::Missing => 2,
            DiffStatus::Extra => 3,
        }
    }
}

/// One concrete way a matched pair's shapes differ. `aspect` names the facet
/// (`"paramCount"`, `"returnType"`, `"missingField"`, `"extraField"`,
/// `"missingVariant"`, `"extraVariant"`, `"paramType"`); `source` / `target`
/// carry the two rendered sides (one may be empty when the facet exists on only
/// one side).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MismatchAspect {
    /// The facet that differs.
    pub aspect: String,
    /// The source side, rendered (empty when absent on the source).
    pub source: String,
    /// The target side, rendered (empty when absent on the target).
    pub target: String,
}

/// One graded item: a source public item and its verdict against the target
/// surface, or (for [`DiffStatus::Extra`]) a target-only item. Evidence — the
/// source and target ids, files, and lines — travels with every pairing so a
/// human or agent can verify it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiDiffItem {
    /// The item's short name (normalized-comparable, rendered as the source spells
    /// it — or, for an `extra` item, as the target spells it).
    pub name: String,
    /// The item kind (the source kind, or the target kind for an `extra` item).
    pub kind: String,
    /// The conformance verdict.
    pub status: DiffStatus,
    /// The source item's stable id (`None` for an `extra`, target-only item).
    pub source_id: Option<String>,
    /// The source item's defining file, when known.
    pub source_file: Option<String>,
    /// The source item's first source line, when known.
    pub source_line: Option<u32>,
    /// The matched (or extra) target item's stable id, when there is one.
    pub target_id: Option<String>,
    /// The target item's defining file, when known.
    pub target_file: Option<String>,
    /// The target item's first source line, when known.
    pub target_line: Option<u32>,
    /// The specific shape differences, for a [`DiffStatus::SignatureMismatch`]
    /// (`None` otherwise).
    pub mismatch: Option<Vec<MismatchAspect>>,
    /// A short human note when one helps (e.g. the cross-kind pairing that
    /// matched, `"interface↔struct"`), else `None`.
    pub note: Option<String>,
}

/// The summary counts + the overall conformance grade.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffSummary {
    /// Source items with a compatible target match.
    pub matched: usize,
    /// Source items with no target match.
    pub missing: usize,
    /// Source items matched by name + kind but with a differing shape.
    pub signature_mismatch: usize,
    /// Target items with no source counterpart.
    pub extra: usize,
    /// `matched / (matched + missing + signatureMismatch)`, rounded to 3 dp — the
    /// fraction of the source public surface that has a compatible target match.
    /// `extra` items are target-only and are not in the denominator.
    pub conformance: f64,
}

/// The per-kind conformance breakdown, keyed by the item kind.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KindBreakdown {
    /// The item kind (`"function"`, `"struct"`, `"interface"`, …).
    pub kind: String,
    pub matched: usize,
    pub missing: usize,
    pub signature_mismatch: usize,
    pub extra: usize,
}

/// The complete cross-language API-conformance report, ready to serialize as
/// JSON. Deterministic: [`items`](ApiDiffReport::items) are sorted by
/// `(status, kind, name)` and [`by_kind`](ApiDiffReport::by_kind) by kind; no
/// timestamps or absolute paths are introduced (the paths are whatever the two
/// reports carry).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiDiffReport {
    /// The schema version, echoed from [`HINZU_API_VERSION`].
    pub hinzu_api_version: u32,
    /// The source package (its public surface is the contract to match).
    pub source: PackageInfo,
    /// The target package (the port whose surface is graded).
    pub target: PackageInfo,
    /// The summary counts + overall conformance grade.
    pub summary: DiffSummary,
    /// The per-kind conformance breakdown, sorted by kind.
    pub by_kind: Vec<KindBreakdown>,
    /// Every graded item, sorted by `(status, kind, name)`.
    pub items: Vec<ApiDiffItem>,
}

/// The broad kind-equivalence class two items must share to be considered a
/// candidate pairing. Compatibility is deliberately **lenient across languages**:
/// within [`KindClass::Type`], a TS `interface` / `typeAlias` / `class` and a Rust
/// `struct` / `enum` / `trait` are all interchangeable, because a ported type may
/// land in any of those shapes. The specific pairing is recorded as evidence
/// (`ApiDiffItem::note`) when the kinds differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KindClass {
    /// `function`, `method`.
    Callable,
    /// `struct`, `enum`, `trait`, `class`, `interface`, `typeAlias`, `record`.
    Type,
    /// `const`.
    Const,
    /// `namespace`.
    Namespace,
    /// Anything else — not indexed or graded.
    Other,
}

/// Map an item kind string to its [`KindClass`]. The equivalence table is
/// documented on [`KindClass`] and in `notes/api-diff.md`.
fn kind_class(kind: &str) -> KindClass {
    match kind {
        "function" | "method" => KindClass::Callable,
        "struct" | "enum" | "trait" | "class" | "interface" | "typeAlias" | "record" => {
            KindClass::Type
        }
        "const" => KindClass::Const,
        "namespace" => KindClass::Namespace,
        _ => KindClass::Other,
    }
}

/// Whether a kind carries a comparable **named-field** shape (so a field-set diff
/// is meaningful). A `trait` / `enum` / `typeAlias` does not, so a source
/// aggregate matched onto one is not penalized for "missing" fields it could not
/// carry.
fn has_fields(kind: &str) -> bool {
    matches!(kind, "struct" | "class" | "interface" | "record")
}

/// A flattened, normalized view of one item from either report — everything the
/// matcher and the shape comparison key on, computed once.
struct NormItem<'a> {
    item: &'a ApiItem,
    norm_name: String,
    class: KindClass,
    module: String,
}

impl<'a> NormItem<'a> {
    /// Normalize one item, anchoring its module on the defining file. `is_source`
    /// selects the file→module lowering (`source_file_to_module` for the source
    /// side, `target_file_to_module` for the target) — a direct branch rather than
    /// a function pointer, so the analysis engine can resolve the call (the
    /// functional-core self-check rejects an unresolvable indirect call).
    ///
    /// `keep_pascal_types` is a **type**-name rule, so for a callable (function /
    /// method) it is disabled before normalizing: a PascalCase function name is a
    /// convention (`StringEnum` ↔ `string_enum`), not a kept-verbatim type, and
    /// must snake-fold like any other callable leaf so it doesn't false-miss.
    fn new(item: &'a ApiItem, rules: &NamingRules, is_source: bool) -> Self {
        let module = match item.file.as_deref() {
            Some(f) if is_source => source_file_to_module(f, rules),
            Some(f) => target_file_to_module(f, rules),
            None => String::new(),
        };
        let class = kind_class(&item.kind);
        let norm_name = if class == KindClass::Callable && rules.keep_pascal_types {
            let callable_rules = NamingRules {
                keep_pascal_types: false,
                ..rules.clone()
            };
            norm_leaf(&item.name, &callable_rules)
        } else {
            norm_leaf(&item.name, rules)
        };
        NormItem {
            norm_name,
            class,
            module,
            item,
        }
    }
}

/// Flatten a report's public items into normalized views, skipping `external:*`
/// modules (re-exported third-party surface, not the package's own declared
/// contract) and [`KindClass::Other`] items. `is_source` selects the side's
/// file→module lowering. Order follows the report's own deterministic sort.
fn flatten<'a>(report: &'a ApiReport, rules: &NamingRules, is_source: bool) -> Vec<NormItem<'a>> {
    let mut out = Vec::new();
    for module in &report.modules {
        if module.path.starts_with("external:") {
            continue;
        }
        for item in &module.items {
            let n = NormItem::new(item, rules, is_source);
            if n.class != KindClass::Other {
                out.push(n);
            }
        }
    }
    out
}

/// Compute a source→target public-surface conformance report.
///
/// `source` is the contract (the source package's public API), `target` the port
/// whose surface is graded against it, and `rules` the naming ruleset (reused from
/// the port config) that normalizes names and anchors modules. Both reports are
/// consumed read-only; the result is deterministic — the same inputs always
/// produce the same bytes.
pub fn build_api_diff(
    source: &ApiReport,
    target: &ApiReport,
    rules: &NamingRules,
) -> ApiDiffReport {
    let src_items = flatten(source, rules, true);
    let tgt_items = flatten(target, rules, false);

    // Index target items by normalized name (values in flatten order, so ranking
    // ties break deterministically).
    let mut by_name: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, t) in tgt_items.iter().enumerate() {
        by_name.entry(t.norm_name.as_str()).or_default().push(i);
    }
    let mut claimed = vec![false; tgt_items.len()];
    let same_lang = source.package.language == target.package.language;

    let mut items: Vec<ApiDiffItem> = Vec::with_capacity(src_items.len());
    for s in &src_items {
        match pick_match(s, &by_name, &tgt_items) {
            Some(ti) => {
                claimed[ti] = true;
                items.push(graded_item(s, &tgt_items[ti], same_lang, rules));
            }
            None => items.push(source_only_item(s, DiffStatus::Missing, None)),
        }
    }
    // Target items never chosen as a match are the extra (target-only) surface.
    for (i, t) in tgt_items.iter().enumerate() {
        if !claimed[i] {
            items.push(target_only_item(t));
        }
    }

    items.sort_by(|a, b| {
        a.status
            .rank()
            .cmp(&b.status.rank())
            .then_with(|| a.kind.cmp(&b.kind))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.source_id.cmp(&b.source_id))
            .then_with(|| a.target_id.cmp(&b.target_id))
    });

    let summary = summarize(&items);
    let by_kind = by_kind(&items);
    ApiDiffReport {
        hinzu_api_version: HINZU_API_VERSION,
        source: source.package.clone(),
        target: target.package.clone(),
        summary,
        by_kind,
        items,
    }
}

/// Pick the best target match for a source item: candidates are target items with
/// the same normalized name **and** the same [`KindClass`]; the best is the one in
/// the same anchored module (an exact-module match), else the shortest, then
/// lexically-first id — a deterministic, most-direct choice.
fn pick_match(
    s: &NormItem,
    by_name: &BTreeMap<&str, Vec<usize>>,
    tgt: &[NormItem],
) -> Option<usize> {
    let cands = by_name.get(s.norm_name.as_str())?;
    cands
        .iter()
        .copied()
        .filter(|&i| tgt[i].class == s.class)
        .min_by(|&a, &b| {
            let am = (tgt[a].module != s.module) as u8;
            let bm = (tgt[b].module != s.module) as u8;
            am.cmp(&bm)
                .then_with(|| tgt[a].item.id.len().cmp(&tgt[b].item.id.len()))
                .then_with(|| tgt[a].item.id.cmp(&tgt[b].item.id))
        })
}

/// Assemble one graded item. `src` / `tgt` supply the two evidence sides (id,
/// file, line), each `None` when that side is absent. The item's `name` + `kind`
/// come from whichever side is present (the source, or the target for an `extra`
/// item). The single place an [`ApiDiffItem`]'s eleven fields are written.
fn make_item(
    status: DiffStatus,
    src: Option<&ApiItem>,
    tgt: Option<&ApiItem>,
    mismatch: Option<Vec<MismatchAspect>>,
    note: Option<String>,
) -> ApiDiffItem {
    let ident = src.or(tgt).expect("a graded item has at least one side");
    ApiDiffItem {
        name: ident.name.clone(),
        kind: ident.kind.clone(),
        status,
        source_id: src.map(|i| i.id.clone()),
        source_file: src.and_then(|i| i.file.clone()),
        source_line: src.and_then(|i| i.line),
        target_id: tgt.map(|i| i.id.clone()),
        target_file: tgt.and_then(|i| i.file.clone()),
        target_line: tgt.and_then(|i| i.line),
        mismatch,
        note,
    }
}

/// Grade a matched pair: [`DiffStatus::Matched`] when the shapes are structurally
/// compatible, else [`DiffStatus::SignatureMismatch`] carrying the specific
/// differences. Records a cross-kind note when the two kinds differ within a class.
fn graded_item(s: &NormItem, t: &NormItem, same_lang: bool, rules: &NamingRules) -> ApiDiffItem {
    let aspects = compare_shape(s.item, t.item, same_lang, rules);
    let note = (s.item.kind != t.item.kind).then(|| format!("{}↔{}", s.item.kind, t.item.kind));
    let (status, mismatch) = if aspects.is_empty() {
        (DiffStatus::Matched, None)
    } else {
        (DiffStatus::SignatureMismatch, Some(aspects))
    };
    make_item(status, Some(s.item), Some(t.item), mismatch, note)
}

/// A source-only item (a [`DiffStatus::Missing`]) with no target side.
fn source_only_item(s: &NormItem, status: DiffStatus, note: Option<String>) -> ApiDiffItem {
    make_item(status, Some(s.item), None, None, note)
}

/// A target-only item (a [`DiffStatus::Extra`]) with no source side.
fn target_only_item(t: &NormItem) -> ApiDiffItem {
    make_item(DiffStatus::Extra, None, Some(t.item), None, None)
}

/// Compare a matched pair's shapes and return the structural differences (empty
/// when compatible). Structural facts drive the verdict: parameter arity for
/// callables, the field-name set for aggregates, the variant-name set for enums.
///
/// Field and variant names are normalized with `rules` before comparison, so a
/// pure convention rename (`maxTokens` ↔ `max_tokens`) is **not** flagged as a
/// missing/extra field — the aspect list carries the original spellings for a
/// difference that survives normalization. Rendered type strings are compared
/// **only** when the two sides share a language (`same_lang`); cross-language they
/// are omitted, since `string` ↔ `String` is not a real difference.
fn compare_shape(
    s: &ApiItem,
    t: &ApiItem,
    same_lang: bool,
    rules: &NamingRules,
) -> Vec<MismatchAspect> {
    let mut out = Vec::new();
    // Callables: arity is the language-agnostic structural signal.
    if let (Some(ss), Some(ts)) = (&s.signature, &t.signature) {
        if ss.params.len() != ts.params.len() {
            out.push(MismatchAspect {
                aspect: "paramCount".to_string(),
                source: ss.params.len().to_string(),
                target: ts.params.len().to_string(),
            });
        }
        if same_lang {
            for (i, (sp, tp)) in ss.params.iter().zip(ts.params.iter()).enumerate() {
                if sp.ty != tp.ty {
                    out.push(MismatchAspect {
                        aspect: format!("paramType[{i}]"),
                        source: sp.ty.clone(),
                        target: tp.ty.clone(),
                    });
                }
            }
            if ss.return_type != ts.return_type {
                out.push(MismatchAspect {
                    aspect: "returnType".to_string(),
                    source: ss.return_type.clone().unwrap_or_default(),
                    target: ts.return_type.clone().unwrap_or_default(),
                });
            }
        }
    }
    // Aggregates: the field-name set, when both sides carry named fields.
    if has_fields(&s.kind) && has_fields(&t.kind) {
        set_diff(
            &s.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            &t.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            "Field",
            rules,
            &mut out,
        );
    }
    // Enums: the variant-name set.
    if s.kind == "enum" && t.kind == "enum" {
        set_diff(
            &s.variants
                .iter()
                .map(|v| v.name.as_str())
                .collect::<Vec<_>>(),
            &t.variants
                .iter()
                .map(|v| v.name.as_str())
                .collect::<Vec<_>>(),
            "Variant",
            rules,
            &mut out,
        );
    }
    out
}

/// Push `missing<Facet>` / `extra<Facet>` aspects for names present on one side
/// only, comparing on the **normalized** name (so a convention rename is not a
/// difference) but reporting the original spelling. Preserves each side's
/// declaration order. Shared by the field-set and variant-set comparisons so both
/// diff identically.
fn set_diff(
    source: &[&str],
    target: &[&str],
    facet: &str,
    rules: &NamingRules,
    out: &mut Vec<MismatchAspect>,
) {
    // Normalize positionally (empty stays empty) so the spelling↔normalized zip
    // below stays aligned; empty (tuple/positional) names are skipped in the loop.
    let norm =
        |names: &[&str]| -> Vec<String> { names.iter().map(|n| norm_leaf(n, rules)).collect() };
    let src_norm = norm(source);
    let tgt_norm = norm(target);
    for (name, n) in source.iter().zip(src_norm.iter()) {
        if !name.is_empty() && !tgt_norm.contains(n) {
            out.push(MismatchAspect {
                aspect: format!("missing{facet}"),
                source: (*name).to_string(),
                target: String::new(),
            });
        }
    }
    for (name, n) in target.iter().zip(tgt_norm.iter()) {
        if !name.is_empty() && !src_norm.contains(n) {
            out.push(MismatchAspect {
                aspect: format!("extra{facet}"),
                source: String::new(),
                target: (*name).to_string(),
            });
        }
    }
}

/// Roll the graded items up into the summary counts + conformance grade.
fn summarize(items: &[ApiDiffItem]) -> DiffSummary {
    let mut s = DiffSummary::default();
    for it in items {
        match it.status {
            DiffStatus::Matched => s.matched += 1,
            DiffStatus::Missing => s.missing += 1,
            DiffStatus::SignatureMismatch => s.signature_mismatch += 1,
            DiffStatus::Extra => s.extra += 1,
        }
    }
    let denom = s.matched + s.missing + s.signature_mismatch;
    s.conformance = if denom > 0 {
        ((s.matched as f64 / denom as f64) * 1000.0).round() / 1000.0
    } else {
        0.0
    };
    s
}

/// The per-kind breakdown, one row per kind seen, sorted by kind.
fn by_kind(items: &[ApiDiffItem]) -> Vec<KindBreakdown> {
    let mut map: BTreeMap<&str, KindBreakdown> = BTreeMap::new();
    for it in items {
        let row = map
            .entry(it.kind.as_str())
            .or_insert_with(|| KindBreakdown {
                kind: it.kind.clone(),
                ..Default::default()
            });
        match it.status {
            DiffStatus::Matched => row.matched += 1,
            DiffStatus::Missing => row.missing += 1,
            DiffStatus::SignatureMismatch => row.signature_mismatch += 1,
            DiffStatus::Extra => row.extra += 1,
        }
    }
    map.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{build_api, ApiItem, Fidelity, Field, Module, Param, Signature, Variant};

    /// A neutral naming ruleset (the TS→Rust one from the port prototype) — pure
    /// data, so the tests never touch the filesystem.
    fn rules() -> NamingRules {
        crate::portdiff::PortDiffConfig::default_ts_rust().naming
    }

    /// Build a one-module report in the given language from raw items. Uses
    /// [`build_api`] so items are sorted exactly as a real report's are.
    fn report(language: &str, module: &str, items: Vec<ApiItem>) -> ApiReport {
        let package = PackageInfo {
            name: "demo".to_string(),
            language: language.to_string(),
            root: "demo".to_string(),
            version: None,
        };
        let fidelity = Fidelity {
            source: "test".to_string(),
            format_version: None,
            complete: false,
            notes: Vec::new(),
        };
        build_api(
            package,
            fidelity,
            vec![Module {
                path: module.to_string(),
                file: None,
                doc: None,
                items,
            }],
        )
    }

    /// A function item with the given name and parameter types (in a file so the
    /// module anchors), positional names left blank.
    fn func(
        id: &str,
        name: &str,
        module: &str,
        file: &str,
        params: &[&str],
        ret: Option<&str>,
    ) -> ApiItem {
        let mut it = ApiItem::new("function", id, name, module);
        it.file = Some(file.to_string());
        it.signature = Some(Signature {
            params: params
                .iter()
                .map(|ty| Param {
                    name: String::new(),
                    ty: (*ty).to_string(),
                    optional: false,
                    default: None,
                })
                .collect(),
            return_type: ret.map(str::to_string),
            is_async: false,
            receiver: None,
            error_type: None,
            generics: Vec::new(),
        });
        it
    }

    /// A named field with a rendered type.
    fn field(name: &str, ty: &str) -> Field {
        Field {
            name: name.to_string(),
            ty: ty.to_string(),
            visibility: "public".to_string(),
            doc: None,
            optional: false,
        }
    }

    /// An aggregate item (`kind` = struct/interface/…) carrying named fields.
    fn aggregate(kind: &str, id: &str, name: &str, module: &str, fields: Vec<Field>) -> ApiItem {
        let mut it = ApiItem::new(kind, id, name, module);
        it.fields = fields;
        it
    }

    /// Find the graded item for a source/target name.
    fn find<'a>(r: &'a ApiDiffReport, name: &str) -> &'a ApiDiffItem {
        r.items
            .iter()
            .find(|i| i.name == name)
            .expect("item present")
    }

    /// The graded item's mismatch aspects as `(aspect, source, target)` tuples.
    fn aspects_of(it: &ApiDiffItem) -> Vec<(&str, &str, &str)> {
        it.mismatch
            .as_ref()
            .expect("a mismatch carries aspects")
            .iter()
            .map(|a| (a.aspect.as_str(), a.source.as_str(), a.target.as_str()))
            .collect()
    }

    #[test]
    fn camel_case_source_matches_snake_case_target() {
        // A convention rename must NOT read as missing: `streamText` ↔ `stream_text`.
        let src = report(
            "typescript",
            "src/api/foo",
            vec![func(
                "s#streamText",
                "streamText",
                "src/api/foo",
                "src/api/foo.ts",
                &["string"],
                Some("void"),
            )],
        );
        let tgt = report(
            "rust",
            "crate::api::foo",
            vec![func(
                "crate::api::foo::stream_text",
                "stream_text",
                "crate::api::foo",
                "crates/x/src/api/foo.rs",
                &["String"],
                Some("()"),
            )],
        );
        let diff = build_api_diff(&src, &tgt, &rules());
        let it = find(&diff, "streamText");
        // Same arity, cross-language types not compared -> a clean match.
        assert_eq!(it.status, DiffStatus::Matched);
        assert_eq!(diff.summary.conformance, 1.0);
        assert_eq!(
            it.target_id.as_deref(),
            Some("crate::api::foo::stream_text")
        );
    }

    #[test]
    fn param_count_difference_is_a_signature_mismatch() {
        let src = report(
            "typescript",
            "m",
            vec![func(
                "s#f",
                "f",
                "m",
                "src/m.ts",
                &["string", "number"],
                None,
            )],
        );
        let tgt = report(
            "rust",
            "m",
            vec![func(
                "t::f",
                "f",
                "m",
                "crates/x/src/m.rs",
                &["String"],
                None,
            )],
        );
        let diff = build_api_diff(&src, &tgt, &rules());
        let it = find(&diff, "f");
        assert_eq!(it.status, DiffStatus::SignatureMismatch);
        let m = it.mismatch.as_ref().unwrap();
        assert_eq!(m[0].aspect, "paramCount");
        assert_eq!((m[0].source.as_str(), m[0].target.as_str()), ("2", "1"));
    }

    #[test]
    fn field_set_difference_is_a_signature_mismatch() {
        // interface ↔ struct with a dropped + an added field.
        let src = report(
            "typescript",
            "m",
            vec![aggregate(
                "interface",
                "s#Opt",
                "Opt",
                "m",
                vec![field("keep", "string"), field("dropped", "number")],
            )],
        );
        let tgt = report(
            "rust",
            "m",
            vec![aggregate(
                "struct",
                "t::Opt",
                "Opt",
                "m",
                vec![field("keep", "String"), field("added", "u32")],
            )],
        );
        let diff = build_api_diff(&src, &tgt, &rules());
        let it = find(&diff, "Opt");
        assert_eq!(it.status, DiffStatus::SignatureMismatch);
        // Cross-kind pairing recorded as evidence.
        assert_eq!(it.note.as_deref(), Some("interface↔struct"));
        let aspects = aspects_of(it);
        assert!(aspects.contains(&("missingField", "dropped", "")));
        assert!(aspects.contains(&("extraField", "", "added")));
        // The kept field is not flagged.
        assert!(!aspects.iter().any(|a| a.1 == "keep" || a.2 == "keep"));
    }

    #[test]
    fn camel_case_fields_match_snake_case_fields() {
        // A pure field convention rename must NOT read as missing/extra fields:
        // `maxTokens` ↔ `max_tokens`, `sessionId` ↔ `session_id`.
        let src = report(
            "typescript",
            "m",
            vec![aggregate(
                "interface",
                "s#Opt",
                "Opt",
                "m",
                vec![field("maxTokens", "number"), field("sessionId", "string")],
            )],
        );
        let tgt = report(
            "rust",
            "m",
            vec![aggregate(
                "struct",
                "t::Opt",
                "Opt",
                "m",
                vec![field("max_tokens", "u32"), field("session_id", "String")],
            )],
        );
        let diff = build_api_diff(&src, &tgt, &rules());
        // All fields align after normalization -> a clean match, not a mismatch.
        assert_eq!(find(&diff, "Opt").status, DiffStatus::Matched);
        assert_eq!(diff.summary.conformance, 1.0);
    }

    #[test]
    fn missing_and_extra_are_classified() {
        let src = report(
            "typescript",
            "m",
            vec![func(
                "s#only_source",
                "onlySource",
                "m",
                "src/m.ts",
                &[],
                None,
            )],
        );
        let tgt = report(
            "rust",
            "m",
            vec![func(
                "t::only_target",
                "only_target",
                "m",
                "crates/x/src/m.rs",
                &[],
                None,
            )],
        );
        let diff = build_api_diff(&src, &tgt, &rules());
        assert_eq!(find(&diff, "onlySource").status, DiffStatus::Missing);
        let extra = find(&diff, "only_target");
        assert_eq!(extra.status, DiffStatus::Extra);
        assert!(extra.source_id.is_none());
        assert_eq!(extra.target_id.as_deref(), Some("t::only_target"));
        // conformance = matched 0 / (0 + 1 missing + 0) = 0.
        assert_eq!(diff.summary.matched, 0);
        assert_eq!(diff.summary.missing, 1);
        assert_eq!(diff.summary.extra, 1);
        assert_eq!(diff.summary.conformance, 0.0);
    }

    #[test]
    fn same_language_compares_param_and_return_types() {
        // rust ↔ rust: rendered types ARE meaningful, so a type change is flagged.
        let src = report(
            "rust",
            "m",
            vec![func(
                "a::f",
                "f",
                "m",
                "crates/x/src/m.rs",
                &["u32"],
                Some("bool"),
            )],
        );
        let tgt = report(
            "rust",
            "m",
            vec![func(
                "b::f",
                "f",
                "m",
                "crates/x/src/m.rs",
                &["u64"],
                Some("String"),
            )],
        );
        let diff = build_api_diff(&src, &tgt, &rules());
        let it = find(&diff, "f");
        assert_eq!(it.status, DiffStatus::SignatureMismatch);
        let aspects: Vec<&str> = it
            .mismatch
            .as_ref()
            .unwrap()
            .iter()
            .map(|a| a.aspect.as_str())
            .collect();
        assert!(aspects.contains(&"paramType[0]"));
        assert!(aspects.contains(&"returnType"));
    }

    #[test]
    fn is_deterministic_and_sorted() {
        let src = report(
            "typescript",
            "m",
            vec![
                func("s#b", "b", "m", "src/m.ts", &[], None),
                func("s#a", "a", "m", "src/m.ts", &[], None),
                aggregate("interface", "s#Z", "Z", "m", vec![field("x", "string")]),
            ],
        );
        let tgt = report(
            "rust",
            "m",
            vec![
                func("t::a", "a", "m", "crates/x/src/m.rs", &[], None),
                func("t::extra", "extra", "m", "crates/x/src/m.rs", &[], None),
            ],
        );
        let d1 = build_api_diff(&src, &tgt, &rules());
        let d2 = build_api_diff(&src, &tgt, &rules());
        let bytes1 = serde_json::to_string(&d1).unwrap();
        let bytes2 = serde_json::to_string(&d2).unwrap();
        assert_eq!(bytes1, bytes2);
        // Items are sorted by (status rank, kind, name): matched first.
        let order: Vec<(&str, &str)> = d1
            .items
            .iter()
            .map(|i| (i.kind.as_str(), i.name.as_str()))
            .collect();
        // `a` matched (function), then missing `b` (function) and `Z` (interface),
        // then extra `extra`. Matched sorts ahead of missing which sorts ahead of extra.
        assert_eq!(order.first(), Some(&("function", "a")));
        assert_eq!(order.last(), Some(&("function", "extra")));
        assert!(d1.hinzu_api_version >= 1);
    }

    #[test]
    fn variant_set_difference_is_flagged() {
        let mut s_enum = ApiItem::new("enum", "s#E", "E", "m");
        s_enum.variants = vec![
            Variant {
                name: "A".to_string(),
                fields: vec![],
                discriminant: None,
                doc: None,
            },
            Variant {
                name: "Gone".to_string(),
                fields: vec![],
                discriminant: None,
                doc: None,
            },
        ];
        let mut t_enum = ApiItem::new("enum", "t::E", "E", "m");
        t_enum.variants = vec![
            Variant {
                name: "A".to_string(),
                fields: vec![],
                discriminant: None,
                doc: None,
            },
            Variant {
                name: "New".to_string(),
                fields: vec![],
                discriminant: None,
                doc: None,
            },
        ];
        let src = report("typescript", "m", vec![s_enum]);
        let tgt = report("rust", "m", vec![t_enum]);
        let diff = build_api_diff(&src, &tgt, &rules());
        let it = find(&diff, "E");
        assert_eq!(it.status, DiffStatus::SignatureMismatch);
        let aspects = aspects_of(it);
        assert!(aspects.contains(&("missingVariant", "Gone", "")));
        assert!(aspects.contains(&("extraVariant", "", "New")));
    }
}
