//! Shared Rust-source loading for the syn-based CLI extractors.
//!
//! Both structural extractors ([`crate::structural_rust`] and
//! [`crate::library_extract`]) walk a cargo project's `.rs` files and `syn`-parse
//! each one, skipping a file that cannot be read or parsed with a stderr warning
//! rather than sinking the whole run. That file-walk-and-parse is the same in
//! both; it lives here so there is one copy. This is the CLI/adapter layer, so it
//! is the only place that reads files.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Every `.rs` file under `root`, skipping any `target/` directory and hidden
/// directories. A plain recursive walk — no external crate needed.
pub fn rust_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .with_context(|| format!("reading directory {}", dir.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if name == "target" || name.starts_with('.') {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Read and `syn`-parse every `.rs` file under `project`, pairing each parsed AST
/// with its project-relative path. A file that cannot be read or parsed is
/// skipped with a stderr warning (a project may hold a generated or
/// edition-specific file `syn` cannot read, and one bad file should not sink the
/// analysis) — never faked.
pub fn parsed_rust_files(project: &Path) -> Result<Vec<(String, syn::File)>> {
    let files = rust_files(project)?;
    let mut out = Vec::with_capacity(files.len());
    for file in &files {
        let rel = file
            .strip_prefix(project)
            .unwrap_or(file)
            .to_string_lossy()
            .to_string();
        let src = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("warning: skipping {} (unreadable: {e})", file.display());
                continue;
            }
        };
        let parsed = match syn::parse_file(&src) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("warning: skipping {rel} (parse error: {e})");
                continue;
            }
        };
        out.push((rel, parsed));
    }
    Ok(out)
}
