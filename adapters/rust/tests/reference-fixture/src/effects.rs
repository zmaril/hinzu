//! The effectful carve-out. These functions really touch the filesystem; the
//! policy's `effects` region allows `fs` here, so they are not themselves
//! violations — they are the leaves a forbidden core must not be able to reach.

use std::fs;

/// Reads a file — a genuine `fs` effect. Handed around as a value by the core,
/// never called there directly, so only the reference-edge rung attributes its
/// effect to the core function that passes it.
pub fn read_audit(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_default()
}
