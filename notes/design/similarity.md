# Structural similarity analysis (`hinzu similar`)

`hinzu similar` finds places where several implementations are structurally
similar enough that a human or an agent should investigate a **shared
abstraction**. It is **advisory and evidence-based**, in the same spirit as the
rest of hinzu: it locates clusters, explains what they share and what differs,
names a likely abstraction family with a confidence, cites the per-language
capability/limitations that bear on the finding, and lists reasons **not** to
consolidate. It never performs a refactor and never claims an abstraction is
definitely correct.

This document is the frozen contract for the shared schema and the algorithm, so
later phases (TypeScript, and any other language) drop in without changing the
engine.

## Design philosophy (hinzu's, applied here)

- **Facts with evidence.** Every finding cites concrete source locations and the
  concrete structural features that are shared vs. varying — never an unexplained
  score.
- **Honest uncertainty, fail-closed.** A syntactic-only extractor cannot be
  certain two signatures share a type or a behaviour, so uncertainty *lowers
  confidence* and appears as explicit `limitations` / `counter_evidence`. It is
  never smoothed into a faked claim.
- **Honest capability edges.** The analysis states what it does and does not
  capture, per `(language, extractor)`, in a `LanguageProfile` shipped as data.
- **Pure core.** `hinzu-core` reads no files (the self-check effect gate enforces
  this). All source reading happens in the CLI/adapter layer; the core receives
  already-parsed structural signatures and returns findings.

## The two layers

```
   source files                     hinzu-core::similarity  (pure)
        │                                    │
        ▼                                    ▼
  extractor (CLI/adapter)  ──signatures──▶  analyze()  ──▶  SimilarityOutput
  reads files, parses AST                   bucket · score · cluster · explain
  → StructuralSignature[]                   + LanguageProfile (fidelity block)
```

The extractor is the only file-reader. Phase 1 ships the **Rust** extractor
(`crates/hinzu-cli/src/structural_rust.rs`, built on `syn`). The engine is
language-neutral: a TypeScript extractor drops in by emitting the same
`StructuralSignature` shape plus a TS `LanguageProfile`, with no change to
`analyze()`.

## The frozen contract (types in `hinzu-core::similarity`, serde JSON)

### `StructuralSignature` — one per function/def

A language-neutral structural fingerprint. Structure only — no identifiers, no
literals, no semantics — so it is comparable across bodies and across languages.

| field | meaning |
| --- | --- |
| `symbol_id` | stable id (Rust: file-and-item-qualified path, e.g. `src/parse.rs::Foo::bar`) |
| `display` | human name |
| `language` | `"rust"` \| `"typescript"` \| … |
| `kind` | `"function"` \| `"impl_method"` \| `"trait_method"` \| `"closure"` \| … |
| `file`, `line_start`, `line_end` | location |
| `arity` | `{ params, results, generics }` |
| `cfg` | control-flow skeleton `{ branch_count, match_arms, loop_count, try_count, return_points, max_nesting }` |
| `stmt_histogram` | node-kind counts (`let`, `call`, `if`, `match`, `loop`, `return`, `assign`, `macro`, `await`, …) |
| `call_sequence` | ordered, normalized callee simple-names (generics/paths stripped) |
| `type_shape` | `{ params: [String], result: String }` — identifiers erased to `_`, constructors kept (`Result<_,_>`, `Vec<_>`, `&_`) |
| `shingles` | k-gram (k=3) hashes over the normalized AST-node-kind sequence, for Jaccard/MinHash |
| `token_len` | normalized size (node-kind count), for length filtering / the min-size gate |
| `features` | optional lang-specific extras (`has_macro`, `is_async`, `has_await`, …) |

Extractors emit `{ "language": …, "extractor": …, "signatures": [ … ] }`
(`SignatureDoc`). `hinzu similar --structural <file>` reads exactly this shape.

**Type-shape erasure rule.** A path segment is a *constructor* (name kept)
precisely when it carries generic arguments; a bare nominal leaf (`Foo`, `u32`,
`String`) erases to `_`. So `Vec<Foo>` → `Vec<_>`, `Result<T, E>` → `Result<_,_>`,
`&str` → `&_`, and two functions with the same shape but different concrete types
match exactly — the strong "same shape, different types" signal.

### `LanguageProfile` — shipped as data, one per `(language, extractor)`

The fidelity block for structural similarity — the exact analogue of the graph's
`Fidelity`. `capabilities` are graded `yes` / `no` / `partial` / `syntactic`
(the last meaning "observed from syntax, not resolved by a type checker"). Keys:
`types_resolved`, `call_targets_known`, `macro_expansion_visible`,
`control_flow_available`, `generics_visible`, `dynamic_dispatch_understood`,
`suggestion_scope`. Plus `abstraction_families` (the only families a finding from
this profile will name) and honest prose `limitations`.

**The Rust/syn profile is honest that it is SYNTACTIC:**

```
types_resolved:              syntactic   (compared by written form, not resolved — aliases look different)
call_targets_known:          syntactic   (matched by name, not resolved to a def)
macro_expansion_visible:     no          (macro bodies opaque)
control_flow_available:      yes
generics_visible:            yes
dynamic_dispatch_understood: no
suggestion_scope:            language_specific
abstraction_families: helper_function, generic_function, trait, macro_rules,
                      proc_macro_derive, builder, enum_dispatch, generated_declaration
```

Its limitations state explicitly: types are compared by written form not
resolved identity (aliases look different); macro invocations are opaque; and
there is no monomorphization (it cannot confirm two generic instantiations are
structurally identical at the type level).

### `Finding` — one per candidate cluster (>= 2 members)

`{ id, members[], pattern, differences[], likely_abstraction, confidence,
confidence_basis, counter_evidence[], profile }`, where:

- `pattern` = `{ summary, shared_features[], similarity, similarity_breakdown }`
  — `shared_features` are the concrete features ~identical across members
  ("identical control-flow skeleton (…)", "same call sequence […]", "same type
  shape …"); `similarity_breakdown` is the per-signal map
  (`shingle_jaccard`, `cfg`, `type_shape`, `call_seq`, `histogram`).
- `differences` = what **varies** — the axes an abstraction must range over
  (differing types, differing callees in matching slots, differing arity/size).
- `likely_abstraction` = `{ family, rationale, language_mechanisms[] }`.
- `confidence` + `confidence_basis` — capped by profile resolution (syntactic).
- `counter_evidence` = reasons **not** to consolidate (too small, cross-file,
  superficial, macro-hidden, types differ, and the always-true "syntactic match
  only" caveat).
- `profile` = the subset of capabilities/limitations relevant to *this* finding.

### Output document (stdout JSON) — mirrors the `graph` convention

```json
{ "hinzu_similarity_version": 1,
  "root": "<path>",
  "languages": ["rust"],
  "profiles": [ LanguageProfile … ],          // the fidelity/capability block
  "params": { "min_similarity", "min_size", "min_statements", "language_filter" },
  "stats": { "signatures_analyzed", "signatures_after_filter",
             "pairs_compared", "pairs_over_threshold", "candidates_found" },
  "candidates": [ Finding … ] }                // sorted by confidence desc
```

## The algorithm (pure, in `analyze`)

1. **Filter trivial defs.** Drop `token_len < min_size` (default 12) or fewer than
   `min_statements` (default 2) histogram nodes.
2. **Generate candidate pairs without O(N²) blowup.** Two passes, unioned into a
   distinct pair set (so each pair is scored once, and `pairs_compared` is
   honest):
   - **Coarse bucketing** by `(param band, cfg-shape band, size band)` — pairs
     within a bucket are compared.
   - **MinHash / LSH** over `shingles` (32 hashes, 8 bands × 4 rows) — pairs
     sharing any band-bucket are compared, catching cross-bucket structural
     matches the coarse key split apart.
3. **Score each pair** as a weighted combination, every signal exposed in the
   breakdown:
   - shingle Jaccard (primary, weight 0.40),
   - type-shape structural match (0.20 — "same shape, different types"),
   - ordered call-sequence overlap via LCS (0.15),
   - cfg-skeleton closeness = `1 − normalized L1` (0.15),
   - statement-histogram cosine (0.10).
4. **Cluster** with union-find over pairs at or above `min_similarity` (default
   0.55). Each connected component of >= 2 is a candidate.
5. **Explain** each cluster:
   - `shared_features` = features ~identical across members; `differences` =
     features that vary.
   - `likely_abstraction` via heuristics:
     - differences confined to **types** (calls + skeleton constant) →
       `generic_function` (or a trait);
     - **same call shape, different callees in matching slots** → `enum_dispatch`
       (trait method per case / enum + match / higher-order fn);
     - near-identical including calls → `helper_function` (with a `macro_rules!`
       option once there are 3+ members);
     - 3+ members sharing one skeleton with variation confined to a few slots →
       `macro_rules`.
   - `confidence` from the cluster similarity, **capped at 0.85** for a
     syntactic-only profile, then docked for small members, opaque macros,
     cross-file spread, and shell-matches-but-calls-diverge (superficiality).
   - `counter_evidence` names the honest reasons against consolidation.

## The Rust extractor (`syn`, syntactic)

`crates/hinzu-cli/src/structural_rust.rs` walks a cargo project's `.rs` files
(skipping `target/`), parses each with `syn::parse_file`, and visits every free
`fn`, inherent/trait impl method, and trait **default** method. Each body is
reduced with a `syn::visit::Visit` walker:

- the AST-node-kind pre-order sequence → shingles (FNV-1a over k=3 grams) +
  `stmt_histogram` + `token_len`;
- `if` / `match` (arm count) / loops / `?` / `.await` / `return` → the `cfg`
  skeleton; block nesting → `max_nesting`;
- call and method-call expressions → the normalized `call_sequence` (last path
  segment / method ident, generics stripped);
- the signature → `arity` + the erased `type_shape`;
- a macro invocation anywhere in the body sets `features.has_macro` (and is
  counted as a `macro` node) — the profile states macro bodies are opaque.

`symbol_id` is `"<file-rel>::<item path>"` (module / impl-type / trait segments +
name), stable and consistent for a prototype. A file that fails to parse is
skipped with a warning rather than sinking the whole run.

## Honest limitations

- **Syntactic only.** Structural sameness does not imply behavioural sameness;
  two identically-shaped type slots may be different types (or aliases of the
  same). This is stated in every finding.
- **Macro bodies are opaque.** Logic hidden inside a macro invocation is
  invisible to the comparison.
- **No monomorphization.** Two generic instantiations cannot be confirmed
  identical at the type level.
- **Call targets are name-matched**, not resolved — two same-named callees in
  different modules look like the same call.
- **`max_nesting` is block-based**, so a `match` arm written without braces
  under-counts nesting slightly.
- **Advisory, not a verdict.** The output is a set of *candidates to
  investigate*, deliberately not a refactor and not a correctness claim.

## Phasing

- **Phase 1 (this):** the pure engine + the language-profile model + the
  subcommand wiring + the Rust/`syn` extractor.
- **Later:** the TypeScript extractor (emits the same signatures + a TS profile),
  cross-language clustering, and richer abstraction-family heuristics.
