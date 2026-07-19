//! The functional core. The policy forbids `fs` here. Every effect below reaches
//! the filesystem only by *handing off* a function value — a callback, a closure,
//! or a lazy import-time initializer — which a call-only graph never sees. Under
//! call-only all three functions look pure; the reference-edge rung recovers them.

use std::sync::LazyLock;

use crate::effects::read_audit;

/// Higher-order: passes the effectful `read_audit` as a callback to `run_with`,
/// which invokes it. Call-only records `schedule_audit -> run_with` and an
/// `<indirect>` call inside `run_with`, so the `fs` effect never reaches here.
/// The reference edge `schedule_audit -> read_audit` recovers it.
pub fn schedule_audit() -> String {
    run_with(read_audit)
}

/// Invokes a callback through a fn pointer — the indirection that hides the
/// callee from a call-only graph.
fn run_with(cb: fn(&str) -> String) -> String {
    cb("audit.log")
}

/// A closure that performs the effect, handed to `run_closure` to invoke. The
/// closure is its own body (walked under its own id); the reference edge
/// `defer_read -> {closure}` carries the closure body's `fs` effect up.
pub fn defer_read() -> String {
    let job = || read_audit("deferred.log");
    run_closure(job)
}

fn run_closure<F: Fn() -> String>(f: F) -> String {
    f()
}

/// An import-time (lazy) initializer: a `LazyLock` whose closure reads a file on
/// first access — the Rust analogue of a module-level import-time effect. The
/// static's initializer references the closure, whose body reaches `fs`.
pub static BOOT_CONFIG: LazyLock<String> = LazyLock::new(|| read_audit("boot.cfg"));

/// A genuinely pure core function — arithmetic only. It must never be flagged;
/// the rung is additive, not a blanket taint.
pub fn pure_total(a: i32, b: i32) -> i32 {
    a + b
}
