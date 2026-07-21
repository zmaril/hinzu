//! The `--libraries` config for `hinzu similar`: a committed TOML that declares
//! which libraries the user likes, so the curated-library tier can match local
//! code against the shapes those libraries expose.
//!
//! Precedent: `portdiff_config.rs` — the CLI reads and parses the file, then
//! lowers it into the pure core's inputs ([`hinzu_core::similarity::LibraryParams`]
//! and, for `rustdoc` sources, a set of virtual signatures). The core never reads
//! the file; it consumes the external shapes as data, exactly like local
//! signatures.
//!
//! Shape:
//!
//! ```toml
//! [[libraries.rust]]
//! crate  = "thiserror"
//! kinds  = ["derive"]        # function | trait | derive
//! source = "curated"         # curated | rustdoc
//! trust  = 0.8               # 0..1, scales the finding's confidence
//!
//! [[libraries.rust]]
//! crate        = "itertools"
//! version      = "0.13"      # optional, advisory (version-skew note)
//! kinds        = ["function"]
//! source       = "rustdoc"
//! trust        = 0.9
//! rustdoc_json = "target/doc/itertools.json"   # a pre-generated rustdoc JSON
//! items        = ["process_results"]           # which items to virtualize
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use hinzu_core::similarity::{CuratedSelection, LibraryParams, VirtualSignature};
use serde::Deserialize;

use crate::library_rustdoc;

/// The whole `--libraries` file.
#[derive(Debug, Deserialize)]
pub struct LibraryConfig {
    /// The declared libraries, grouped by language. Only `rust` is read today.
    #[serde(default)]
    pub libraries: Languages,
}

/// The per-language library lists.
#[derive(Debug, Default, Deserialize)]
pub struct Languages {
    /// The Rust libraries the user likes.
    #[serde(default)]
    pub rust: Vec<LibraryEntry>,
}

/// One declared library.
#[derive(Debug, Deserialize)]
pub struct LibraryEntry {
    /// The crate name (`"thiserror"`).
    #[serde(rename = "crate")]
    pub crate_name: String,
    /// The advisory version, echoed into the version-skew note.
    #[serde(default)]
    pub version: Option<String>,
    /// Which item kinds to match (`function` | `trait` | `derive`).
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Where the shapes come from (`curated` | `rustdoc`).
    pub source: String,
    /// The `user_trust` factor (0..1). Defaults to a cautious 0.5.
    #[serde(default = "default_trust")]
    pub trust: f64,
    /// For a `rustdoc` source, a pre-generated rustdoc JSON file to read the
    /// item signatures from (generate with `cargo rustdoc -p <crate> --
    /// -Zunstable-options --output-format json` on the pinned nightly).
    #[serde(default)]
    pub rustdoc_json: Option<String>,
    /// For a `rustdoc` source, which named items to virtualize. Empty = none
    /// (the reader does not guess).
    #[serde(default)]
    pub items: Vec<String>,
}

fn default_trust() -> f64 {
    0.5
}

/// The lowered inputs for the core's library tier: the curated selection map and
/// the Tier-A virtual signatures.
pub struct LoweredLibraries {
    /// The core matcher params (curated crate selection + thresholds).
    pub params: LibraryParams,
    /// The Tier-A virtual signatures (from `rustdoc` sources).
    pub virtual_sigs: Vec<VirtualSignature>,
    /// Human notes about what was and was not loaded, printed to stderr so the
    /// capability edge is never silent (e.g. a rustdoc source with no JSON).
    pub notes: Vec<String>,
}

impl LibraryConfig {
    /// Read + parse the config file.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading libraries config {}", path.display()))?;
        toml::from_str(&text)
            .with_context(|| format!("parsing libraries config {}", path.display()))
    }

    /// Lower the config into the core's library-tier inputs. `base` is the
    /// directory relative `rustdoc_json` paths resolve against (the config's own
    /// directory).
    pub fn lower(&self, base: &Path) -> Result<LoweredLibraries> {
        let mut params = LibraryParams::default();
        let mut virtual_sigs = Vec::new();
        let mut notes = Vec::new();

        for entry in &self.libraries.rust {
            let trust = entry.trust.clamp(0.0, 1.0);
            match entry.source.as_str() {
                "curated" => lower_curated(entry, trust, &mut params, &mut notes),
                "rustdoc" => lower_rustdoc(entry, trust, base, &mut virtual_sigs, &mut notes)?,
                other => notes.push(format!(
                    "libraries: ignoring crate `{}` with unknown source `{other}` (expected \
                     `curated` or `rustdoc`)",
                    entry.crate_name
                )),
            }
        }

        Ok(LoweredLibraries {
            params,
            virtual_sigs,
            notes,
        })
    }
}

/// Fold a `curated` entry into the crate-selection map: set its trust and turn on
/// the kinds it declared.
fn lower_curated(
    entry: &LibraryEntry,
    trust: f64,
    params: &mut LibraryParams,
    notes: &mut Vec<String>,
) {
    let existing = params
        .curated_crates
        .entry(entry.crate_name.clone())
        .or_insert(CuratedSelection {
            trust,
            derive: false,
            function: false,
        });
    existing.trust = trust;
    for k in &entry.kinds {
        match k.as_str() {
            "derive" => existing.derive = true,
            // `trait`-shape matching reuses the function tier.
            "function" | "trait" => existing.function = true,
            other => notes.push(format!(
                "libraries: ignoring unknown kind `{other}` for curated crate `{}`",
                entry.crate_name
            )),
        }
    }
}

/// Load a `rustdoc` entry's virtual signatures from its pre-generated JSON, or
/// note honestly that none was given.
fn lower_rustdoc(
    entry: &LibraryEntry,
    trust: f64,
    base: &Path,
    virtual_sigs: &mut Vec<VirtualSignature>,
    notes: &mut Vec<String>,
) -> Result<()> {
    let Some(rel) = &entry.rustdoc_json else {
        notes.push(format!(
            "libraries: crate `{}` declared source=rustdoc but no `rustdoc_json` path was given — \
             no virtual signatures loaded for it (generate one with `cargo rustdoc -p {} -- \
             -Zunstable-options --output-format json`)",
            entry.crate_name, entry.crate_name
        ));
        return Ok(());
    };
    let json_path = resolve(base, rel);
    let sigs = library_rustdoc::virtual_signatures_from_json(
        &json_path,
        &entry.crate_name,
        &entry.items,
        trust,
        entry.version.clone(),
    )
    .with_context(|| format!("reading rustdoc JSON {}", json_path.display()))?;
    notes.push(format!(
        "libraries: loaded {} rustdoc virtual signature(s) for `{}` from {}",
        sigs.len(),
        entry.crate_name,
        json_path.display()
    ));
    virtual_sigs.extend(sigs);
    Ok(())
}

/// Resolve `rel` against `base`, unless it is already absolute.
fn resolve(base: &Path, rel: &str) -> PathBuf {
    let p = PathBuf::from(rel);
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}
