# Curated-library "adopt the library" tier (`hinzu similar --libraries`)

The base `hinzu similar` finds **intra-repo** clusters — several local
implementations that share a shape, worth hand-abstracting into one helper /
generic / trait. This tier adds a second question, pointed **outward**:

> Is this local code the shape of something a library the user already likes
> exposes? If so, say "investigate adopting X" — with the evidence, the
> differences, and the honest reasons it might not fit.

Two example verdicts:

- *"This manual `for x in xs { acc.push(f(x)?) }` loop is the shape of
  `itertools::process_results` / `Iterator::collect::<Result<_,_>>()` — adopt
  the combinator."*
- *"This hand-written `impl Display` + `impl std::error::Error` on an enum is
  what `#[derive(thiserror::Error)]` eliminates."*

It is **advisory, evidence-carrying, and honest about provenance**, exactly like
the rest of hinzu. It never claims the library is the right choice; every
finding carries the reasons it might not be (semantics unverified, adds a
dependency, version skew, curated-pattern incompleteness). It **fails closed**:
a tier that can produce no real match on the input produces no finding.

## The reframe: one structural space, external shapes as data

The base engine compares LOCAL signatures to each other. This tier compares
LOCAL structure to **external shapes** — the shapes libraries expose or the
boilerplate their derives eliminate — expressed in the *same* structural
vocabulary and consumed by the pure core as **data**, exactly like local
signatures. `hinzu-core` stays pure: it reads no files, invokes no `cargo`, and
parses no rustdoc JSON. The CLI layer produces the external-shape data (from
rustdoc JSON, a hand-authored descriptor, or the shipped curated catalog) and
hands it in, the same seam local signatures already cross.

```
   libraries.toml + rustdoc JSON + shipped catalog   (CLI / adapter layer)
                 │  reads files, runs cargo rustdoc
                 ▼
   external shapes (virtual signatures + curated patterns)  ── data ──▶
                                                        hinzu-core::similarity::libraries  (pure)
   local StructuralSignatures + local TypeImplFacts  ── data ──▶  match ──▶ LibraryFinding[]
```

## The five design answers

### 1. Shape extraction, by kind

- **Functions / combinators (Tier A).** A library's exposed generic fn becomes a
  **virtual `StructuralSignature`** in the same structural space: its erased
  `type_shape` (params + result), `arity`, and where-bound-derived call/consume
  shape. Source `rustdoc`: read from `cargo rustdoc --output-format json` (the
  probe works in-container on the pinned nightly — see below), which exposes the
  public signature, generics, and where-bounds but **not** the body. Because
  there is no body, Tier A scores local sigs against a virtual sig on the
  **body-free** signals only — `type_shape` + `arity` — never faking a shingle
  overlap it cannot compute. A `curated` virtual signature (the hand-authored
  fallback) instead encodes the *hand-written shape the combinator replaces*
  (e.g. the loop+`?`+push body), so it can be matched on control-flow shape.
- **Derives / macros (Tier B).** A derive has no run-time signature to compare —
  what it has is the **boilerplate it eliminates**. So Tier B is a curated,
  hand-authored catalog keyed by crate, each entry a **structural predicate**
  over local `impl` / `enum` blocks (`TypeImplFacts`) or local function bodies.
  `thiserror::Error` = a type with a hand-written `impl Display` **and**
  `impl std::error::Error`; `derive_more::From` = an `impl From<T> for E` whose
  body wraps `T` into a single-field variant; `derive_more::Display` /
  `strum::Display` = an `impl Display` whose body is `match self => write!`.

### 2. Macros as eliminated boilerplate

A derive/macro is matched by the **shape of the code it would remove**, not by
any signature. Each curated pattern names the exact impls/blocks it eliminates,
so a finding can point at concrete `file:line` ranges ("`impl Display` at 85-92
and `impl Error` at 94") and say what the one-line derive replaces. Each pattern
records its own provenance string: *curated, hand-authored, may miss variants or
be version-skewed*.

### 3. Layered matching direction

The base engine matches local ↔ local. This tier matches local → external, and
external shapes are the *fixed* side. Tier A reuses the engine's
`type_shape_similarity` scoring (so the definition of "same shape" is identical
to the intra-repo tier). Tier B is a new predicate matcher (`libraries.rs`),
because a derive's elimination target is a structural precondition, not a
similarity score. Local inputs to both tiers are the **singletons and the
already-found clusters** from the base run — so a cluster the base tier says
"hand-abstract this" can also carry "…or adopt library X instead."

### 4. Config surface

A committed `libraries.toml` (precedent: `portdiff_config.rs`), read by the CLI:

```toml
[[libraries.rust]]
crate  = "itertools"
version = "0.13"          # optional, advisory (version-skew note)
kinds  = ["function"]     # function | trait | derive
source = "rustdoc"        # rustdoc | curated
trust  = 0.9              # 0..1, scales the finding's confidence

[[libraries.rust]]
crate  = "thiserror"
kinds  = ["derive"]
source = "curated"
trust  = 0.8
```

`trust` is the `user_trust` factor (a library the user likes → higher). The
shipped curated catalog covers the `curated` crates; a `rustdoc` crate is
doc'd on demand (or falls back to the hand-authored descriptor when rustdoc
JSON is unobtainable, stated honestly in the finding).

Wired as `hinzu similar <path> --libraries <config.toml>`. Absent the flag, the
base run is unchanged.

### 5. Advisory / honest capability records

Two new **source profiles**, shipped as data next to the language profiles:

- **`rustdoc`** — sees public generic signatures, generics, and where-bounds;
  does **not** see macro expansion, private impls, or semantics. So a Tier-A
  match is a *signature-shape* match, never a behaviour match.
- **`curated-pattern`** — only matches the patterns hand-encoded in the catalog;
  it is honest that it may miss variants a real derive handles and may be
  version-skewed against the crate.

`confidence = user_trust × structural_similarity × source_fidelity`
(`source_fidelity`: rustdoc > curated, because rustdoc reads the crate's real
signatures while the curated pattern is a hand transcription). Every
`LibraryFinding` carries `counter_evidence` that **must** include: semantics
unverified (shape-match ≠ behaviour-match), adopting adds a dependency, possible
version skew, and (for curated) curated-pattern incompleteness.

## The finding

`LibraryFinding` is a sibling of `Finding`:

```
{ id, local[], external { library, item, kind, source, version },
  match_basis[], differences[], likely_abstraction { family="adopt_library", … },
  confidence, confidence_basis, counter_evidence[], profile }
```

It is added to `SimilarityOutput` as `library_candidates` (serde-default empty,
so a base run's document is byte-identical to before — the v1 contract holds).

## Feasibility (probed in-container, 2026-07-21)

- **rustdoc JSON is obtainable.** `cargo +nightly-2026-07-18 rustdoc -p itertools
  -- -Zunstable-options --output-format json` produces `target/doc/itertools.json`
  (exit 0), whose `index` entries carry each fn's `sig.inputs` / `sig.output` and
  `generics.where_predicates` — enough to reduce a combinator to a virtual
  `type_shape`. The pinned nightly is the same one the StableMIR driver already
  uses. When rustdoc JSON is not obtainable, the hand-authored descriptor is the
  honest fallback (and the finding says so).
- **Real Tier-B targets exist.** `PromptError`
  (`pidgin-coding/src/core/agent_session/turn.rs:85`) is an enum with a
  hand-written `impl Display { match self … }` and `impl std::error::Error {}` —
  exactly `#[derive(thiserror::Error)]` boilerplate. Dozens more error types
  across pidgin share the shape.
- **Real Tier-A targets exist.** `py_to_json`
  (`pidgin-extensions/src/python/convert.rs:80`) is a manual
  `for item in list.iter() { array.push(py_to_json(&item)?) }` accumulation —
  the shape `itertools::process_results` / `.collect::<Result<_,_>>()` replaces.

## Honest limitations

- **Shape-match is not behaviour-match.** The whole tier is advisory. A matched
  local body may do more (the optional-`cause` append in `ModelsError::fmt` is
  *not* expressible as a single `thiserror` `#[error("…")]` string) — this is why
  every finding lists what **differs**, not just what matches.
- **Adopting adds a dependency.** Always stated as counter-evidence; a one-off
  hand-rolled impl may be cheaper than a new dep + its transitive tree.
- **Curated patterns are incomplete and version-skew-prone.** They are a hand
  transcription of a subset of what each derive does, pinned to no exact version.
- **rustdoc sees signatures, not bodies or semantics.** Tier-A matches are
  signature-shape only; two same-shaped generic fns can do unrelated work.
- **Curated-pattern extraction is syntactic** (syn): it reads a trait impl by the
  written trait path, so a re-exported or aliased trait can be missed.
