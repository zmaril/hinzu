//! The tree-sitter **syntactic** layer of the reference-edge rung. It is the
//! second, syntax-level fact source layered under the LSP: `callHierarchy/
//! outgoingCalls` is call-only, so a function used as a *value* — passed as a
//! callback, stored, returned, decorated — and any use at **module scope** (which
//! call hierarchy never anchors, since it is not inside a function definition) are
//! both invisible to it. This pass walks the source with tree-sitter and
//! enumerates those non-call **reference sites**; [`crate::extract`] then resolves
//! each through the same `textDocument/definition` → provenance → effect path the
//! call resolver uses, and emits `reference` edges.
//!
//! It is deliberately Python-only for now (the query below is Python's grammar).
//! Go and other LSP-tier languages are the same shape — a per-grammar node/field
//! table over this identical enumeration — and are a documented follow-up.
//!
//! What it enumerates, and what it does NOT:
//!   * a name (identifier or `a.b` attribute) in a **value position** — a call
//!     argument (`f(g)`), an assignment right-hand side (`x = g`), a default
//!     parameter (`def h(cb=g)`), a `return`, a collection element, a `pair`
//!     value, a bare decorator (`@deco`) — is a **reference**.
//!   * the **callee** of a call (`g` in `g(x)`) is NOT re-emitted **inside a
//!     function** — call hierarchy already emits that `call` edge. It IS
//!     enumerated at **module scope**, where call hierarchy emitted nothing, so
//!     import-time effects (`create_engine(...)`, a SQLAlchemy `Column(...)` in a
//!     class body) become visible. The caller decides by source position: a site
//!     with no enclosing collected function is attributed to a synthetic
//!     per-file `<module>` definition.
//!   * `import` / `from … import` statements are skipped wholesale — importing a
//!     name is not using it.

use tree_sitter::{Node, Parser};

/// One syntactic reference site: a name used as a value (or a module-scope call
/// callee). The caller resolves its definition at `(query_line, query_char)` and
/// attributes the resulting edge to the function enclosing `site_line` (or to the
/// file's `<module>` node when nothing encloses it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefSite {
    /// 0-based line of the token to resolve (`textDocument/definition` position).
    pub query_line: u32,
    /// 0-based column (byte offset) of that token. Python identifiers are ASCII,
    /// so the byte column equals the UTF-16 character offset an LSP position wants
    /// for the identifier itself; a non-ASCII run earlier on the same line is the
    /// only case this approximates, and never affects the resolved token.
    pub query_char: u32,
    /// 1-based line of the site, for caller attribution and edge evidence.
    pub site_line: u32,
    /// Whether this site is the callee of a call. A callee is enumerated so the
    /// caller can emit it at module scope, but the caller drops it inside a
    /// function (call hierarchy already covers that `call` edge — the dedupe the
    /// design calls for, done by position rather than by re-matching text).
    pub is_call_callee: bool,
}

/// Parse `source` as Python and enumerate every reference site in it. A parse
/// failure (or a grammar mismatch) yields an empty list — the pass is additive,
/// so a file it cannot read simply contributes no reference edges rather than
/// failing the run.
pub fn python_reference_sites(source: &str) -> Vec<RefSite> {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .is_err()
    {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let mut sites = Vec::new();
    walk(tree.root_node(), &mut sites);
    sites
}

/// Recurse into `node`'s named children, classifying each as a reference site or
/// descending further. An `import` subtree is pruned whole.
fn walk(node: Node, sites: &mut Vec<RefSite>) {
    let kind = node.kind();
    if kind == "import_statement" || kind == "import_from_statement" {
        return;
    }
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let child = cursor.node();
        if child.is_named() {
            let field = cursor.field_name();
            handle(child, node, field, sites);
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

/// Classify one `child` (occupying `field` in `parent`): record it as a reference
/// site and stop, or recurse into it.
fn handle(child: Node, parent: Node, field: Option<&str>, sites: &mut Vec<RefSite>) {
    let k = child.kind();
    if k == "identifier" || k == "attribute" {
        if let Some(is_callee) = classify(parent.kind(), field) {
            if let Some(site) = make_site(child, is_callee) {
                sites.push(site);
            }
            // A name expression is a leaf use: do not descend into it (its inner
            // object identifier is part of this expression, not a separate use).
            return;
        }
    }
    walk(child, sites);
}

/// Whether a name in `field` of a `parent_kind` node is a reference site, and if
/// so whether it is the callee of a call. `None` means "not a site — recurse."
fn classify(parent_kind: &str, field: Option<&str>) -> Option<bool> {
    // The callee of a call: `g` in `g(x)`.
    if parent_kind == "call" && field == Some("function") {
        return Some(true);
    }
    // Value positions — a name used as a value rather than called.
    let is_ref = match parent_kind {
        // A positional call argument: `f(g)` (positional args carry no field).
        "argument_list" => field.is_none(),
        // A keyword argument value: `f(cb=g)`.
        "keyword_argument" => field == Some("value"),
        // An assignment right-hand side: `x = g`, `x += g`.
        "assignment" | "augmented_assignment" => field == Some("right"),
        // A default parameter value: `def h(cb=g)`.
        "default_parameter" => field == Some("value"),
        // A returned value: `return g`.
        "return_statement" => true,
        // A collection element / dict value: `[g]`, `(g,)`, `{g}`, `{k: g}`.
        "list" | "tuple" | "set" => true,
        "pair" => field == Some("value"),
        // A bare decorator name: `@deco` (a `@deco(...)` decorator wraps a call,
        // whose callee is handled above).
        "decorator" => true,
        // A bare expression statement: `obj.method` on its own line.
        "expression_statement" => true,
        _ => false,
    };
    is_ref.then_some(false)
}

/// Build a [`RefSite`] for a name node. For an `a.b.c` attribute the definition is
/// resolved at the trailing property (`c`), so the member — not the receiver —
/// drives resolution; for a plain identifier it is the identifier itself.
fn make_site(node: Node, is_call_callee: bool) -> Option<RefSite> {
    let token = if node.kind() == "attribute" {
        node.child_by_field_name("attribute").unwrap_or(node)
    } else {
        node
    };
    let q = token.start_position();
    let s = node.start_position();
    Some(RefSite {
        query_line: q.row as u32,
        query_char: q.column as u32,
        site_line: s.row as u32 + 1,
        is_call_callee,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site_tokens(src: &str) -> Vec<(u32, u32, bool)> {
        python_reference_sites(src)
            .into_iter()
            .map(|s| (s.query_line, s.query_char, s.is_call_callee))
            .collect()
    }

    /// A passed-as-argument function IS a reference; the call's own callee is
    /// recorded as a callee (so the emitter can drop it inside a function), never
    /// as a value reference.
    #[test]
    fn passed_as_argument_is_a_reference_callee_is_marked() {
        // `register(handler)` — `register` is the callee, `handler` the value.
        let sites = python_reference_sites("def f():\n    register(handler)\n");
        let refs: Vec<_> = sites.iter().filter(|s| !s.is_call_callee).collect();
        let callees: Vec<_> = sites.iter().filter(|s| s.is_call_callee).collect();
        assert_eq!(refs.len(), 1, "exactly one value reference: {sites:?}");
        assert_eq!(refs[0].site_line, 2);
        assert_eq!(
            callees.len(),
            1,
            "the call's callee is marked, not dropped here: {sites:?}"
        );
        assert!(callees[0].is_call_callee);
    }

    /// The callee of a plain call is not enumerated as a value reference — only as
    /// a callee — so a resolved call is never double-emitted as a reference.
    #[test]
    fn call_callee_is_not_a_value_reference() {
        let sites = python_reference_sites("def f():\n    g(x)\n");
        // `g` is a callee; `x` is a positional-argument reference.
        let refs: Vec<_> = sites.iter().filter(|s| !s.is_call_callee).collect();
        assert_eq!(refs.len(), 1, "only `x` is a value reference: {sites:?}");
        assert!(sites.iter().any(|s| s.is_call_callee), "`g` is a callee");
    }

    /// The higher-order cases: assignment RHS, default parameter, return, and a
    /// collection element are all references.
    #[test]
    fn stored_returned_defaulted_and_collected_are_references() {
        for src in [
            "x = g\n",
            "def h(cb=g):\n    pass\n",
            "def f():\n    return g\n",
            "xs = [g, h]\n",
        ] {
            let refs: Vec<_> = python_reference_sites(src)
                .into_iter()
                .filter(|s| !s.is_call_callee)
                .collect();
            assert!(!refs.is_empty(), "expected a reference in `{src}`");
        }
    }

    /// A module-scope call callee IS enumerated (with `is_call_callee`), so the
    /// emitter can attribute it to `<module>` — the SQLAlchemy import-time case.
    #[test]
    fn module_scope_call_callee_is_enumerated() {
        // `Base = declarative_base()` at module scope.
        let sites = python_reference_sites("Base = declarative_base()\n");
        assert!(
            sites.iter().any(|s| s.is_call_callee && s.site_line == 1),
            "module-scope call callee enumerated: {sites:?}"
        );
    }

    /// An attribute reference resolves at its trailing member, not its receiver:
    /// `register(fs.read_file)` queries the `read_file` column.
    #[test]
    fn attribute_reference_queries_the_member() {
        let src = "def f():\n    register(fs.read_file)\n";
        let refs: Vec<_> = python_reference_sites(src)
            .into_iter()
            .filter(|s| !s.is_call_callee)
            .collect();
        assert_eq!(refs.len(), 1);
        // `    register(fs.read_file)` — `read_file` starts at column 16.
        assert_eq!(refs[0].query_line, 1);
        assert_eq!(refs[0].query_char, 16, "queries the member token");
    }

    /// `import` / `from … import` names are not uses — the pass skips them.
    #[test]
    fn imports_are_not_references() {
        let sites = site_tokens("from sqlalchemy import create_engine\nimport os\n");
        assert!(sites.is_empty(), "imports contribute no sites: {sites:?}");
    }
}
