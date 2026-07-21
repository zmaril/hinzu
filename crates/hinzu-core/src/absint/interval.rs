//! The numeric abstract domain — a Rust port of freerange's `AbstractNumber`
//! lattice (`src/domain/number.ts`), adapted to Rust idiom.
//!
//! A value is one continuous interval `[lower, upper]` over IEEE-754 doubles,
//! plus:
//! - an `integer` flag (every finite inhabitant is a whole number),
//! - a `may_be_nan` flag (the value can be `NaN`),
//! - at most one `excluded` point cut out of the interval — set by a `!= c`
//!   guard, where no interval endpoint can express the hole. Division consumes a
//!   zero exclusion directly, and the arithmetic rules forward an exclusion into
//!   a zero exclusion through the same float-exact inversions requirement peeling
//!   trusts (`x != 4` makes `x - 4 != 0`).
//!
//! Finiteness is **structural**, exactly as in freerange: a value that can be
//! `±Infinity` carries that infinity as a bound rather than a separate flag, so
//! [`AbstractNumber::may_be_infinite`] is derived from the bounds. Bounds are
//! always real numbers or `±Infinity`, never `NaN` (NaN-ness lives in the flag);
//! every producer maintains that invariant.
//!
//! Float arithmetic is IEEE-754-exact: overflow-to-`Infinity` and `NaN` are
//! tracked separately and soundly, and strict comparisons refine to the adjacent
//! representable double via [`next_up`] / [`next_down`]. Reasoning over reals is
//! deliberately rejected — floating-point is not associative, rounds, and
//! overflows, so real-number reasoning would produce *false* proofs.

/// The smallest positive subnormal double (`5e-324`) — the result of
/// [`next_up`] at zero. Rust's `f64::MIN_POSITIVE` is the smallest *normal*
/// double, so we build the subnormal from its bit pattern.
const SMALLEST_SUBNORMAL: f64 = f64::from_bits(1);

/// The adjacent representable double above `value` — the exact refinement for a
/// strict float comparison: a runtime `x > b` implies `x >= next_up(b)`, and no
/// double sits between them.
pub fn next_up(value: f64) -> f64 {
    if value.is_nan() || value == f64::INFINITY {
        return value;
    }
    if value == 0.0 {
        return SMALLEST_SUBNORMAL;
    }
    let bits = value.to_bits() as i64;
    let next = if value > 0.0 { bits + 1 } else { bits - 1 };
    f64::from_bits(next as u64)
}

/// The adjacent representable double below `value`.
pub fn next_down(value: f64) -> f64 {
    -next_up(-value)
}

/// One interval value in the domain. See the module docs for the invariants.
#[derive(Clone, Copy, Debug)]
pub struct AbstractNumber {
    pub lower: f64,
    pub upper: f64,
    pub integer: bool,
    pub may_be_nan: bool,
    pub excluded: Option<f64>,
}

impl AbstractNumber {
    /// `[lower, upper]` with no excluded point.
    fn plain(lower: f64, upper: f64, integer: bool, may_be_nan: bool) -> Self {
        AbstractNumber {
            lower,
            upper,
            integer,
            may_be_nan,
            excluded: None,
        }
    }

    /// The claim-free full range with the `NaN` flag on — the honest cover when
    /// an operation's result cannot be reasoned about.
    pub fn unknown() -> Self {
        AbstractNumber::plain(f64::NEG_INFINITY, f64::INFINITY, false, true)
    }

    /// A single constant value.
    pub fn constant(value: f64) -> Self {
        AbstractNumber::plain(
            value,
            value,
            value.is_finite() && value.fract() == 0.0,
            value.is_nan(),
        )
    }

    /// The full integer range: any whole number, no `NaN`. The starting value of
    /// an integer parameter whose range is otherwise unknown. (Integer bounds
    /// beyond 2^53 are approximated by `±Infinity` — a sound over-approximation
    /// that never hides a divide-by-zero; it only widens the interval.)
    pub fn any_integer() -> Self {
        AbstractNumber::plain(f64::NEG_INFINITY, f64::INFINITY, true, false)
    }

    /// The full finite real range with no `NaN` — a float parameter assumed
    /// finite at the boundary (freerange's `finiteInputNumber`).
    pub fn finite_input() -> Self {
        AbstractNumber::plain(-f64::MAX, f64::MAX, false, false)
    }

    /// Whether both bounds are finite (the value cannot be `±Infinity`).
    pub fn is_finite(&self) -> bool {
        self.lower.is_finite() && self.upper.is_finite()
    }

    /// Whether the value can be `±Infinity` — derived from the bounds, since
    /// finiteness is structural.
    pub fn may_be_infinite(&self) -> bool {
        self.lower.is_infinite() || self.upper.is_infinite()
    }

    /// Whether the value can be exactly zero — the divide-by-zero test. Zero is
    /// in range and not cut out.
    pub fn includes_zero(&self) -> bool {
        self.lower <= 0.0 && self.upper >= 0.0 && self.excluded != Some(0.0)
    }

    /// Whether the value is provably exactly zero (and never `NaN`).
    pub fn is_definitely_zero(&self) -> bool {
        self.lower == 0.0 && self.upper == 0.0 && !self.may_be_nan && self.excluded != Some(0.0)
    }

    /// Whether the value provably never holds `point` — by its bounds, by the
    /// integer flag against a fractional point, or by the excluded-point cut.
    pub fn point_excluded(&self, point: f64) -> bool {
        if point < self.lower || point > self.upper {
            return true;
        }
        // `integer` describes every finite inhabitant; the interval may still
        // include an infinity from overflow. Only a finite fractional point is
        // impossible.
        if self.integer && point.is_finite() && point.fract() != 0.0 {
            return true;
        }
        self.excluded == Some(point)
    }

    /// Structural equality — the fixed-point stop test. Bounds are never `NaN`,
    /// so `==` is the right comparison.
    pub fn same(&self, other: &AbstractNumber) -> bool {
        self.lower == other.lower
            && self.upper == other.upper
            && self.integer == other.integer
            && self.may_be_nan == other.may_be_nan
            && self.excluded == other.excluded
    }
}

/// Whether both operands are finite and `NaN`-free — the precondition for
/// trusting bound arithmetic (`Infinity - Infinity` is `NaN`).
fn safe_operands(left: &AbstractNumber, right: &AbstractNumber) -> bool {
    left.is_finite() && right.is_finite() && !left.may_be_nan && !right.may_be_nan
}

/// With clean operands the bounds are trustworthy even when they overflow to
/// `±Infinity` (overflow yields an infinity, never a `NaN`); otherwise the
/// result collapses to unknown.
fn bounded_result(
    lower: f64,
    upper: f64,
    integer: bool,
    left: &AbstractNumber,
    right: &AbstractNumber,
) -> AbstractNumber {
    if !safe_operands(left, right) {
        return AbstractNumber::unknown();
    }
    AbstractNumber::plain(lower, upper, integer, false)
}

/// Addition. The only `NaN` case is opposite-signed infinities meeting; with
/// `NaN`-free operands the bounds stay real. A zero exclusion is forwarded when
/// one side is an exact point and the other excludes its negation (`x != -c`
/// makes `x + c != 0`).
pub fn add(left: &AbstractNumber, right: &AbstractNumber) -> AbstractNumber {
    let lower = left.lower + right.lower;
    let upper = left.upper + right.upper;
    let opposite_infinities = (left.upper == f64::INFINITY && right.lower == f64::NEG_INFINITY)
        || (left.lower == f64::NEG_INFINITY && right.upper == f64::INFINITY);
    let mut result = AbstractNumber::plain(
        if lower.is_nan() {
            f64::NEG_INFINITY
        } else {
            lower
        },
        if upper.is_nan() { f64::INFINITY } else { upper },
        left.integer && right.integer,
        left.may_be_nan || right.may_be_nan || opposite_infinities,
    );
    if let Some((point, other)) = point_and_other(left, right) {
        if other.point_excluded(-point) && result.lower < 0.0 && result.upper > 0.0 {
            result.excluded = Some(0.0);
        }
    }
    result
}

/// The exact point value of a one-point, `NaN`-free operand, if it is one.
fn point_operand(value: &AbstractNumber) -> Option<f64> {
    if value.lower == value.upper && !value.may_be_nan {
        Some(value.lower)
    } else {
        None
    }
}

/// When one operand is a single point, that point and the *other* operand — the
/// shape both the addition and multiplication zero-exclusion rules need. The
/// right side is preferred as the point when both are points.
fn point_and_other<'a>(
    left: &'a AbstractNumber,
    right: &'a AbstractNumber,
) -> Option<(f64, &'a AbstractNumber)> {
    if let Some(point) = point_operand(right) {
        Some((point, left))
    } else {
        point_operand(left).map(|point| (point, right))
    }
}

/// The four corner quotients `left / right` — the monotone-image bounds a
/// division computes over interval endpoints.
fn quotient_corners(left: &AbstractNumber, right: &AbstractNumber) -> [f64; 4] {
    [
        left.lower / right.lower,
        left.lower / right.upper,
        left.upper / right.lower,
        left.upper / right.upper,
    ]
}

/// Subtraction is `left + (-right)`; negation is exact on every value including
/// infinities, so an excluded point flips sign with the value.
pub fn sub(left: &AbstractNumber, right: &AbstractNumber) -> AbstractNumber {
    let negated = AbstractNumber {
        lower: -right.upper,
        upper: -right.lower,
        integer: right.integer,
        may_be_nan: right.may_be_nan,
        excluded: right.excluded.map(|p| -p),
    };
    add(left, &negated)
}

/// Multiplication. Collapses to unknown on unsafe operands; otherwise the corner
/// products bound the result. A factor of magnitude at least 1 preserves a zero
/// exclusion (`|c*x| >= |x|`).
pub fn mul(left: &AbstractNumber, right: &AbstractNumber) -> AbstractNumber {
    if !safe_operands(left, right) {
        return AbstractNumber::unknown();
    }
    let products = [
        left.lower * right.lower,
        left.lower * right.upper,
        left.upper * right.lower,
        left.upper * right.upper,
    ];
    let (lo, hi) = min_max(&products);
    let mut result = bounded_result(lo, hi, left.integer && right.integer, left, right);
    if let Some((point, other)) = point_and_other(left, right) {
        if point.is_finite()
            && point.abs() >= 1.0
            && other.point_excluded(0.0)
            && !result.may_be_nan
            && result.lower < 0.0
            && result.upper > 0.0
        {
            result.excluded = Some(0.0);
        }
    }
    result
}

/// Division. A one-signed finite divisor gives exact monotone corner quotients;
/// a zero-straddling divisor with zero excluded takes the across-zero path;
/// otherwise (the divisor may be zero) the result is unknown.
pub fn div(left: &AbstractNumber, right: &AbstractNumber) -> AbstractNumber {
    if !left.may_be_nan && !right.may_be_nan && right.is_finite() {
        if right.lower > 0.0 || right.upper < 0.0 {
            let (lo, hi) = min_max(&quotient_corners(left, right));
            return AbstractNumber::plain(lo, hi, false, false);
        }
        if right.excluded == Some(0.0) {
            return divide_across_zero(left, right);
        }
    }
    if !safe_operands(left, right) || (right.lower <= 0.0 && right.upper >= 0.0) {
        return AbstractNumber::unknown();
    }
    let (lo, hi) = min_max(&quotient_corners(left, right));
    bounded_result(lo, hi, false, left, right)
}

/// A divisor interval straddling zero with zero itself excluded. An integer
/// divisor then has magnitude at least 1 (quotient bounded by the dividend); a
/// float divisor can sit arbitrarily close to zero (quotient can overflow), but
/// never `NaN` since zero is cut.
fn divide_across_zero(left: &AbstractNumber, right: &AbstractNumber) -> AbstractNumber {
    if !right.integer {
        return AbstractNumber::plain(f64::NEG_INFINITY, f64::INFINITY, false, false);
    }
    let negative = AbstractNumber {
        upper: right.upper.min(-1.0),
        ..*right
    };
    let positive = AbstractNumber {
        lower: right.lower.max(1.0),
        ..*right
    };
    let mut quotients: Vec<f64> = Vec::new();
    for part in [negative, positive] {
        if part.lower <= part.upper {
            quotients.extend(quotient_corners(left, &part));
        }
    }
    if quotients.is_empty() {
        return AbstractNumber::unknown();
    }
    let (lo, hi) = min_max(&quotients);
    bounded_result(lo, hi, false, left, right)
}

/// Remainder. The result's sign follows the dividend; its magnitude stays below
/// both operands' magnitudes (tightened to `|divisor| - 1` for integers on the
/// divisor side only). `NaN` when the divisor may be zero or the dividend may be
/// infinite. `divisor_nonzero` records a discharged nonzero requirement.
pub fn rem(left: &AbstractNumber, right: &AbstractNumber, divisor_nonzero: bool) -> AbstractNumber {
    if left.may_be_nan || right.may_be_nan {
        return AbstractNumber::unknown();
    }
    let divisor_may_be_zero = !divisor_nonzero && right.includes_zero();
    let dividend_may_be_infinite = !left.is_finite();
    let dividend_magnitude = left.lower.abs().max(left.upper.abs());
    let divisor_magnitude = right.lower.abs().max(right.upper.abs());
    let integer = left.integer && right.integer;
    let divisor_bound = if integer && divisor_magnitude.is_finite() {
        (divisor_magnitude - 1.0).max(0.0)
    } else {
        divisor_magnitude
    };
    let bound = dividend_magnitude.min(divisor_bound);
    let lower = if left.lower < 0.0 {
        if bound.is_finite() {
            -bound
        } else {
            f64::NEG_INFINITY
        }
    } else {
        0.0
    };
    let upper = if left.upper > 0.0 {
        if bound.is_finite() {
            bound
        } else {
            f64::INFINITY
        }
    } else {
        0.0
    };
    AbstractNumber::plain(
        lower,
        upper,
        integer,
        divisor_may_be_zero || dividend_may_be_infinite,
    )
}

/// Join (least upper bound): the interval that covers both, keeping a hole only
/// where neither side can hold it. Zero is always tried, so a sign-split join
/// (`[-5,-2]` ∪ `[2,5]`) keeps its zero exclusion.
pub fn join(left: &AbstractNumber, right: &AbstractNumber) -> AbstractNumber {
    let mut joined = AbstractNumber::plain(
        left.lower.min(right.lower),
        left.upper.max(right.upper),
        left.integer && right.integer,
        left.may_be_nan || right.may_be_nan,
    );
    joined.excluded = shared_excluded_point(left, right, joined.lower, joined.upper);
    joined
}

/// The one hole a join or widen may keep — a point both inputs exclude, strictly
/// inside the combined interval. Zero is always considered.
fn shared_excluded_point(
    left: &AbstractNumber,
    right: &AbstractNumber,
    lower: f64,
    upper: f64,
) -> Option<f64> {
    for point in [left.excluded, right.excluded, Some(0.0)] {
        let Some(point) = point else { continue };
        if left.point_excluded(point)
            && right.point_excluded(point)
            && lower < point
            && point < upper
        {
            return Some(point);
        }
    }
    None
}

/// Widening: a fresh wider cover that pushes an unstable bound straight to the
/// extreme (`±MAX` while both sides are finite, else `±Infinity`), so a loop
/// reaches its fixed point in bounded rounds. A hole survives only when both
/// rounds excluded it.
pub fn widen(previous: &AbstractNumber, next: &AbstractNumber) -> AbstractNumber {
    let finite = previous.is_finite() && next.is_finite();
    let lower = if next.lower < previous.lower {
        if finite {
            -f64::MAX
        } else {
            f64::NEG_INFINITY
        }
    } else {
        next.lower
    };
    let upper = if next.upper > previous.upper {
        if finite {
            f64::MAX
        } else {
            f64::INFINITY
        }
    } else {
        next.upper
    };
    let mut widened = AbstractNumber::plain(lower, upper, next.integer, next.may_be_nan);
    widened.excluded = shared_excluded_point(previous, next, widened.lower, widened.upper);
    widened
}

/// `[min, max]` of the corner values, ignoring any `NaN` (a `NaN` corner means a
/// mixed-infinity product the caller has already guarded against via
/// `safe_operands`).
fn min_max(values: &[f64]) -> (f64, f64) {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for &v in values {
        if v.is_nan() {
            continue;
        }
        lo = lo.min(v);
        hi = hi.max(v);
    }
    (lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_up_and_down_are_adjacent_doubles() {
        assert_eq!(next_up(0.0), SMALLEST_SUBNORMAL);
        assert_eq!(next_down(0.0), -SMALLEST_SUBNORMAL);
        // next_up then next_down returns the original for a normal value.
        assert_eq!(next_down(next_up(1.0)), 1.0);
        assert_eq!(next_up(f64::INFINITY), f64::INFINITY);
        assert!(next_up(f64::NAN).is_nan());
        // Strictly greater / less.
        assert!(next_up(1.0) > 1.0);
        assert!(next_down(1.0) < 1.0);
    }

    #[test]
    fn constant_sets_integer_and_nan_flags() {
        assert!(AbstractNumber::constant(3.0).integer);
        assert!(!AbstractNumber::constant(3.5).integer);
        assert!(AbstractNumber::constant(f64::NAN).may_be_nan);
    }

    #[test]
    fn includes_zero_respects_the_excluded_point() {
        let mut n = AbstractNumber::any_integer();
        assert!(n.includes_zero());
        n.excluded = Some(0.0);
        assert!(!n.includes_zero());
    }

    #[test]
    fn any_integer_may_be_zero_but_a_positive_range_may_not() {
        assert!(AbstractNumber::any_integer().includes_zero());
        let positive = AbstractNumber::plain(1.0, 10.0, true, false);
        assert!(!positive.includes_zero());
    }

    #[test]
    fn add_forwards_a_nonzero_exclusion_to_zero() {
        // x != 4, then x + (-4): the sum excludes zero (x - 4 != 0).
        let mut x = AbstractNumber::plain(0.0, 10.0, true, false);
        x.excluded = Some(4.0);
        let minus_four = AbstractNumber::constant(-4.0);
        let sum = add(&x, &minus_four);
        assert_eq!(sum.excluded, Some(0.0));
        assert!(!sum.includes_zero());
    }

    #[test]
    fn sub_flips_the_excluded_point_sign() {
        let mut x = AbstractNumber::plain(0.0, 10.0, true, false);
        x.excluded = Some(4.0);
        // x - 4 excludes 0.
        let four = AbstractNumber::constant(4.0);
        let diff = sub(&x, &four);
        assert_eq!(diff.excluded, Some(0.0));
    }

    #[test]
    fn add_of_two_integers_is_an_integer() {
        let a = AbstractNumber::plain(1.0, 2.0, true, false);
        let b = AbstractNumber::plain(3.0, 4.0, true, false);
        let s = add(&a, &b);
        assert!(s.integer);
        assert_eq!(s.lower, 4.0);
        assert_eq!(s.upper, 6.0);
    }

    #[test]
    fn div_by_a_one_signed_range_is_exact() {
        let a = AbstractNumber::plain(10.0, 20.0, true, false);
        let b = AbstractNumber::plain(2.0, 5.0, true, false);
        let q = div(&a, &b);
        assert_eq!(q.lower, 2.0);
        assert_eq!(q.upper, 10.0);
        assert!(!q.may_be_nan);
    }

    #[test]
    fn div_by_a_zero_straddling_range_is_unknown() {
        let a = AbstractNumber::constant(10.0);
        let b = AbstractNumber::any_integer(); // may be zero
        let q = div(&a, &b);
        assert!(q.may_be_nan);
    }

    #[test]
    fn div_across_excluded_zero_stays_finite_for_integers() {
        let a = AbstractNumber::constant(10.0);
        let mut b = AbstractNumber::plain(-5.0, 5.0, true, false);
        b.excluded = Some(0.0);
        let q = div(&a, &b);
        assert!(!q.may_be_nan);
        assert!(q.is_finite());
    }

    #[test]
    fn rem_flags_nan_when_the_divisor_may_be_zero() {
        let a = AbstractNumber::constant(10.0);
        let b = AbstractNumber::any_integer();
        let r = rem(&a, &b, false);
        assert!(r.may_be_nan);
        // With the nonzero requirement discharged, no NaN.
        let r2 = rem(&a, &b, true);
        assert!(!r2.may_be_nan);
    }

    #[test]
    fn join_of_disjoint_sign_ranges_excludes_zero() {
        let neg = AbstractNumber::plain(-5.0, -2.0, true, false);
        let pos = AbstractNumber::plain(2.0, 5.0, true, false);
        let j = join(&neg, &pos);
        assert_eq!(j.excluded, Some(0.0));
        assert!(!j.includes_zero());
    }

    #[test]
    fn widen_pushes_a_growing_bound_to_the_extreme_and_terminates() {
        let prev = AbstractNumber::plain(0.0, 1.0, true, false);
        let next = AbstractNumber::plain(0.0, 2.0, true, false);
        let w = widen(&prev, &next);
        assert_eq!(w.lower, 0.0);
        assert_eq!(w.upper, f64::MAX);
        // A second round (widen against the join of the widened value with a
        // still-growing sample) is stable — the fixed point is reached.
        let next2 = join(&w, &AbstractNumber::plain(0.0, 3.0, true, false));
        let w2 = widen(&w, &next2);
        assert_eq!(w2.upper, f64::MAX);
        assert!(w.same(&w2));
    }

    #[test]
    fn may_be_infinite_is_derived_from_bounds() {
        assert!(!AbstractNumber::finite_input().may_be_infinite());
        assert!(AbstractNumber::any_integer().may_be_infinite());
    }
}
