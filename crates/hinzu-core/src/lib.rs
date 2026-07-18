//! The hinzu engine. The CLI is a thin shell over this crate; the real work
//! lands here so it stays testable without going through argv.

use anyhow::Result;

/// Placeholder engine entry point. Replace with the real surface as it lands.
pub fn run() -> Result<String> {
    Ok("hinzu: nothing to do yet".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_returns_a_message() {
        assert!(run().unwrap().starts_with("hinzu:"));
    }
}
