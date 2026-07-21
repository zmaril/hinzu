//! Hazard detection and the deterministic report the `hinzu ranges` subcommand
//! emits. A hazard is an **evidence-carrying** fact: which function, which
//! source location, why — the divisor range that proves it.
//!
//! # Integer divide-by-zero / remainder-by-zero
//!
//! The one hazard the MVP detects, and the cleanest to defend over MIR. rustc
//! inserts a runtime **divide-by-zero assert** before an integer `Div`/`Rem`; on
//! the assert's success edge the divisor is guaranteed nonzero. The engine
//! deliberately does **not** refine on that assert (see `engine.rs` /
//! `Terminator::Assert`), so the divisor range at the operation is the range the
//! program actually produced. A divisor whose integer range still includes zero
//! there means the inserted panic-assert is **reachable**: this division can
//! panic at runtime with divide-by-zero. A user-level guard (`if c != 0 { .. }`)
//! compiles to a `SwitchInt` the engine *does* refine, so a guarded divide has a
//! zero-excluded divisor and is correctly not flagged.
//!
//! Float division by zero yields `Infinity`/`NaN` rather than a panic, so it is
//! not a divide-by-zero *hazard*; the domain still tracks the resulting
//! `may_be_nan` / non-finiteness for a later NaN/Infinity hazard (deferred).

use serde::Serialize;

use super::body::Loc;
use super::interval::AbstractNumber;

/// The schema version of the `hinzu ranges` JSON report.
pub const HINZU_RANGES_VERSION: u32 = 1;

/// What kind of hazard was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HazardKind {
    /// An integer division whose divisor range may be zero.
    DivideByZero,
    /// An integer remainder whose divisor range may be zero.
    RemainderByZero,
}

impl HazardKind {
    /// The report/JSON spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            HazardKind::DivideByZero => "divide-by-zero",
            HazardKind::RemainderByZero => "remainder-by-zero",
        }
    }

    /// The operation word for the human-readable message.
    fn operation(self) -> &'static str {
        match self {
            HazardKind::DivideByZero => "division",
            HazardKind::RemainderByZero => "remainder",
        }
    }
}

/// A hazard found in a function, still carrying the domain value that proves it
/// (converted to a display string for the report).
pub struct RawHazard {
    pub kind: HazardKind,
    pub loc: Loc,
    pub divisor: AbstractNumber,
}

/// The full deterministic report: per-function ranges plus the hazards found.
#[derive(Clone, Debug, Serialize)]
pub struct RangesReport {
    pub hinzu_ranges_version: u32,
    /// Functions analyzed, sorted by symbol id.
    pub functions: Vec<FunctionRanges>,
    /// Hazards found, sorted by (function, line, column, kind).
    pub hazards: Vec<Hazard>,
}

/// The inferred ranges for one function.
#[derive(Clone, Debug, Serialize)]
pub struct FunctionRanges {
    pub id: String,
    pub display: String,
    pub file: String,
    pub line: u32,
    /// The range each parameter is analyzed with, in declaration order.
    pub parameters: Vec<ParamRange>,
    /// The range the function's return value can hold.
    pub returns: String,
}

/// One parameter's analyzed range.
#[derive(Clone, Debug, Serialize)]
pub struct ParamRange {
    /// The MIR local index of the parameter (`1..=arg_count`).
    pub local: u32,
    pub range: String,
}

/// One reported hazard, with its evidence.
#[derive(Clone, Debug, Serialize)]
pub struct Hazard {
    /// The symbol id of the function the hazard is in.
    pub function: String,
    pub kind: HazardKind,
    pub file: String,
    pub line: u32,
    pub column: u32,
    /// A human-readable explanation.
    pub message: String,
    /// The divisor range that proves the hazard — the evidence.
    pub divisor_range: String,
}

impl Hazard {
    /// Build a reportable hazard from a raw finding in `function`.
    pub fn from_raw(function: &str, raw: &RawHazard) -> Hazard {
        let divisor_range = describe(&raw.divisor);
        let message = format!(
            "integer {} can panic: the divisor may be zero (range: {})",
            raw.kind.operation(),
            divisor_range
        );
        Hazard {
            function: function.to_string(),
            kind: raw.kind,
            file: raw.loc.file.clone(),
            line: raw.loc.line,
            column: raw.loc.col,
            message,
            divisor_range,
        }
    }
}

/// A freerange-style human-readable description of a domain value — what the
/// `requires:`/`ensures:` lines and hazard evidence print.
pub fn describe(value: &AbstractNumber) -> String {
    // A claim-free full range with NaN is genuinely unknown.
    if value.may_be_nan && value.lower == f64::NEG_INFINITY && value.upper == f64::INFINITY {
        return "unknown".to_string();
    }
    let mut out = String::new();
    if value.integer {
        out.push_str("integer ");
    }
    if value.lower == f64::NEG_INFINITY && value.upper == f64::INFINITY {
        out.push_str("(any)");
    } else {
        out.push_str(&format!(
            "in [{}, {}]",
            format_bound(value.lower),
            format_bound(value.upper)
        ));
    }
    if value.excluded == Some(0.0) {
        out.push_str(" excluding 0");
    } else if let Some(p) = value.excluded {
        out.push_str(&format!(" excluding {}", format_bound(p)));
    }
    if value.may_be_nan {
        out.push_str(", may be NaN");
    }
    // Only a real (float) value can be literally ±Infinity. An integer's ±inf
    // bounds are just the unbounded representation, not an infinite inhabitant.
    if !value.integer && value.may_be_infinite() {
        out.push_str(", may be infinite");
    }
    out
}

/// Format one interval bound: `-inf`/`inf` for infinities, a plain integer when
/// whole, otherwise the shortest round-trip float.
fn format_bound(value: f64) -> String {
    if value == f64::INFINITY {
        return "inf".to_string();
    }
    if value == f64::NEG_INFINITY {
        return "-inf".to_string();
    }
    if value == f64::MAX {
        return "max".to_string();
    }
    if value == -f64::MAX {
        return "-max".to_string();
    }
    if value.fract() == 0.0 && value.abs() < 1e15 {
        return format!("{}", value as i64);
    }
    format!("{value}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_reads_a_bounded_integer() {
        let mut v = AbstractNumber::any_integer();
        v.lower = 1.0;
        v.upper = 10.0;
        assert_eq!(describe(&v), "integer in [1, 10]");
    }

    #[test]
    fn describe_marks_an_excluded_zero() {
        let mut v = AbstractNumber::any_integer();
        v.excluded = Some(0.0);
        assert_eq!(describe(&v), "integer (any) excluding 0");
    }

    #[test]
    fn describe_calls_a_full_nan_range_unknown() {
        assert_eq!(describe(&AbstractNumber::unknown()), "unknown");
    }

    #[test]
    fn hazard_message_carries_the_divisor_evidence() {
        let raw = RawHazard {
            kind: HazardKind::DivideByZero,
            loc: Loc {
                file: "d.rs".into(),
                line: 3,
                col: 5,
            },
            divisor: AbstractNumber::any_integer(),
        };
        let h = Hazard::from_raw("app::ratio", &raw);
        assert_eq!(h.kind, HazardKind::DivideByZero);
        assert_eq!(h.line, 3);
        assert!(h.message.contains("divisor may be zero"));
        assert_eq!(h.divisor_range, "integer (any)");
    }
}
