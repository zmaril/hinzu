//! Target-agnostic helpers shared by the `hinzu model` emitters. Both the Quint
//! lowering ([`quint`](super::quint)) and the Stateright lowering
//! ([`stateright`](super::stateright)) walk the same [`body`](super::body) IR and
//! need the same primitives — the collision-free per-function name key, the
//! `<fnkey>_l<localid>` variable name, the local-role annotation, the modeled
//! binary-operator symbol, the constant rendering, the straight-line test, and
//! the CFG-summary terminator description. Factoring them here keeps the two
//! emitters honest twins instead of copy-paste clones (and the duplication gate
//! green): a fix to the naming scheme or the CFG summary lands in one place and
//! both backends inherit it.
//!
//! Purity: string building only, like the rest of `hinzu-core` — no filesystem,
//! network, environment, clock, or process access; it allocates and nothing more.

use super::body::{BinOp, ConstVal, FunctionBody, Terminator};

/// A sanitized, collision-free name stem for a function: every non-alphanumeric
/// character of its symbol id becomes `_`. Both emitters key their per-function
/// state names and actions off this, so a symbol like `app::ratio` maps to the
/// same `app__ratio` stem in the Quint model and the Stateright model.
pub(crate) fn fn_key(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// The per-local variable name for local `idx` of the function keyed `key`:
/// `<key>_l<idx>`. Shared so the Quint `var` and the Stateright struct field name
/// the same slot identically.
pub(crate) fn var_name(key: &str, idx: usize) -> String {
    format!("{key}_l{idx}")
}

/// How local `idx` is used, for a source comment: the return place (0), a
/// parameter (`1..=arg_count`), or a temporary. The MIR local numbering
/// convention, kept language-agnostic.
pub(crate) fn local_role(idx: usize, arg_count: usize) -> &'static str {
    if idx == 0 {
        "return place"
    } else if idx <= arg_count {
        "param"
    } else {
        "temp"
    }
}

/// The infix operator symbol for a modeled binary op, or `None` for `Other`
/// (which becomes a hole). Quint and Rust spell the modeled arithmetic and
/// comparison operators identically (`+ - * / % == != < <= > >=`), so both
/// emitters share this mapping; only the un-modeled `Other` differs by needing a
/// per-target hole.
pub(crate) fn binop_symbol(kind: BinOp) -> Option<&'static str> {
    Some(match kind {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::Other => return None,
    })
}

/// Render a scalar constant to a literal both target languages accept: integers
/// and booleans lower directly; a float or an unmodelable constant becomes a `0`
/// carrying an inline `AGENT-TODO` for the abstraction choice (an `/* … */`
/// comment, legal in Quint and Rust alike).
pub(crate) fn lower_const(value: &ConstVal) -> String {
    match value {
        ConstVal::Int(v) => v.to_string(),
        ConstVal::Uint(v) => v.to_string(),
        ConstVal::Bool(v) => v.to_string(),
        ConstVal::Float(_) => {
            "0 /* AGENT-TODO: float constant — choose an abstraction */".to_string()
        }
        ConstVal::Unknown => "0 /* AGENT-TODO: unknown constant — choose a value */".to_string(),
    }
}

/// Whether a function is a single block ending in `Return` — the case that lowers
/// directly, with no CFG hole. Anything richer (a branch, a loop, an `Assert`, or
/// a `Call` continuation) is surfaced as a CFG summary by both emitters.
pub(crate) fn is_straight_line(func: &FunctionBody) -> bool {
    func.blocks.len() == 1 && matches!(func.blocks[0].terminator, Terminator::Return)
}

/// A one-line human description of a terminator for the CFG-summary comment both
/// emitters print when a function is not straight-line.
pub(crate) fn describe_terminator(term: &Terminator) -> String {
    match term {
        Terminator::Goto { block } => format!("Goto -> block {block}"),
        Terminator::Return => "Return".to_string(),
        Terminator::Unreachable => "Unreachable".to_string(),
        Terminator::SwitchInt {
            targets, otherwise, ..
        } => {
            let mut cases: Vec<String> = targets
                .iter()
                .map(|t| format!("{}=>{}", t.value, t.block))
                .collect();
            if let Some(o) = otherwise {
                cases.push(format!("else=>{o}"));
            }
            format!("SwitchInt [{}]", cases.join(", "))
        }
        Terminator::Assert { target } => format!("Assert -> block {target}"),
        Terminator::Call {
            destination,
            target,
        } => {
            let dest = destination
                .map(|d| format!("dest _{d}"))
                .unwrap_or_else(|| "no dest".to_string());
            let tgt = target
                .map(|t| format!("-> block {t}"))
                .unwrap_or_else(|| "diverging".to_string());
            format!("Call ({dest}) {tgt}")
        }
        Terminator::Other { successors } => {
            let succ: Vec<String> = successors.iter().map(|s| s.to_string()).collect();
            format!("Other -> [{}]", succ.join(", "))
        }
    }
}
