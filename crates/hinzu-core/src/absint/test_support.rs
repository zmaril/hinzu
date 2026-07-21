//! Shared body-fact builders for the `absint` unit tests. Constructing a
//! [`FunctionBody`](super::body::FunctionBody) CFG by hand is verbose, and the
//! engine tests (`mod.rs`) and the Quint-emitter tests (`quint.rs`) need the same
//! two shapes — a straight-line binary op and a guarded divide. Factoring the
//! constructors here keeps that setup in one place (and the duplication gate
//! green). Test-only.

use super::body::*;

/// A one-block `id(a, b) { _0 = a <op> b; return }` over two integer params plus
/// the return place — the straight-line shape both test suites exercise.
pub(crate) fn binop_fn(id: &str, op: BinOp) -> FunctionBody {
    FunctionBody {
        id: id.into(),
        display: id.into(),
        file: "demo.rs".into(),
        line: 1,
        arg_count: 2,
        locals: vec![
            Local { kind: NumKind::Int }, // _0 return
            Local { kind: NumKind::Int }, // _1 = a
            Local { kind: NumKind::Int }, // _2 = b
        ],
        blocks: vec![Block {
            stmts: vec![Stmt {
                place: 0,
                rvalue: Rvalue::Binary {
                    kind: op,
                    left: Operand::Local { local: 1 },
                    right: Operand::Local { local: 2 },
                },
                loc: Loc {
                    file: "demo.rs".into(),
                    line: 2,
                    col: 5,
                },
            }],
            terminator: Terminator::Return,
        }],
    }
}

/// A guarded divide `fn safe(a, b) { if b != 0 { _0 = a / b } else { _0 = 0 } }`,
/// as the four-block CFG MIR produces:
///   bb0: _3 = Ne(_2, 0); switchInt(_3) -> [0: bb2(else), otherwise: bb1(then)]
///   bb1: _0 = _1 / _2; goto bb3
///   bb2: _0 = 0; goto bb3
///   bb3: return
/// The engine proves the divide safe; the emitter surfaces the `SwitchInt` as a
/// CFG `AGENT-TODO`. Same shape, two consumers.
pub(crate) fn guarded_divide_fn() -> FunctionBody {
    FunctionBody {
        id: "app::safe".into(),
        display: "safe".into(),
        file: "demo.rs".into(),
        line: 1,
        arg_count: 2,
        locals: vec![
            Local { kind: NumKind::Int }, // _0
            Local { kind: NumKind::Int }, // _1 = a
            Local { kind: NumKind::Int }, // _2 = b
            Local {
                kind: NumKind::Bool,
            }, // _3 = b != 0
        ],
        blocks: vec![
            Block {
                stmts: vec![Stmt {
                    place: 3,
                    rvalue: Rvalue::Binary {
                        kind: BinOp::Ne,
                        left: Operand::Local { local: 2 },
                        right: Operand::Const {
                            value: ConstVal::Int(0),
                        },
                    },
                    loc: Loc::default(),
                }],
                terminator: Terminator::SwitchInt {
                    discr: Operand::Local { local: 3 },
                    targets: vec![SwitchTarget { value: 0, block: 2 }],
                    otherwise: Some(1),
                },
            },
            Block {
                stmts: vec![Stmt {
                    place: 0,
                    rvalue: Rvalue::Binary {
                        kind: BinOp::Div,
                        left: Operand::Local { local: 1 },
                        right: Operand::Local { local: 2 },
                    },
                    loc: Loc {
                        file: "demo.rs".into(),
                        line: 3,
                        col: 9,
                    },
                }],
                terminator: Terminator::Goto { block: 3 },
            },
            Block {
                stmts: vec![Stmt {
                    place: 0,
                    rvalue: Rvalue::Use {
                        operand: Operand::Const {
                            value: ConstVal::Int(0),
                        },
                    },
                    loc: Loc::default(),
                }],
                terminator: Terminator::Goto { block: 3 },
            },
            Block {
                stmts: vec![],
                terminator: Terminator::Return,
            },
        ],
    }
}
