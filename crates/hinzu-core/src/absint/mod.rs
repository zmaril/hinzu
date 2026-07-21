//! Freerange-style numeric range / abstract-interpretation analysis, as a
//! **pure, language-agnostic core**. Nothing here does I/O or knows any source
//! language: it consumes the [`body`] IR (a control-flow graph of basic blocks
//! an extractor produced) and computes, per function, the interval each value
//! can hold — then reports **hazards** (integer divide-by-zero today) as
//! evidence-carrying facts.
//!
//! The pieces:
//! - [`interval`] — the `AbstractNumber` domain (interval + integer flag +
//!   may-be-NaN + one excluded point; IEEE-754-exact float arithmetic).
//! - [`body`] — the language-agnostic body-fact IR the engine consumes.
//! - [`engine`] — the generic worklist abstract interpreter with widening and
//!   branch refinement.
//! - [`hazards`] — hazard detection and the deterministic JSON report.
//!
//! A new language later = a new extractor emitting [`body::BodyFacts`]; the
//! engine and domain are reused unchanged. This module is the reusable core the
//! Rust MIR extractor (the StableMIR driver) feeds — the architecture the whole
//! feature is built to prove.

pub mod body;
mod emit_common;
pub mod engine;
pub mod hazards;
pub mod interval;
pub mod quint;
pub mod stateright;

#[cfg(test)]
pub(crate) mod test_support;

use body::BodyFacts;
use hazards::{FunctionRanges, Hazard, ParamRange, RangesReport, HINZU_RANGES_VERSION};
pub use quint::emit_quint;
pub use stateright::emit_stateright;

/// Analyze every function in a body-fact set and produce the deterministic
/// ranges-and-hazards report. Pure: no I/O, no ordering dependence — functions
/// are sorted by symbol id and hazards by location.
pub fn analyze_bodies(facts: &BodyFacts) -> RangesReport {
    let mut functions: Vec<FunctionRanges> = Vec::new();
    let mut hazards: Vec<Hazard> = Vec::new();

    for body in &facts.functions {
        let summary = engine::function_summary(body);

        let parameters = summary
            .param_ranges
            .iter()
            .enumerate()
            .map(|(i, range)| ParamRange {
                // local index is 1-based (local 0 is the return place)
                local: (i + 1) as u32,
                range: hazards::describe(range),
            })
            .collect();

        functions.push(FunctionRanges {
            id: body.id.clone(),
            display: body.display.clone(),
            file: body.file.clone(),
            line: body.line,
            parameters,
            returns: hazards::describe(&summary.return_range),
        });

        for raw in &summary.hazards {
            hazards.push(Hazard::from_raw(&body.id, raw));
        }
    }

    functions.sort_by(|a, b| a.id.cmp(&b.id));
    hazards.sort_by(|a, b| {
        a.function
            .cmp(&b.function)
            .then(a.line.cmp(&b.line))
            .then(a.column.cmp(&b.column))
            .then(a.kind.as_str().cmp(b.kind.as_str()))
    });

    RangesReport {
        hinzu_ranges_version: HINZU_RANGES_VERSION,
        functions,
        hazards,
    }
}

#[cfg(test)]
mod tests {
    use super::body::*;
    use super::test_support::{binop_fn, guarded_divide_fn};
    use super::*;

    #[test]
    fn an_unguarded_integer_divide_is_flagged() {
        let facts = BodyFacts {
            functions: vec![binop_fn("app::ratio", BinOp::Div)],
        };
        let report = analyze_bodies(&facts);
        assert_eq!(report.hazards.len(), 1);
        assert_eq!(report.hazards[0].kind, hazards::HazardKind::DivideByZero);
        assert_eq!(report.hazards[0].function, "app::ratio");
        assert_eq!(report.hazards[0].line, 2);
    }

    #[test]
    fn an_unguarded_integer_remainder_is_flagged() {
        let facts = BodyFacts {
            functions: vec![binop_fn("app::modulo", BinOp::Rem)],
        };
        let report = analyze_bodies(&facts);
        assert_eq!(report.hazards.len(), 1);
        assert_eq!(report.hazards[0].kind, hazards::HazardKind::RemainderByZero);
    }

    #[test]
    fn a_divide_by_a_nonzero_constant_is_not_flagged() {
        // fn f(a) { _0 = a / 2; return }
        let f = FunctionBody {
            id: "app::half".into(),
            display: "half".into(),
            file: "demo.rs".into(),
            line: 1,
            arg_count: 1,
            locals: vec![Local { kind: NumKind::Int }, Local { kind: NumKind::Int }],
            blocks: vec![Block {
                stmts: vec![Stmt {
                    place: 0,
                    rvalue: Rvalue::Binary {
                        kind: BinOp::Div,
                        left: Operand::Local { local: 1 },
                        right: Operand::Const {
                            value: ConstVal::Int(2),
                        },
                    },
                    loc: Loc::default(),
                }],
                terminator: Terminator::Return,
            }],
        };
        let report = analyze_bodies(&BodyFacts { functions: vec![f] });
        assert!(report.hazards.is_empty());
    }

    #[test]
    fn a_guarded_integer_divide_is_not_flagged() {
        // fn safe(a, b) { if b != 0 { _0 = a / b } else { _0 = 0 }; return }
        let report = analyze_bodies(&BodyFacts {
            functions: vec![guarded_divide_fn()],
        });
        assert!(
            report.hazards.is_empty(),
            "guarded divide should not be flagged, got {:?}",
            report.hazards
        );
    }
}
