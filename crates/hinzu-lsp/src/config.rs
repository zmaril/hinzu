//! Per-language configuration as data. A [`LanguageConfig`] is everything the
//! generic LSP extractor needs to turn one language into hinzu facts, and
//! nothing about the extractor knows any language beyond it:
//!
//! * `server_cmd` / `file_globs` / excludes — how to launch the server and which
//!   files it owns,
//! * `init_options` — the server's LSP `initializationOptions` (for ty: the
//!   pinned `python-version` / `python-platform` and `diagnosticMode`),
//! * `provenance` — a uri→(package, origin) ruleset that classifies an external
//!   callee's defining file (ty's vendored typeshed, the interpreter's real
//!   stdlib, an installed package, …), and
//! * the effect map — `<package>::<qualname>`→effect and `<package>`→effect,
//!   loaded straight from the shipped annotation table so there is one source of
//!   truth (`python.toml` for Python).
//!
//! Adding a language is a new config file plus its provenance/effect rows — no
//! new extractor code. Python (over ty) and Go (over gopls) are both shipped,
//! green deliverables driven by this one config type.

use std::collections::BTreeMap;
use std::str::FromStr;

use anyhow::{Context, Result};
use hinzu_core::facts::{Effect, Language};
use regex::Regex;
use serde::Deserialize;

/// One uri→package rule. A callee whose defining file matches `regex` belongs to
/// `package` (built by substituting the capture groups into the template and,
/// when `normalize`, turning a `foo/__init__` or `foo/bar` path tail into the
/// dotted module `foo` / `foo.bar`). `origin` says how to treat an unmapped
/// member of the package: `stdlib` is trusted-pure baseline, `module` (a
/// third-party package) is `Unknown` and fails closed.
#[derive(Clone, Debug)]
pub struct ProvenanceRule {
    pub regex: Regex,
    pub template: String,
    pub origin: Origin,
    pub normalize: bool,
    /// Package prefixes this rule refuses to claim (e.g. `lib-dynload.` and
    /// `_vendor.` under a real-stdlib lib dir are not importable module names),
    /// leaving them unmapped so they fall through to the next rule / `Other`.
    pub reject_prefixes: Vec<String>,
}

/// How to treat an external package the effect map does not name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Standard library — trusted-pure baseline unless the effect map names it.
    Stdlib,
    /// A third-party module — `Unknown` / fail-closed unless the effect map or a
    /// project `[trust]` line names it.
    Module,
}

impl ProvenanceRule {
    /// Apply this rule to a filesystem path, returning `(package, origin)` when
    /// it matches (and the package is not rejected), else `None`.
    fn apply(&self, path: &str) -> Option<(String, Origin)> {
        let caps = self.regex.captures(path)?;
        let mut pkg = self.template.clone();
        for (i, cap) in caps.iter().enumerate().skip(1) {
            let val = cap.map(|m| m.as_str()).unwrap_or("");
            pkg = pkg.replace(&format!("${i}"), val);
        }
        if self.normalize {
            pkg = pkg.replace("/__init__", "").replace('/', ".");
        }
        if self.reject_prefixes.iter().any(|p| pkg.starts_with(p)) {
            return None;
        }
        Some((pkg, self.origin))
    }
}

/// A ready-probe: an in-memory document the extractor opens and queries until the
/// server can resolve a known stdlib symbol, so the real extraction does not race
/// the server's cold-start workspace warm-up. Ported from the Python adapter's
/// `_await_ready`.
#[derive(Clone, Debug, Deserialize)]
pub struct ReadyProbe {
    pub filename: String,
    pub text: String,
    pub line: u32,
    pub character: u32,
    /// The probe is satisfied once a `textDocument/definition` target's uri
    /// contains any of these markers (for ty: `subprocess` or `/typeshed/`).
    pub expect: Vec<String>,
}

/// A fully-parsed language configuration.
#[derive(Clone, Debug)]
pub struct LanguageConfig {
    pub language_id: String,
    pub server_cmd: Vec<String>,
    pub file_globs: Vec<String>,
    pub exclude_dirs: Vec<String>,
    pub exclude_suffixes: Vec<String>,
    pub init_options: serde_json::Value,
    pub provenance: Vec<ProvenanceRule>,
    pub ready_probe: Option<ReadyProbe>,
    /// `<package>::<qualname>`→effect (the specific rows, e.g. `os::system`).
    pub effect_specific: BTreeMap<String, Effect>,
    /// `<package>`→effect (the whole-module rows, e.g. `subprocess`).
    pub effect_package: BTreeMap<String, Effect>,
    /// Whether a whole-module effect inherits to its submodules — true for
    /// Python (`urllib.request` is net because `urllib` is), false for Go
    /// (`net/url` is pure and independent of `net`). When true, a package's
    /// effect is looked up by walking up its `package_separator`-delimited
    /// prefixes.
    pub package_effects_inherit: bool,
    /// The separator that splits a package into inheritable prefixes (`.` for
    /// Python's dotted modules).
    pub package_separator: String,
}

impl LanguageConfig {
    /// The hinzu-core [`Language`] this config produces facts for, parsed from
    /// its `language_id`. Every definition the extractor emits is stamped with
    /// it, so `hinzu check`'s language-aware root seeding (`go.toml` for Go,
    /// `python.toml` for Python) resolves the stdlib effects the same way for a
    /// live run and for `--facts` JSON. An unrecognized `language_id` falls back
    /// to Rust, which never fires a language-specific annotation rule.
    pub fn language(&self) -> Language {
        self.language_id.parse().unwrap_or(Language::Rust)
    }

    /// Classify an external callee's defining-file path into `(package, origin)`
    /// by the first matching provenance rule. `None` means no rule claimed it —
    /// an unmapped foreign file, which becomes `Unknown` / fail-closed.
    pub fn package_of(&self, path: &str) -> Option<(String, Origin)> {
        self.provenance.iter().find_map(|r| r.apply(path))
    }

    /// The effect a resolved external symbol seeds, if any. A `<package>::<qual>`
    /// specific row wins over the whole-`<package>` row — so `os::system`
    /// (process) overrides the absence of a bare `os` effect, and most of `os`
    /// stays pure. Keyed on the canonical symbol the extractor reconstructs.
    pub fn effect_of(&self, symbol: &str, package: &str) -> Option<Effect> {
        if let Some(e) = self.effect_specific.get(symbol) {
            return Some(*e);
        }
        if let Some(e) = self.effect_package.get(package) {
            return Some(*e);
        }
        // Submodule inheritance (Python): `urllib.request` inherits `urllib`'s
        // net effect. Walk up the dotted prefixes, longest already tried above.
        if self.package_effects_inherit {
            let mut pkg = package;
            while let Some(idx) = pkg.rfind(&self.package_separator) {
                pkg = &pkg[..idx];
                if let Some(e) = self.effect_package.get(pkg) {
                    return Some(*e);
                }
            }
        }
        None
    }
}

/// The on-disk config file shape (`configs/<lang>.toml`).
#[derive(Deserialize)]
struct ConfigDoc {
    language_id: String,
    server_cmd: Vec<String>,
    file_globs: Vec<String>,
    #[serde(default)]
    exclude_dirs: Vec<String>,
    #[serde(default)]
    exclude_suffixes: Vec<String>,
    #[serde(default)]
    init_options: Option<toml::Value>,
    #[serde(default)]
    provenance: Vec<ProvenanceDoc>,
    #[serde(default)]
    ready_probe: Option<ReadyProbe>,
    #[serde(default)]
    package_effects_inherit: bool,
    #[serde(default = "default_separator")]
    package_separator: String,
}

/// The default package separator when a config omits it.
fn default_separator() -> String {
    ".".to_string()
}

#[derive(Deserialize)]
struct ProvenanceDoc {
    regex: String,
    package: String,
    origin: String,
    #[serde(default)]
    normalize: bool,
    #[serde(default)]
    reject_prefixes: Vec<String>,
}

impl LanguageConfig {
    /// Build a config from its TOML file and one or more shipped annotation
    /// tables (the `[roots]` maps that become the effect map). Passing several
    /// tables merges their `[roots]` in order — Python loads the stdlib set
    /// (`python.toml`) plus the third-party library pack (`python-libs.toml`), a
    /// later table overriding an earlier row for the same key. `subst` fills
    /// `{placeholder}` tokens in `init_options` (the pinned python-version /
    /// platform).
    pub fn from_parts(
        config_toml: &str,
        annotations_tomls: &[&str],
        subst: &BTreeMap<String, String>,
    ) -> Result<Self> {
        let doc: ConfigDoc =
            toml::from_str(config_toml).context("parsing the language config TOML")?;

        let mut provenance = Vec::with_capacity(doc.provenance.len());
        for p in doc.provenance {
            let regex = Regex::new(&p.regex)
                .with_context(|| format!("compiling provenance regex `{}`", p.regex))?;
            let origin = match p.origin.as_str() {
                "stdlib" => Origin::Stdlib,
                "module" => Origin::Module,
                other => {
                    anyhow::bail!("provenance origin must be `stdlib` or `module`, got `{other}`")
                }
            };
            provenance.push(ProvenanceRule {
                regex,
                template: p.package,
                origin,
                normalize: p.normalize,
                reject_prefixes: p.reject_prefixes,
            });
        }

        // init_options: TOML value → JSON value, with `{token}` substitution.
        let init_json = match doc.init_options {
            Some(v) => toml_to_json(v),
            None => serde_json::Value::Object(Default::default()),
        };
        let init_options = substitute(init_json, subst);

        let (effect_specific, effect_package) = parse_effect_map(annotations_tomls)?;

        Ok(LanguageConfig {
            language_id: doc.language_id,
            server_cmd: doc.server_cmd,
            file_globs: doc.file_globs,
            exclude_dirs: doc.exclude_dirs,
            exclude_suffixes: doc.exclude_suffixes,
            init_options,
            provenance,
            ready_probe: doc.ready_probe,
            effect_specific,
            effect_package,
            package_effects_inherit: doc.package_effects_inherit,
            package_separator: doc.package_separator,
        })
    }
}

/// Parse one or more annotation tables' `[roots]` into the effect map: a key
/// that carries a `::` is a specific `<package>::<qualname>` row, a bare key is a
/// whole `<package>` row. The tables are merged in order (a later one overrides
/// an earlier row for the same key), so Python's stdlib set (`python.toml`) and
/// its third-party library pack (`python-libs.toml`) become one map — the very
/// same tables hinzu-core's root seeding merges, so there is no drift. A table's
/// `[trust]` section (a pure vouch) is intentionally ignored here: the extractor
/// only seeds effect roots, and a pure package is an edge with no root that
/// hinzu-core clears.
fn parse_effect_map(
    annotations_tomls: &[&str],
) -> Result<(BTreeMap<String, Effect>, BTreeMap<String, Effect>)> {
    #[derive(Deserialize)]
    struct Roots {
        #[serde(default)]
        roots: BTreeMap<String, String>,
    }
    let mut specific = BTreeMap::new();
    let mut package = BTreeMap::new();
    for annotations_toml in annotations_tomls {
        let doc: Roots =
            toml::from_str(annotations_toml).context("parsing the effect-map [roots]")?;
        for (key, name) in doc.roots {
            let effect = Effect::from_str(&name)
                .with_context(|| format!("effect-map rule `{key}` names an unknown effect"))?;
            if key.contains("::") {
                specific.insert(key, effect);
            } else {
                package.insert(key, effect);
            }
        }
    }
    Ok((specific, package))
}

/// Convert a TOML value to the JSON value an LSP `initializationOptions` needs.
fn toml_to_json(v: toml::Value) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        toml::Value::String(s) => J::String(s),
        toml::Value::Integer(i) => J::Number(i.into()),
        toml::Value::Float(f) => serde_json::Number::from_f64(f)
            .map(J::Number)
            .unwrap_or(J::Null),
        toml::Value::Boolean(b) => J::Bool(b),
        toml::Value::Datetime(d) => J::String(d.to_string()),
        toml::Value::Array(a) => J::Array(a.into_iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => {
            J::Object(t.into_iter().map(|(k, v)| (k, toml_to_json(v))).collect())
        }
    }
}

/// Replace `{token}` occurrences in every string leaf of a JSON value.
fn substitute(v: serde_json::Value, subst: &BTreeMap<String, String>) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        J::String(mut s) => {
            for (k, val) in subst {
                s = s.replace(&format!("{{{k}}}"), val);
            }
            J::String(s)
        }
        J::Array(a) => J::Array(a.into_iter().map(|x| substitute(x, subst)).collect()),
        J::Object(o) => J::Object(
            o.into_iter()
                .map(|(k, x)| (k, substitute(x, subst)))
                .collect(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn python_test_config() -> LanguageConfig {
        let mut subst = BTreeMap::new();
        subst.insert("python_version".to_string(), "3.11".to_string());
        subst.insert("python_platform".to_string(), "linux".to_string());
        LanguageConfig::from_parts(
            crate::PYTHON_CONFIG,
            &[crate::PYTHON_ANNOTATIONS, crate::PYTHON_LIB_ANNOTATIONS],
            &subst,
        )
        .expect("shipped python config parses")
    }

    #[test]
    fn python_config_loads_and_substitutes() {
        let cfg = python_test_config();
        assert_eq!(cfg.language_id, "python");
        assert_eq!(cfg.server_cmd, vec!["ty".to_string(), "server".to_string()]);
        // The pinned target reached the init options.
        let ver = cfg
            .init_options
            .pointer("/configuration/environment/python-version")
            .and_then(|v| v.as_str());
        assert_eq!(ver, Some("3.11"));
    }

    #[test]
    fn effect_map_splits_specific_and_package() {
        let cfg = python_test_config();
        // Whole-module row.
        assert_eq!(
            cfg.effect_of("subprocess::run", "subprocess"),
            Some(Effect::Process)
        );
        // Specific row overrides the (absent) whole-`os` effect.
        assert_eq!(cfg.effect_of("os::system", "os"), Some(Effect::Process));
        // Class-qualified pathlib I/O method.
        assert_eq!(
            cfg.effect_of("pathlib::Path.mkdir", "pathlib"),
            Some(Effect::Fs)
        );
        // The bare constructor / pure path algebra is not an effect.
        assert_eq!(cfg.effect_of("pathlib::Path", "pathlib"), None);
        assert_eq!(
            cfg.effect_of("builtins::open", "builtins"),
            Some(Effect::Fs)
        );
        // A pure os helper: no specific row, no whole-`os` row.
        assert_eq!(cfg.effect_of("os::getcwd", "os"), Some(Effect::Env));
        assert_eq!(cfg.effect_of("posixpath::join", "posixpath"), None);
        // Whole-module effects inherit to submodules (Python): a call in
        // `urllib.request` / `http.server` is net because `urllib` / `http` is.
        assert_eq!(
            cfg.effect_of("urllib.request::urlopen", "urllib.request"),
            Some(Effect::Net)
        );
        assert_eq!(
            cfg.effect_of(
                "http.server::BaseHTTPRequestHandler.send_header",
                "http.server"
            ),
            Some(Effect::Net)
        );
        // But inheritance never fabricates an effect for an unrelated submodule.
        assert_eq!(
            cfg.effect_of("importlib.util::find_spec", "importlib.util"),
            None
        );
    }

    #[test]
    fn library_pack_merges_into_the_effect_map() {
        // The third-party library pack (`python-libs.toml`) is merged onto the
        // stdlib set, so the extractor's effect map seeds SQLAlchemy's execution
        // surface directly by declaration provenance.
        let cfg = python_test_config();
        assert_eq!(
            cfg.effect_of("sqlalchemy::Session.execute", "sqlalchemy"),
            Some(Effect::Db)
        );
        assert_eq!(
            cfg.effect_of("sqlalchemy::create_engine", "sqlalchemy"),
            Some(Effect::Db)
        );
        // rich / PyYAML are pure — they carry no `[roots]` effect, so the
        // extractor seeds no root; hinzu-core's pure vouch clears them instead.
        assert_eq!(cfg.effect_of("yaml::safe_load", "yaml"), None);
        assert_eq!(
            cfg.effect_of("rich.console::Console.print", "rich.console"),
            None
        );
    }

    #[test]
    fn provenance_classifies_stdlib_thirdparty_and_real_stdlib() {
        let cfg = python_test_config();
        // ty's vendored typeshed stdlib.
        assert_eq!(
            cfg.package_of("/root/.cache/ty/vendored/typeshed/abc123/stdlib/subprocess.pyi"),
            Some(("subprocess".to_string(), Origin::Stdlib))
        );
        // A nested stdlib package's __init__ normalizes to the bare module.
        assert_eq!(
            cfg.package_of("/x/typeshed/abc/stdlib/pathlib/__init__.pyi"),
            Some(("pathlib".to_string(), Origin::Stdlib))
        );
        // An installed third-party package.
        assert_eq!(
            cfg.package_of("/venv/lib/python3.11/site-packages/yaml/__init__.py"),
            Some(("yaml".to_string(), Origin::Module))
        );
        // The interpreter's REAL stdlib (the headless-CI resolution fix): a def
        // under `.../python3.11/<module>.py`, NOT site-packages, is stdlib.
        assert_eq!(
            cfg.package_of("/usr/lib/python3.11/subprocess.py"),
            Some(("subprocess".to_string(), Origin::Stdlib))
        );
        assert_eq!(
            cfg.package_of("/usr/lib/python3.11/importlib/util.py"),
            Some(("importlib.util".to_string(), Origin::Stdlib))
        );
        // lib-dynload C-extension shims are not importable stdlib module names.
        assert_eq!(
            cfg.package_of("/usr/lib/python3.11/lib-dynload/_socket.cpython-311.so"),
            None
        );
    }
}
