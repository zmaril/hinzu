//! Reference-edge fixture crate. See `core.rs` (the forbidden functional core,
//! reaching `fs` only through handed-off function values) and `effects.rs` (the
//! allowed effectful carve-out).

pub mod core;
pub mod effects;

/// A tiny driver so the higher-order paths are actually reachable/instantiated
/// (the closure and fn-pointer monomorphizations exist only where used).
pub fn run() -> (String, String, i32, usize) {
    let a = core::schedule_audit();
    let b = core::defer_read();
    let c = core::pure_total(2, 3);
    let d = core::BOOT_CONFIG.len();
    (a, b, c, d)
}
