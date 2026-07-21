//! MIR **body-fact** extraction — the Rust-specific half of the range analysis.
//!
//! Where the call-graph collector (`main.rs`) emits definitions/edges/roots,
//! this pass lowers each monomorphized MIR `Body` into hinzu's language-agnostic
//! body-fact IR: per-function basic blocks, assignment statements, and
//! terminators, over integer/float/bool locals. The pure engine in
//! `hinzu_core::absint` consumes exactly this schema — a new language later
//! feeds the same schema from its own extractor.
//!
//! The schema structs below mirror `hinzu_core::absint::body` (kept in sync the
//! same way the `Facts` structs mirror `hinzu_core::facts`; the committed
//! `bodies.json` round-trip test fails on any drift). Only what the MVP models
//! is lowered faithfully; anything else becomes `Rvalue::Unknown` (the target
//! local goes to `Top`) or `Terminator::Other`, an honest over-approximation.
//!
//! Gated behind `HINZU_EMIT_BODIES=1` so the default extraction path (and every
//! stable CI job) is untouched.

use serde::Serialize;

use rustc_public::mir::{
    BinOp as MirBinOp, Body, ConstOperand, Operand as MirOperand, Place, Rvalue as MirRvalue,
    StatementKind, TerminatorKind, UnOp as MirUnOp,
};
use rustc_public::ty::{ConstantKind, FloatTy, RigidTy, Ty, TyKind};

/// Every function body extracted, serialized in the `hinzu_core::absint::body`
/// schema.
#[derive(Serialize, Default)]
pub struct BodyFacts {
    pub functions: Vec<FunctionBody>,
}

#[derive(Serialize)]
pub struct FunctionBody {
    pub id: String,
    pub display: String,
    pub file: String,
    pub line: u32,
    pub arg_count: usize,
    pub locals: Vec<Local>,
    pub blocks: Vec<Block>,
}

#[derive(Serialize)]
pub struct Local {
    pub kind: NumKind,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum NumKind {
    Int,
    Uint,
    Float,
    Bool,
    Other,
}

#[derive(Serialize)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub terminator: Terminator,
}

#[derive(Serialize, Default, Clone)]
pub struct Loc {
    pub file: String,
    pub line: u32,
    pub col: u32,
}

#[derive(Serialize)]
pub struct Stmt {
    pub place: u32,
    pub rvalue: Rvalue,
    pub loc: Loc,
}

#[derive(Serialize)]
#[serde(tag = "op")]
pub enum Rvalue {
    Use { operand: Operand },
    Binary { kind: BinOp, left: Operand, right: Operand },
    Unary { kind: UnOp, operand: Operand },
    Unknown,
}

#[derive(Serialize)]
#[serde(tag = "kind")]
pub enum Operand {
    Const { value: ConstVal },
    Local { local: u32 },
}

#[derive(Serialize)]
#[serde(tag = "ty", content = "v")]
pub enum ConstVal {
    Int(i64),
    Uint(u64),
    Float(f64),
    Bool(bool),
    Unknown,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Other,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum UnOp {
    Neg,
    Not,
    Other,
}

#[derive(Serialize, Clone, Copy)]
pub struct SwitchTarget {
    pub value: i64,
    pub block: u32,
}

#[derive(Serialize)]
#[serde(tag = "kind")]
pub enum Terminator {
    Goto {
        block: u32,
    },
    Return,
    Unreachable,
    SwitchInt {
        discr: Operand,
        targets: Vec<SwitchTarget>,
        otherwise: Option<u32>,
    },
    Assert {
        target: u32,
    },
    Call {
        destination: Option<u32>,
        target: Option<u32>,
    },
    Other {
        successors: Vec<u32>,
    },
}

/// The numeric kind of a local's type.
fn num_kind(ty: Ty) -> NumKind {
    match ty.kind() {
        TyKind::RigidTy(RigidTy::Int(_)) => NumKind::Int,
        TyKind::RigidTy(RigidTy::Uint(_)) => NumKind::Uint,
        TyKind::RigidTy(RigidTy::Float(_)) => NumKind::Float,
        TyKind::RigidTy(RigidTy::Bool) => NumKind::Bool,
        _ => NumKind::Other,
    }
}

/// Lower one MIR `Operand` to the schema. A `Copy`/`Move` of a bare local is a
/// local read; a projected place (`*p`, `s.field`) is not modeled, so it becomes
/// an `Unknown` constant (the reader treats it as `Top`). A constant is decoded
/// to its scalar value when possible; a runtime-checks operand is `Unknown`.
fn lower_operand(op: &MirOperand) -> Operand {
    match op {
        MirOperand::Copy(place) | MirOperand::Move(place) => place_local(place)
            .map(|local| Operand::Local { local })
            .unwrap_or(Operand::Const {
                value: ConstVal::Unknown,
            }),
        MirOperand::Constant(konst) => Operand::Const {
            value: lower_const(konst),
        },
        _ => Operand::Const {
            value: ConstVal::Unknown,
        },
    }
}

/// The local index of a projection-free place, if it is one.
fn place_local(place: &Place) -> Option<u32> {
    if place.projection.is_empty() {
        Some(place.local as u32)
    } else {
        None
    }
}

/// Decode a scalar constant (integer / unsigned / float / bool) to its value, or
/// `Unknown` when it is non-scalar or cannot be read. Reads the constant's
/// evaluated allocation directly, so no generic normalization is forced.
fn lower_const(konst: &ConstOperand) -> ConstVal {
    let ConstantKind::Allocated(alloc) = konst.const_.kind() else {
        return ConstVal::Unknown;
    };
    match konst.const_.ty().kind() {
        TyKind::RigidTy(RigidTy::Bool) => match alloc.read_bool() {
            Ok(b) => ConstVal::Bool(b),
            Err(_) => ConstVal::Unknown,
        },
        TyKind::RigidTy(RigidTy::Int(_)) => match alloc.read_int() {
            Ok(v) if fits_i64(v) => ConstVal::Int(v as i64),
            _ => ConstVal::Unknown,
        },
        TyKind::RigidTy(RigidTy::Uint(_)) => match alloc.read_uint() {
            Ok(v) if v <= u64::MAX as u128 => ConstVal::Uint(v as u64),
            _ => ConstVal::Unknown,
        },
        TyKind::RigidTy(RigidTy::Float(width)) => match alloc.read_uint() {
            Ok(bits) => decode_float(bits, width),
            Err(_) => ConstVal::Unknown,
        },
        _ => ConstVal::Unknown,
    }
}

/// Reinterpret a float constant's bit pattern as an `f64` value.
fn decode_float(bits: u128, width: FloatTy) -> ConstVal {
    match width {
        FloatTy::F32 => ConstVal::Float(f32::from_bits(bits as u32) as f64),
        FloatTy::F64 => ConstVal::Float(f64::from_bits(bits as u64)),
        _ => ConstVal::Unknown,
    }
}

/// Whether an `i128` fits an `i64`.
fn fits_i64(v: i128) -> bool {
    v >= i64::MIN as i128 && v <= i64::MAX as i128
}

/// Lower a MIR binary operator; unmodeled operators (bitwise, shifts, offset)
/// become `Other` (the reader yields `Top`).
fn lower_binop(op: MirBinOp) -> BinOp {
    match op {
        MirBinOp::Add | MirBinOp::AddUnchecked => BinOp::Add,
        MirBinOp::Sub | MirBinOp::SubUnchecked => BinOp::Sub,
        MirBinOp::Mul | MirBinOp::MulUnchecked => BinOp::Mul,
        MirBinOp::Div => BinOp::Div,
        MirBinOp::Rem => BinOp::Rem,
        MirBinOp::Eq => BinOp::Eq,
        MirBinOp::Ne => BinOp::Ne,
        MirBinOp::Lt => BinOp::Lt,
        MirBinOp::Le => BinOp::Le,
        MirBinOp::Gt => BinOp::Gt,
        MirBinOp::Ge => BinOp::Ge,
        _ => BinOp::Other,
    }
}

/// Lower a MIR unary operator.
fn lower_unop(op: MirUnOp) -> UnOp {
    match op {
        MirUnOp::Neg => UnOp::Neg,
        MirUnOp::Not => UnOp::Not,
        _ => UnOp::Other,
    }
}

/// Lower an rvalue the domain models; everything else is `Unknown`.
fn lower_rvalue(rvalue: &MirRvalue) -> Rvalue {
    match rvalue {
        MirRvalue::Use(op, _) => Rvalue::Use {
            operand: lower_operand(op),
        },
        MirRvalue::BinaryOp(op, l, r) | MirRvalue::CheckedBinaryOp(op, l, r) => Rvalue::Binary {
            kind: lower_binop(*op),
            left: lower_operand(l),
            right: lower_operand(r),
        },
        MirRvalue::UnaryOp(op, operand) => Rvalue::Unary {
            kind: lower_unop(*op),
            operand: lower_operand(operand),
        },
        _ => Rvalue::Unknown,
    }
}

/// Lower a terminator's control flow. An `Assert` (including the inserted
/// divide-by-zero check) is a plain edge to its success target — the engine
/// deliberately does not refine on it.
fn lower_terminator(kind: &TerminatorKind) -> Terminator {
    match kind {
        TerminatorKind::Goto { target } => Terminator::Goto {
            block: *target as u32,
        },
        TerminatorKind::Return => Terminator::Return,
        TerminatorKind::Unreachable => Terminator::Unreachable,
        TerminatorKind::SwitchInt { discr, targets } => {
            let switch_targets: Vec<SwitchTarget> = targets
                .branches()
                .map(|(value, block)| SwitchTarget {
                    value: value as i64,
                    block: block as u32,
                })
                .collect();
            Terminator::SwitchInt {
                discr: lower_operand(discr),
                targets: switch_targets,
                otherwise: Some(targets.otherwise() as u32),
            }
        }
        TerminatorKind::Assert { target, .. } => Terminator::Assert {
            target: *target as u32,
        },
        TerminatorKind::Call {
            destination,
            target,
            ..
        } => Terminator::Call {
            destination: place_local(destination),
            target: target.map(|t| t as u32),
        },
        // Drop, and anything else with successors, is taken conservatively.
        TerminatorKind::Drop { target, .. } => Terminator::Other {
            successors: vec![*target as u32],
        },
        _ => Terminator::Other {
            successors: Vec::new(),
        },
    }
}

/// Lower one function body to the schema, or `None` if it has no blocks.
pub fn lower_body(id: &str, file: &str, line: u32, body: &Body) -> Option<FunctionBody> {
    if body.blocks.is_empty() {
        return None;
    }
    let locals: Vec<Local> = body
        .locals()
        .iter()
        .map(|decl| Local {
            kind: num_kind(decl.ty),
        })
        .collect();

    let blocks: Vec<Block> = body
        .blocks
        .iter()
        .map(|bb| {
            let stmts: Vec<Stmt> = bb
                .statements
                .iter()
                .filter_map(lower_statement)
                .collect();
            Block {
                stmts,
                terminator: lower_terminator(&bb.terminator.kind),
            }
        })
        .collect();

    Some(FunctionBody {
        id: id.to_string(),
        display: id.to_string(),
        file: file.to_string(),
        line,
        arg_count: body.arg_locals().len(),
        locals,
        blocks,
    })
}

/// Lower one statement. Only assignments are modeled: a bare-local target keeps
/// its rvalue; a projected target is soundly havocked (`Unknown` rvalue on the
/// base local). Non-assignment statements are dropped (they do not change a
/// modeled local's value).
fn lower_statement(stmt: &rustc_public::mir::Statement) -> Option<Stmt> {
    let StatementKind::Assign(place, rvalue) = &stmt.kind else {
        return None;
    };
    let (base_local, is_projected) = (place.local as u32, !place.projection.is_empty());
    let rvalue = if is_projected {
        Rvalue::Unknown
    } else {
        lower_rvalue(rvalue)
    };
    let (file, line, col) = span_loc(stmt.source_info.span);
    Some(Stmt {
        place: base_local,
        rvalue,
        loc: Loc { file, line, col },
    })
}

/// A span's file, 1-based start line, and start column.
fn span_loc(span: rustc_public::ty::Span) -> (String, u32, u32) {
    let lines = span.get_lines();
    (
        span.get_filename(),
        lines.start_line as u32,
        lines.start_col as u32,
    )
}
