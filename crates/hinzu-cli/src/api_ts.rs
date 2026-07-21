//! The TypeScript public-API extraction path: drive the compiler-API adapter
//! (`analyze.mjs --api`) over a target package and lower its JSON into hinzu's
//! language-agnostic [`ApiReport`].
//!
//! The adapter (a Node program using the TypeScript `TypeChecker`) already emits
//! the phase-1 schema shape — `{ package, fidelity, modules }` — so this path is
//! thin: spawn the adapter, capture its stdout, deserialize straight into the
//! core [`hinzu_core::api`] types, and hand them to the pure
//! [`hinzu_core::api::build_api`] for the same normalization/sorting the Rust
//! path uses. All process/fs effects live here in the CLI; core only transforms
//! the parsed result.

use std::path::Path;

use anyhow::{Context, Result};
use hinzu_core::api::{build_api, ApiReport, Fidelity, Module, PackageInfo};
use serde::Deserialize;

use crate::ts_adapter;

/// The adapter's `--api` output: the unsorted, un-versioned pieces the core
/// `build_api` normalizes. Deserializes directly into the core API types (their
/// serde renames match the adapter's JSON keys).
#[derive(Deserialize)]
struct TsApiEnvelope {
    package: PackageInfo,
    fidelity: Fidelity,
    modules: Vec<Module>,
}

/// Extract the public API of a TypeScript package: run the adapter in API mode,
/// parse its report, and normalize it through core. `root_label` overrides the
/// report's `package.root` with the target as the operator named it (matching
/// the Rust path).
pub fn extract(project: &Path, root_label: &str) -> Result<ApiReport> {
    let json = ts_adapter::ts_script_adapter()?.run_capture(project, &["--api"])?;
    let mut envelope: TsApiEnvelope =
        serde_json::from_str(&json).context("parsing the TypeScript API adapter's JSON output")?;
    envelope.package.root = root_label.to_string();
    Ok(build_api(
        envelope.package,
        envelope.fidelity,
        envelope.modules,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The adapter's raw JSON (camelCase contract keys) must deserialize into the
    /// core API types and normalize through `build_api`. This guards the wire
    /// contract between `analyze.mjs --api` and the Rust lowering — a rename on
    /// either side breaks here rather than silently dropping fields.
    #[test]
    fn adapter_json_deserializes_and_normalizes() {
        let raw = r#"{
          "package": {"name": "demo", "language": "typescript", "root": ".", "version": "1.0.0"},
          "fidelity": {"source": "tsc", "format_version": "5.9.3", "complete": false, "notes": []},
          "modules": [
            {"path": "src/z", "file": "src/z.ts", "doc": null, "items": []},
            {"path": "src/a", "file": "src/a.ts", "doc": null, "items": [
              {"kind": "function", "id": "src/a#f", "name": "f", "visibility": "public",
               "modulePath": "src/a", "file": "src/a.ts", "line": 3, "doc": null,
               "generics": [], "deprecated": false,
               "signature": {"params": [{"name": "x", "ty": "string", "optional": true, "default": null}],
                 "returnType": "Promise<void>", "isAsync": true, "receiver": null,
                 "errorType": "Error", "generics": []},
               "fields": [], "variants": [], "implements": [],
               "aliasTarget": null, "constType": null, "constValue": null}
            ]}
          ]
        }"#;
        let envelope: TsApiEnvelope = serde_json::from_str(raw).expect("adapter JSON parses");
        let report = build_api(envelope.package, envelope.fidelity, envelope.modules);

        // Version stamped, modules sorted by path (a before z).
        assert_eq!(report.hinzu_api_version, hinzu_core::api::HINZU_API_VERSION);
        let paths: Vec<&str> = report.modules.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, vec!["src/a", "src/z"]);

        // The camelCase-keyed signature fields round-tripped intact.
        let sig = report.modules[0].items[0]
            .signature
            .as_ref()
            .expect("function signature");
        assert!(sig.is_async);
        assert_eq!(sig.return_type.as_deref(), Some("Promise<void>"));
        assert_eq!(sig.error_type.as_deref(), Some("Error"));
        assert!(sig.params[0].optional);
    }
}
