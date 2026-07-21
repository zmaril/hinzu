//! Fixture functions for hinzu's numeric-range analysis.

/// UNSAFE: `count` can be zero, so this integer division can panic.
pub fn ratio(width: i64, count: i64) -> i64 {
    width / count
}

/// UNSAFE: `n` can be zero, so this remainder can panic.
pub fn modulo(x: i64, n: i64) -> i64 {
    x % n
}

/// SAFE: the guard proves `c != 0` before dividing, so no hazard.
pub fn ratio_guarded(w: i64, c: i64) -> i64 {
    if c != 0 {
        w / c
    } else {
        0
    }
}

/// SAFE: dividing by a nonzero constant can never be zero.
pub fn div_by_const(a: i64) -> i64 {
    a / 2
}
