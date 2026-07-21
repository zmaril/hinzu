//! The language-agnostic **body-fact IR** the abstract-interpretation engine
//! consumes — a small control-flow graph of per-function basic blocks,
//! statements, and terminators, expressed with no reference to any source
//! language. A Rust MIR extractor (the StableMIR driver) emits this schema as
//! JSON today; adding a new language later means writing a new extractor that
//! produces the same `BodyFacts`, feeding the same engine. Nothing below this
//! line knows what produced the facts.
//!
//! The schema is deliberately minimal — the smallest honest slice that supports
//! integer interval analysis and divide-by-zero detection: integer/float/bool
//! locals, constants, `Binary`/`Unary` rvalues, assignment statements, and the
//! terminators a straight-line-plus-branch CFG needs. Anything an extractor
//! cannot model faithfully it emits as [`Rvalue::Unknown`] (the target local
//! becomes `Top`) or [`Terminator::Other`] (successors taken conservatively) —
//! an over-approximation, never a faked value.

use serde::{Deserialize, Serialize};

/// A local variable slot. Local `0` is the return place; `1..=arg_count` are the
/// parameters; the rest are temporaries — the MIR numbering convention, kept
/// language-agnostic here.
pub type LocalId = u32;

/// A basic-block index. Block `0` is the entry.
pub type BlockId = u32;

/// The numeric nature of a local, as far as the domain cares. `Int`/`Uint` are
/// integer-typed (the domain's `integer` flag is set); `Float` is real-valued;
/// `Bool` is a 0/1 integer; `Other` is anything the domain does not model (it
/// stays `Top`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NumKind {
    Int,
    Uint,
    Float,
    Bool,
    Other,
}

/// Every function body an extractor produced for one analysis run.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BodyFacts {
    #[serde(default)]
    pub functions: Vec<FunctionBody>,
}

impl BodyFacts {
    /// Parse a body-fact set from the JSON an extractor emits.
    pub fn from_json(json: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(json)?)
    }
}

/// One function's control-flow graph.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FunctionBody {
    /// The callable's stable symbol id (matches the `FactSet` definition id).
    pub id: String,
    /// A short human-readable name.
    pub display: String,
    /// The defining file and 1-based start line, for report provenance.
    pub file: String,
    pub line: u32,
    /// How many of the locals after the return place are parameters:
    /// `locals[1..=arg_count]`.
    pub arg_count: usize,
    /// Locals indexed by [`LocalId`]; index `0` is the return place.
    pub locals: Vec<Local>,
    /// Basic blocks indexed by [`BlockId`]; index `0` is the entry.
    pub blocks: Vec<Block>,
}

/// A local variable's modeled numeric kind.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Local {
    pub kind: NumKind,
}

/// A basic block: a straight-line run of statements ended by one terminator.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Block {
    #[serde(default)]
    pub stmts: Vec<Stmt>,
    pub terminator: Terminator,
}

/// A source location, for hazard evidence.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Loc {
    pub file: String,
    pub line: u32,
    pub col: u32,
}

/// An assignment `place = rvalue`. Only assignments to a bare local are modeled;
/// an extractor that meets an assignment through a projection (`*p = ..`,
/// `s.field = ..`) emits the base local with [`Rvalue::Unknown`] so the base is
/// soundly havocked rather than silently kept.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stmt {
    pub place: LocalId,
    pub rvalue: Rvalue,
    #[serde(default)]
    pub loc: Loc,
}

/// The right-hand side of an assignment the domain can evaluate.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum Rvalue {
    /// A copy/move of an operand.
    Use { operand: Operand },
    /// A binary arithmetic or comparison operation.
    Binary {
        kind: BinOp,
        left: Operand,
        right: Operand,
    },
    /// A unary operation (negation, logical not).
    Unary { kind: UnOp, operand: Operand },
    /// Anything the extractor could not model — the target local becomes `Top`.
    Unknown,
}

/// A binary-operation operand: a constant or a local read.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Operand {
    Const {
        value: ConstVal,
    },
    /// A read of a local (a MIR `Copy` or `Move` of a bare local).
    Local {
        local: LocalId,
    },
}

/// A scalar constant. Integer literals arrive as `Int`/`Uint`; a literal too
/// large to model, or a non-scalar constant, is `Unknown` (`Top`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "ty", content = "v")]
pub enum ConstVal {
    Int(i64),
    Uint(u64),
    Float(f64),
    Bool(bool),
    Unknown,
}

/// The binary operators the domain has transfer functions for. `Other` is any
/// operator the domain does not model (bitwise, shifts) — its result is `Top`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

impl BinOp {
    /// Whether this is a comparison operator (produces a boolean, and drives
    /// branch refinement).
    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
        )
    }
}

/// The unary operators the domain models. `Other` yields `Top`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UnOp {
    Neg,
    Not,
    Other,
}

/// One `(value, block)` case of a `SwitchInt`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct SwitchTarget {
    /// The discriminant value that takes this edge, as a signed integer (switch
    /// values are small in practice — booleans, enum tags, guard results).
    pub value: i64,
    pub block: BlockId,
}

/// A block's terminator: how control leaves the block.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Terminator {
    Goto {
        block: BlockId,
    },
    Return,
    Unreachable,
    /// A multi-way branch on a discriminant. When `discr` is a boolean produced
    /// by a comparison, or a bare integer local, the engine refines that local
    /// per edge — the excluded-point / branch-narrowing trick.
    SwitchInt {
        discr: Operand,
        targets: Vec<SwitchTarget>,
        otherwise: Option<BlockId>,
    },
    /// A compiler-inserted runtime check (bounds, overflow, **divide-by-zero**).
    /// The engine treats it as `Goto { target }` and deliberately does **not**
    /// refine on its condition — refining on the divide-by-zero assert would
    /// prove every division safe and defeat the analysis. See `hazards.rs`.
    Assert {
        target: BlockId,
    },
    /// A function call. Havocks its destination local (interprocedural range
    /// propagation is the documented follow-up — this node carries that seam),
    /// then continues to `target` if it returns.
    Call {
        destination: Option<LocalId>,
        target: Option<BlockId>,
    },
    /// Any other terminator: its successors are taken conservatively with no
    /// refinement.
    Other {
        successors: Vec<BlockId>,
    },
}

impl Terminator {
    /// The blocks control can flow to from here — used by the worklist to know
    /// the CFG successors independent of refinement.
    pub fn successors(&self) -> Vec<BlockId> {
        match self {
            Terminator::Goto { block } => vec![*block],
            Terminator::Return | Terminator::Unreachable => vec![],
            Terminator::SwitchInt {
                targets, otherwise, ..
            } => {
                let mut out: Vec<BlockId> = targets.iter().map(|t| t.block).collect();
                out.extend(otherwise.iter().copied());
                out
            }
            Terminator::Assert { target } => vec![*target],
            Terminator::Call { target, .. } => target.iter().copied().collect(),
            Terminator::Other { successors } => successors.clone(),
        }
    }
}
