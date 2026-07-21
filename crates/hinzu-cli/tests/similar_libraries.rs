//! Integration coverage for the curated-library tier of `hinzu similar`
//! (`--libraries`). Builds a tiny cargo project on the fly (a hand-written error
//! enum with `impl Display` + `impl std::error::Error`, and a `From` wrapper),
//! runs the live `syn` path over it, and asserts the tier emits the honest
//! "adopt the library" candidates — toolchain-free (no StableMIR driver needed).

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

/// A minimal cargo project whose `lib.rs` holds:
///  * `MyError` — an enum with a hand-written `impl Display { match self }` and
///    `impl std::error::Error` (the thiserror shape), plus
///  * `Wrapper` — a newtype with an `impl From<String>` that just wraps its arg
///    (the derive_more::From shape).
fn write_fixture_project() -> TempDir {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"libfix\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[lib]\npath = \"lib.rs\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("lib.rs"),
        r#"
use std::fmt;

pub enum MyError {
    NotFound(String),
    Denied(String),
}

impl fmt::Display for MyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MyError::NotFound(m) => write!(f, "not found: {m}"),
            MyError::Denied(m) => write!(f, "denied: {m}"),
        }
    }
}

impl std::error::Error for MyError {}

pub struct Wrapper(pub String);

impl From<String> for Wrapper {
    fn from(s: String) -> Self {
        Wrapper(s)
    }
}
"#,
    )
    .unwrap();
    dir
}

/// Run `hinzu similar <project> --rust-extractor syn --libraries <cfg>` and parse
/// the stdout JSON.
fn run_with_libraries(project: &std::path::Path, cfg: &std::path::Path) -> Value {
    let mut cmd = Command::cargo_bin("hinzu").unwrap();
    let assert = cmd
        .arg("similar")
        .arg(project)
        .arg("--rust-extractor")
        .arg("syn")
        .arg("--libraries")
        .arg(cfg)
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    serde_json::from_str(&out).expect("stdout is JSON")
}

#[test]
fn adopt_thiserror_and_derive_more_from_on_a_handwritten_project() {
    let project = write_fixture_project();
    let cfg = project.path().join("libraries.toml");
    std::fs::write(
        &cfg,
        r#"
[[libraries.rust]]
crate  = "thiserror"
kinds  = ["derive"]
source = "curated"
trust  = 0.8

[[libraries.rust]]
crate  = "derive_more"
kinds  = ["derive"]
source = "curated"
trust  = 0.7
"#,
    )
    .unwrap();

    let doc = run_with_libraries(project.path(), &cfg);
    let libs = doc["library_candidates"]
        .as_array()
        .expect("library_candidates array");
    assert!(
        !libs.is_empty(),
        "expected at least one adopt-a-library candidate"
    );

    // The thiserror candidate: MyError with its Display + Error impls.
    let thiserror = libs
        .iter()
        .find(|f| f["external"]["library"] == "thiserror")
        .expect("a thiserror candidate");
    assert_eq!(thiserror["external"]["item"], "Error");
    assert_eq!(thiserror["external"]["kind"], "derive");
    assert_eq!(thiserror["external"]["source"], "curated");
    assert_eq!(thiserror["likely_abstraction"]["family"], "adopt_library");
    // It cites the two hand-written impls it eliminates.
    let locals: Vec<&str> = thiserror["local"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["display"].as_str().unwrap())
        .collect();
    assert!(locals.iter().any(|d| d.contains("MyError")));
    assert!(locals.iter().any(|d| d.contains("impl Display")));
    assert!(locals.iter().any(|d| d.contains("impl Error")));
    // The honest counter-evidence is present.
    let ce: Vec<&str> = thiserror["counter_evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap())
        .collect();
    assert!(ce.iter().any(|c| c.contains("semantics unverified")));
    assert!(ce.iter().any(|c| c.contains("adds a dependency")));

    // The derive_more::From candidate: the wrapping `From<String> for Wrapper`.
    let from = libs
        .iter()
        .find(|f| f["external"]["library"] == "derive_more" && f["external"]["item"] == "From")
        .expect("a derive_more::From candidate");
    assert!(from["match_basis"][0]
        .as_str()
        .unwrap()
        .contains("wraps its argument"));

    // The library-tier source profile is reported in the fidelity block.
    let profiles = doc["profiles"].as_array().unwrap();
    assert!(
        profiles.iter().any(|p| p["extractor"] == "curated-pattern"),
        "the curated-pattern source profile should be attached"
    );
}

#[test]
fn no_libraries_flag_leaves_the_base_document_unchanged() {
    // Without `--libraries`, the document carries no `library_candidates` key at
    // all (serde skips the empty vec), so the frozen v1 shape is preserved.
    let project = write_fixture_project();
    let mut cmd = Command::cargo_bin("hinzu").unwrap();
    let assert = cmd
        .arg("similar")
        .arg(project.path())
        .arg("--rust-extractor")
        .arg("syn")
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let doc: Value = serde_json::from_str(&out).unwrap();
    assert!(
        doc.get("library_candidates").is_none(),
        "a base run must omit library_candidates entirely"
    );
}
