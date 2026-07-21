//! The generic worklist abstract interpreter — language-agnostic, driven only by
//! the [`body`](super::body) IR. Per function it runs a forward fixed-point over
//! the control-flow graph: join the states on a block's incoming edges, transfer
//! each statement through the [`interval`](super::interval) domain, and at a
//! re-visited block **widen** so a loop reaches its fixed point in bounded
//! rounds. Branch edges are **refined** — a `SwitchInt` on a comparison result
//! (or a bare integer local) narrows the tested local per edge, the
//! excluded-point / branch-narrowing trick that lets `if c != 0 { .. / c }`
//! discharge the divisor.
//!
//! The engine computes each block's fixed-point entry state; hazard detection
//! (`hazards.rs`) then does one pass over those states. Deliberately excluded
//! from refinement: the compiler-inserted divide-by-zero [`Terminator::Assert`]
//! — refining on it would prove every division safe. See the module docs there.

use std::collections::{BTreeMap, VecDeque};

use super::body::{
    BinOp, Block, FunctionBody, LocalId, NumKind, Operand, Rvalue, Stmt, Terminator, UnOp,
};
use super::hazards::{HazardKind, RawHazard};
use super::interval::{self, AbstractNumber};

/// Start widening a block once it has been re-queued this many times, so a
/// growing loop bound is pushed to the extreme instead of crawling up forever.
const WIDEN_AFTER: u32 = 3;

/// A hard backstop on how many times one block is re-processed. Widening
/// guarantees convergence well below this; the cap only bounds pathological
/// input. (freerange caps loop-header updates at 16 for the same reason.)
const MAX_VISITS: u32 = 64;

/// The abstract state at a program point: one [`AbstractNumber`] per local.
type State = Vec<AbstractNumber>;

/// A comparison `local <op> constant` recorded while transferring a block, so a
/// `SwitchInt` on the boolean it produced can refine `local` per edge.
#[derive(Clone, Copy)]
struct Guard {
    local: LocalId,
    op: BinOp,
    constant: f64,
}

/// The fixed-point result for one function: the entry state of every reachable
/// block (an unreachable block is `None`), plus the block list for the hazard
/// pass to walk.
pub struct FunctionAnalysis<'a> {
    pub body: &'a FunctionBody,
    pub block_in: Vec<Option<State>>,
}

/// Run the worklist fixed-point over one function body.
pub fn analyze_function(body: &FunctionBody) -> FunctionAnalysis<'_> {
    let n = body.blocks.len();
    let mut block_in: Vec<Option<State>> = vec![None; n];
    let mut visits: Vec<u32> = vec![0; n];

    if n == 0 {
        return FunctionAnalysis { body, block_in };
    }
    block_in[0] = Some(initial_state(body));

    let mut worklist: VecDeque<usize> = VecDeque::new();
    worklist.push_back(0);

    while let Some(b) = worklist.pop_front() {
        let Some(in_state) = block_in[b].clone() else {
            continue;
        };
        let (out_state, guards) = transfer_block(body, &body.blocks[b], in_state);
        for (succ, edge_state) in successor_states(&body.blocks[b].terminator, &out_state, &guards)
        {
            let succ = succ as usize;
            if succ >= n {
                continue;
            }
            let merged = match &block_in[succ] {
                None => edge_state,
                Some(existing) => {
                    let joined = join_states(existing, &edge_state);
                    if visits[succ] >= WIDEN_AFTER {
                        widen_states(existing, &joined)
                    } else {
                        joined
                    }
                }
            };
            let changed = block_in[succ]
                .as_ref()
                .is_none_or(|existing| !states_equal(existing, &merged));
            if changed && visits[succ] < MAX_VISITS {
                block_in[succ] = Some(merged);
                visits[succ] += 1;
                if !worklist.contains(&succ) {
                    worklist.push_back(succ);
                }
            } else if block_in[succ].is_none() {
                // First reach even at the cap: record it so the block is live.
                block_in[succ] = Some(merged);
            }
        }
    }

    FunctionAnalysis { body, block_in }
}

/// The domain-level summary of one function: the inferred parameter and return
/// ranges plus the hazards found. `mod.rs` turns this into the report schema.
pub struct Summary {
    /// The range each parameter was analyzed with (locals `1..=arg_count`).
    pub param_ranges: Vec<AbstractNumber>,
    /// The range the return value can hold (joined over every `Return`).
    pub return_range: AbstractNumber,
    /// The evidence-carrying hazards found, in block-then-statement order.
    pub hazards: Vec<RawHazard>,
}

/// Run the fixed-point, then walk each reachable block once over its fixed-point
/// entry state to collect divide-/remainder-by-zero hazards and the return
/// range. Hazards are checked at the `Div`/`Rem` statement using the divisor
/// range the program produced (the divide-by-zero assert is not refined on).
pub fn function_summary(body: &FunctionBody) -> Summary {
    let analysis = analyze_function(body);

    let param_ranges: Vec<AbstractNumber> = analysis
        .block_in
        .first()
        .and_then(|s| s.as_ref())
        .map(|entry| {
            (1..=body.arg_count)
                .filter_map(|i| entry.get(i).copied())
                .collect()
        })
        .unwrap_or_default();

    let mut hazards: Vec<RawHazard> = Vec::new();
    let mut return_range: Option<AbstractNumber> = None;

    for (b, block) in body.blocks.iter().enumerate() {
        let Some(in_state) = analysis.block_in.get(b).and_then(|s| s.clone()) else {
            continue; // unreachable block — no false hazards from dead code
        };
        let mut state = in_state;
        let mut guards: BTreeMap<LocalId, Guard> = BTreeMap::new();
        for stmt in &block.stmts {
            if let Some(raw) = divisor_hazard(&state, stmt) {
                hazards.push(raw);
            }
            apply_stmt(&mut state, stmt, &mut guards);
        }
        if matches!(block.terminator, Terminator::Return) {
            let ret = state
                .first()
                .copied()
                .unwrap_or_else(AbstractNumber::unknown);
            return_range = Some(match return_range {
                None => ret,
                Some(prev) => interval::join(&prev, &ret),
            });
        }
    }

    Summary {
        param_ranges,
        return_range: return_range.unwrap_or_else(AbstractNumber::unknown),
        hazards,
    }
}

/// If `stmt` is an integer `Div`/`Rem` whose divisor range may be zero, the
/// hazard it proves. The divisor is read in the pre-statement state.
fn divisor_hazard(state: &State, stmt: &Stmt) -> Option<RawHazard> {
    let Rvalue::Binary { kind, right, .. } = &stmt.rvalue else {
        return None;
    };
    let hazard_kind = match kind {
        BinOp::Div => HazardKind::DivideByZero,
        BinOp::Rem => HazardKind::RemainderByZero,
        _ => return None,
    };
    let divisor = eval_operand(state, right);
    // Integer division/remainder by zero panics; a float divisor produces
    // Infinity/NaN instead and is not this hazard.
    if divisor.integer && divisor.includes_zero() {
        Some(RawHazard {
            kind: hazard_kind,
            loc: stmt.loc.clone(),
            divisor,
        })
    } else {
        None
    }
}

/// The entry state: each parameter starts at the widest value its kind allows;
/// the return place and temporaries start at `Top` (they are assigned before
/// use in valid MIR).
fn initial_state(body: &FunctionBody) -> State {
    body.locals
        .iter()
        .enumerate()
        .map(|(i, local)| {
            let is_param = i >= 1 && i <= body.arg_count;
            if is_param {
                start_for_kind(local.kind)
            } else {
                AbstractNumber::unknown()
            }
        })
        .collect()
}

/// The widest domain value for a numeric kind at a function boundary.
fn start_for_kind(kind: NumKind) -> AbstractNumber {
    match kind {
        NumKind::Int | NumKind::Uint => AbstractNumber::any_integer(),
        NumKind::Float => AbstractNumber::finite_input(),
        // A boolean is a 0/1 integer.
        NumKind::Bool => AbstractNumber {
            excluded: None,
            ..constant_pair(0.0, 1.0)
        },
        NumKind::Other => AbstractNumber::unknown(),
    }
}

/// An integer interval `[lower, upper]`.
fn constant_pair(lower: f64, upper: f64) -> AbstractNumber {
    let mut base = AbstractNumber::any_integer();
    base.lower = lower;
    base.upper = upper;
    base
}

/// Transfer a whole block: apply every statement to a working copy of the
/// incoming state, returning the outgoing state and the comparison guards live
/// at the terminator (built block-locally, consumed by [`successor_states`]).
fn transfer_block(
    body: &FunctionBody,
    block: &Block,
    mut state: State,
) -> (State, BTreeMap<LocalId, Guard>) {
    let mut guards: BTreeMap<LocalId, Guard> = BTreeMap::new();
    for stmt in &block.stmts {
        apply_stmt(&mut state, stmt, &mut guards);
    }
    let _ = body;
    (state, guards)
}

/// Apply one statement to `state`, updating the block-local comparison guards.
fn apply_stmt(state: &mut State, stmt: &Stmt, guards: &mut BTreeMap<LocalId, Guard>) {
    let place = stmt.place;
    // Any guard whose recorded value is now overwritten is stale.
    guards.remove(&place);
    guards.retain(|_, g| g.local != place);

    let value = eval_rvalue(state, &stmt.rvalue, place, guards);
    if let Some(slot) = state.get_mut(place as usize) {
        *slot = value;
    }
}

/// Evaluate an rvalue to a domain value, recording a comparison guard for
/// `place` when the rvalue is a comparison against a constant.
fn eval_rvalue(
    state: &State,
    rvalue: &Rvalue,
    place: LocalId,
    guards: &mut BTreeMap<LocalId, Guard>,
) -> AbstractNumber {
    match rvalue {
        Rvalue::Use { operand } => eval_operand(state, operand),
        Rvalue::Unary { kind, operand } => {
            let v = eval_operand(state, operand);
            match kind {
                UnOp::Neg => negate(&v),
                UnOp::Not | UnOp::Other => AbstractNumber::unknown(),
            }
        }
        Rvalue::Binary { kind, left, right } => {
            let l = eval_operand(state, left);
            let r = eval_operand(state, right);
            if kind.is_comparison() {
                if let Some(guard) = extract_guard(*kind, left, right) {
                    guards.insert(place, guard);
                }
                // A comparison yields a boolean 0/1.
                return constant_pair(0.0, 1.0);
            }
            match kind {
                BinOp::Add => interval::add(&l, &r),
                BinOp::Sub => interval::sub(&l, &r),
                BinOp::Mul => interval::mul(&l, &r),
                BinOp::Div => {
                    let quotient = interval::div(&l, &r);
                    // Integer division truncates toward zero and stays an integer
                    // (Rust `i64/i64`), unlike the domain's float division (JS
                    // `/`). On the non-panicking path `|a/b| <= |a|`, so the
                    // quotient is a finite integer bounded by the dividend.
                    if l.integer && r.integer {
                        integer_quotient(&l, &quotient)
                    } else {
                        quotient
                    }
                }
                BinOp::Rem => interval::rem(&l, &r, !r.includes_zero()),
                BinOp::Other => AbstractNumber::unknown(),
                // comparisons handled above
                _ => AbstractNumber::unknown(),
            }
        }
        Rvalue::Unknown => AbstractNumber::unknown(),
    }
}

/// Read an operand's domain value.
fn eval_operand(state: &State, operand: &Operand) -> AbstractNumber {
    match operand {
        Operand::Const { value } => {
            use super::body::ConstVal::*;
            match value {
                Int(i) => AbstractNumber::constant(*i as f64),
                Uint(u) => AbstractNumber::constant(*u as f64),
                Float(f) => AbstractNumber::constant(*f),
                Bool(b) => AbstractNumber::constant(if *b { 1.0 } else { 0.0 }),
                Unknown => AbstractNumber::unknown(),
            }
        }
        Operand::Local { local } => state
            .get(*local as usize)
            .copied()
            .unwrap_or_else(AbstractNumber::unknown),
    }
}

/// The result of integer division: an integer whose magnitude is at most the
/// dividend's, and never `NaN`/infinite on the non-panicking path.
fn integer_quotient(dividend: &AbstractNumber, quotient: &AbstractNumber) -> AbstractNumber {
    let magnitude = dividend.lower.abs().max(dividend.upper.abs());
    AbstractNumber {
        lower: quotient.lower.max(-magnitude),
        upper: quotient.upper.min(magnitude),
        integer: true,
        may_be_nan: false,
        excluded: None,
    }
}

/// Exact negation: `[-upper, -lower]`, keeping the integer flag and flipping the
/// excluded point's sign.
fn negate(value: &AbstractNumber) -> AbstractNumber {
    AbstractNumber {
        lower: -value.upper,
        upper: -value.lower,
        integer: value.integer,
        may_be_nan: value.may_be_nan,
        excluded: value.excluded.map(|p| -p),
    }
}

/// Build a guard from a comparison, normalizing to `local <op> constant`. A
/// `constant <op> local` is flipped; two locals or two constants yield no guard.
fn extract_guard(op: BinOp, left: &Operand, right: &Operand) -> Option<Guard> {
    match (left, right) {
        (Operand::Local { local }, Operand::Const { value }) => Some(Guard {
            local: *local,
            op,
            constant: const_f64(value)?,
        }),
        (Operand::Const { value }, Operand::Local { local }) => Some(Guard {
            local: *local,
            op: flip_comparison(op),
            constant: const_f64(value)?,
        }),
        _ => None,
    }
}

/// The numeric value of a scalar constant, if it has one.
fn const_f64(value: &super::body::ConstVal) -> Option<f64> {
    use super::body::ConstVal::*;
    match value {
        Int(i) => Some(*i as f64),
        Uint(u) => Some(*u as f64),
        Float(f) => Some(*f),
        Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Unknown => None,
    }
}

/// Swap the sides of a comparison operator: `a < b` is `b > a`, etc.
fn flip_comparison(op: BinOp) -> BinOp {
    match op {
        BinOp::Lt => BinOp::Gt,
        BinOp::Le => BinOp::Ge,
        BinOp::Gt => BinOp::Lt,
        BinOp::Ge => BinOp::Le,
        other => other, // Eq, Ne are symmetric
    }
}

/// The refined states on each outgoing edge of a terminator.
fn successor_states(
    terminator: &Terminator,
    out_state: &State,
    guards: &BTreeMap<LocalId, Guard>,
) -> Vec<(LocalId, State)> {
    match terminator {
        Terminator::Goto { block } | Terminator::Assert { target: block } => {
            vec![(*block, out_state.clone())]
        }
        Terminator::Return | Terminator::Unreachable => vec![],
        Terminator::Call {
            destination,
            target,
        } => {
            let Some(target) = target else {
                return vec![];
            };
            // Interprocedural range propagation is the documented follow-up; for
            // now a call havocks its destination.
            let mut state = out_state.clone();
            if let Some(dest) = destination {
                if let Some(slot) = state.get_mut(*dest as usize) {
                    *slot = AbstractNumber::unknown();
                }
            }
            vec![(*target, state)]
        }
        Terminator::Other { successors } => {
            successors.iter().map(|s| (*s, out_state.clone())).collect()
        }
        Terminator::SwitchInt {
            discr,
            targets,
            otherwise,
        } => switch_states(discr, targets, otherwise, out_state, guards),
    }
}

/// Per-edge refined states for a `SwitchInt`. When the discriminant is a
/// comparison result, each edge assumes that comparison true/false and narrows
/// the compared local; when it is a bare integer local, each `value` edge pins
/// the local to that value and the `otherwise` edge excludes a single listed
/// value.
fn switch_states(
    discr: &Operand,
    targets: &[super::body::SwitchTarget],
    otherwise: &Option<LocalId>,
    out_state: &State,
    guards: &BTreeMap<LocalId, Guard>,
) -> Vec<(LocalId, State)> {
    let discr_local = match discr {
        Operand::Local { local } => Some(*local),
        Operand::Const { .. } => None,
    };
    let guard = discr_local.and_then(|l| guards.get(&l).copied());
    let mut edges: Vec<(LocalId, State)> = Vec::new();

    for target in targets {
        let mut state = out_state.clone();
        if let Some(guard) = guard {
            // switchInt on a boolean: value 0 is false, any other value true.
            let assume_true = target.value != 0;
            refine_local(
                &mut state,
                guard.local,
                guard.op,
                guard.constant,
                assume_true,
            );
        } else if let Some(local) = discr_local {
            // A bare integer switch: this edge pins the local to target.value.
            pin_local(&mut state, local, target.value as f64);
        }
        edges.push((target.block, state));
    }

    if let Some(other) = otherwise {
        let mut state = out_state.clone();
        if let Some(guard) = guard {
            // The otherwise edge is the complement of the listed values. With a
            // boolean guard the listed value is 0 (false), so otherwise is true.
            let listed_only_false = targets.iter().all(|t| t.value == 0);
            if listed_only_false {
                refine_local(&mut state, guard.local, guard.op, guard.constant, true);
            }
        } else if let Some(local) = discr_local {
            // otherwise = the local is none of the listed values; refine only the
            // common single-value case (`switchInt(x) -> [0: .., otherwise: ..]`).
            if targets.len() == 1 {
                exclude_local(&mut state, local, targets[0].value as f64);
            }
        }
        edges.push((*other, state));
    }

    edges
}

/// Narrow `local` in `state` assuming `local <op> constant` holds (or its
/// negation when `assume_true` is false).
fn refine_local(state: &mut State, local: LocalId, op: BinOp, constant: f64, assume_true: bool) {
    let Some(current) = state.get(local as usize).copied() else {
        return;
    };
    let effective = if assume_true {
        op
    } else {
        negate_comparison(op)
    };
    let refined = apply_comparison(&current, effective, constant);
    if let Some(slot) = state.get_mut(local as usize) {
        *slot = refined;
    }
}

/// The logical negation of a comparison operator.
fn negate_comparison(op: BinOp) -> BinOp {
    match op {
        BinOp::Eq => BinOp::Ne,
        BinOp::Ne => BinOp::Eq,
        BinOp::Lt => BinOp::Ge,
        BinOp::Le => BinOp::Gt,
        BinOp::Gt => BinOp::Le,
        BinOp::Ge => BinOp::Lt,
        other => other,
    }
}

/// Apply `value <op> constant` as a refinement of `value`.
fn apply_comparison(value: &AbstractNumber, op: BinOp, k: f64) -> AbstractNumber {
    let mut out = *value;
    let int = value.integer;
    match op {
        BinOp::Eq => pin(&mut out, k),
        BinOp::Ne => exclude(&mut out, k),
        BinOp::Lt => {
            let bound = if int { k - 1.0 } else { interval::next_down(k) };
            out.upper = out.upper.min(bound);
        }
        BinOp::Le => out.upper = out.upper.min(k),
        BinOp::Gt => {
            let bound = if int { k + 1.0 } else { interval::next_up(k) };
            out.lower = out.lower.max(bound);
        }
        BinOp::Ge => out.lower = out.lower.max(k),
        _ => {}
    }
    out
}

/// Pin a local to an exact value on a switch edge that selects it.
fn pin_local(state: &mut State, local: LocalId, value: f64) {
    if let Some(slot) = state.get_mut(local as usize) {
        pin(slot, value);
    }
}

/// Exclude a single value from a local on a switch's otherwise edge.
fn exclude_local(state: &mut State, local: LocalId, value: f64) {
    if let Some(slot) = state.get_mut(local as usize) {
        exclude(slot, value);
    }
}

/// Intersect a value with the single point `k` (the `== k` refinement).
fn pin(value: &mut AbstractNumber, k: f64) {
    if k >= value.lower && k <= value.upper {
        value.lower = k;
        value.upper = k;
        value.excluded = None;
    }
    // A `k` outside the range is a contradiction (the edge is dead); leaving the
    // value unchanged is sound (over-approximate) and simple.
}

/// Cut the single point `k` out of a value (the `!= k` refinement), tightening a
/// boundary or recording the excluded point for an interior cut.
fn exclude(value: &mut AbstractNumber, k: f64) {
    if k < value.lower || k > value.upper {
        return; // already excluded by the bounds
    }
    let step = if value.integer { 1.0 } else { 0.0 };
    if k == value.lower {
        value.lower = if value.integer {
            k + 1.0
        } else {
            interval::next_up(k)
        };
    } else if k == value.upper {
        value.upper = if value.integer {
            k - 1.0
        } else {
            interval::next_down(k)
        };
    } else {
        value.excluded = Some(k);
    }
    let _ = step;
}

/// Elementwise join of two equal-length states.
fn join_states(a: &State, b: &State) -> State {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| interval::join(x, y))
        .collect()
}

/// Elementwise widen of `next` against `previous`.
fn widen_states(previous: &State, next: &State) -> State {
    previous
        .iter()
        .zip(next.iter())
        .map(|(p, n)| interval::widen(p, n))
        .collect()
}

/// Whether two states are structurally equal (the fixed-point stop test).
fn states_equal(a: &State, b: &State) -> bool {
    a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x.same(y))
}

#[cfg(test)]
mod tests {
    use super::super::body::*;
    use super::*;

    /// `local(kind)` — a numeric local.
    fn local(kind: NumKind) -> Local {
        Local { kind }
    }

    /// An `int`-typed local.
    fn int() -> Local {
        local(NumKind::Int)
    }

    /// A `Local` operand.
    fn op_local(l: LocalId) -> Operand {
        Operand::Local { local: l }
    }

    /// An integer-constant operand.
    fn op_int(v: i64) -> Operand {
        Operand::Const {
            value: ConstVal::Int(v),
        }
    }

    /// An assignment `place = rvalue` with a default location.
    fn stmt(place: LocalId, rvalue: Rvalue) -> Stmt {
        Stmt {
            place,
            rvalue,
            loc: Loc::default(),
        }
    }

    /// A `place = left <op> right` binary statement.
    fn binary_stmt(place: LocalId, kind: BinOp, left: Operand, right: Operand) -> Stmt {
        stmt(place, Rvalue::Binary { kind, left, right })
    }

    /// A block with the given statements and terminator.
    fn block(stmts: Vec<Stmt>, terminator: Terminator) -> Block {
        Block { stmts, terminator }
    }

    /// An empty `Return` block.
    fn ret_block() -> Block {
        block(vec![], Terminator::Return)
    }

    /// A `switchInt(discr) -> [0: zero_block, otherwise: else_block]` terminator.
    fn switch_zero(discr: LocalId, zero_block: BlockId, otherwise: BlockId) -> Terminator {
        Terminator::SwitchInt {
            discr: op_local(discr),
            targets: vec![SwitchTarget {
                value: 0,
                block: zero_block,
            }],
            otherwise: Some(otherwise),
        }
    }

    /// A one-block function: `params` integer params, `stmts`, and a `Return`.
    fn straight_line(params: usize, locals: Vec<Local>, stmts: Vec<Stmt>) -> FunctionBody {
        FunctionBody {
            id: "t".into(),
            display: "t".into(),
            file: "t.rs".into(),
            line: 1,
            arg_count: params,
            locals,
            blocks: vec![block(stmts, Terminator::Return)],
        }
    }

    fn use_local(l: LocalId) -> Rvalue {
        Rvalue::Use {
            operand: op_local(l),
        }
    }

    /// A function with explicit locals and blocks (id/file are placeholders).
    fn func(arg_count: usize, locals: Vec<Local>, blocks: Vec<Block>) -> FunctionBody {
        FunctionBody {
            id: "t".into(),
            display: "t".into(),
            file: "t.rs".into(),
            line: 1,
            arg_count,
            locals,
            blocks,
        }
    }

    #[test]
    fn a_parameter_starts_at_its_kind_width() {
        // fn(a: i64) { _0 = a }
        let f = straight_line(1, vec![int(), int()], vec![stmt(0, use_local(1))]);
        let a = analyze_function(&f);
        let entry = a.block_in[0].as_ref().unwrap();
        assert!(entry[1].integer);
        assert!(entry[1].includes_zero());
    }

    #[test]
    fn a_switch_on_ne_zero_excludes_zero_on_the_true_edge() {
        // bb0: _2 = Ne(_1, 0); switchInt(_2) -> [0: bb1(else), otherwise: bb2(then)]
        // bb1: return ; bb2: return
        let f = func(
            1,
            vec![int(), int(), local(NumKind::Bool)],
            vec![
                block(
                    vec![binary_stmt(2, BinOp::Ne, op_local(1), op_int(0))],
                    switch_zero(2, 1, 2),
                ),
                ret_block(),
                ret_block(),
            ],
        );
        let a = analyze_function(&f);
        // then-block (bb2): _1 excludes zero.
        let then_in = a.block_in[2].as_ref().unwrap();
        assert!(!then_in[1].includes_zero());
        // else-block (bb1): _1 pinned to zero.
        let else_in = a.block_in[1].as_ref().unwrap();
        assert!(else_in[1].is_definitely_zero());
    }

    #[test]
    fn a_self_loop_converges_by_widening() {
        // bb0: _1 = 0; goto bb1
        // bb1: _1 = _1 + 1; switchInt(_2) -> [0: bb2, otherwise: bb1]  (an
        //      unbounded counter) — must terminate via widening.
        let f = func(
            0,
            vec![int(), int(), local(NumKind::Bool)],
            vec![
                block(
                    vec![stmt(1, Rvalue::Use { operand: op_int(0) })],
                    Terminator::Goto { block: 1 },
                ),
                block(
                    vec![binary_stmt(1, BinOp::Add, op_local(1), op_int(1))],
                    switch_zero(2, 2, 1),
                ),
                ret_block(),
            ],
        );
        // Termination is the assertion: analyze_function returns.
        let a = analyze_function(&f);
        let loop_in = a.block_in[1].as_ref().unwrap();
        // The counter widened upward to an unbounded positive range.
        assert!(loop_in[1].upper.is_infinite() || loop_in[1].upper >= f64::MAX);
    }
}
