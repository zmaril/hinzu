//! Shared body-fact builders for the `absint` unit tests. Constructing a
//! [`FunctionBody`](super::body::FunctionBody) CFG by hand is verbose, and the
//! engine tests (`mod.rs`) and the two model-emitter test suites (`quint.rs`,
//! `stateright.rs`) need the same handful of shapes — a straight-line binary op,
//! a guarded divide, an environment-nondeterminism function, a single-local
//! function of a chosen kind — plus the same CFG-summary assertion. Factoring
//! the constructors (and that shared assertion) here keeps the setup in one place
//! and the duplication gate green, since the two emitter suites would otherwise
//! be token-for-token clones. Test-only.

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

/// A one-block `env() { _0 = <unknown>; return }` whose single statement is an
/// [`Rvalue::Unknown`] — the environment-nondeterminism shape both emitter suites
/// assert becomes an `AGENT-TODO` hole.
pub(crate) fn unknown_rvalue_fn() -> FunctionBody {
    FunctionBody {
        id: "app::env".into(),
        display: "env".into(),
        file: "demo.rs".into(),
        line: 1,
        arg_count: 0,
        locals: vec![Local { kind: NumKind::Int }],
        blocks: vec![Block {
            stmts: vec![Stmt {
                place: 0,
                rvalue: Rvalue::Unknown,
                loc: Loc::default(),
            }],
            terminator: Terminator::Return,
        }],
    }
}

/// A zero-statement, single-local function of the given numeric `kind` — used to
/// assert a `Float`/`Other` local surfaces the target's abstraction hole.
pub(crate) fn single_local_fn(kind: NumKind) -> FunctionBody {
    FunctionBody {
        id: "app::f".into(),
        display: "f".into(),
        file: "demo.rs".into(),
        line: 1,
        arg_count: 0,
        locals: vec![Local { kind }],
        blocks: vec![Block {
            stmts: vec![],
            terminator: Terminator::Return,
        }],
    }
}

/// Assert the CFG-summary holes a [`guarded_divide_fn`] skeleton must carry in any
/// target: the four-block summary, the control-flow `AGENT-TODO`, and the
/// SwitchInt terminator named in the summary. Each emitter test adds its own
/// block-0 lowering assertion on top, so the shared part lives here rather than
/// being cloned across the two suites.
pub(crate) fn assert_guarded_cfg_summary(out: &str) {
    assert!(
        out.contains("// ---- CFG (4 blocks) ----"),
        "multi-block body should surface a CFG summary;\n{out}"
    );
    assert!(
        out.contains("AGENT-TODO: encode control flow"),
        "CFG should carry a control-flow hole;\n{out}"
    );
    assert!(
        out.contains("SwitchInt"),
        "the SwitchInt terminator should appear in the CFG summary;\n{out}"
    );
}
