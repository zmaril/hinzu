# `hinzu api-diff` — cross-language public-surface conformance

`hinzu api-diff` answers the question you ask *after* a port exists:

> Does the target package expose the **same public contract** as the source —
> which of the source's exported items have a matching target item, which have a
> shape that differs, and which are missing entirely?

It takes two [`hinzu api`](./api.md) reports — a SOURCE package's public API and a
TARGET package's public API, typically in different languages — and emits a
stable JSON **conformance grade**. Where [`hinzu port-diff`](./port-diff.md) bands
*files* by how much of the source's internal dependency graph has a target
counterpart (porting *progress*), `api-diff` grades the *public surface* (porting
*conformance*). They are complementary; see [Composing with
port-diff](#composing-with-port-diff).

The matching engine is [`hinzu_core::apidiff`](../crates/hinzu-core/src/apidiff.rs)
— a **pure** function of its two reports plus a naming ruleset. All file I/O (the
two report reads, the config load) is in the CLI; the core reads no files and
spawns no processes, so it stays inside hinzu-core's functional-core region (the
`self-check` CI job proves it).

## Usage

```sh
# 1. generate the two api reports (see notes/api.md)
hinzu api /path/to/pi/packages/ai        --out src-ai.json   # TypeScript source
hinzu api /path/to/pidgin/crates/pidgin-ai --out tgt-ai.json  # Rust target

# 2. grade the target's surface against the source's
hinzu api-diff --source src-ai.json --target tgt-ai.json \
  --config notes/port-pi-pidgin.toml --package ai --out apidiff-ai.json
```

Generating the reports is `hinzu api`'s job; `api-diff` only consumes them.

### What the CLI needs

| flag | meaning |
| --- | --- |
| `--source <api.json>` | the SOURCE package's `hinzu api` report — the contract to match |
| `--target <api.json>` | the TARGET package's `hinzu api` report — the port being graded |
| `--config <toml>` + `--package <name>` | pull the **same** naming rules + package→crate mapping [`port-diff`](./port-diff.md) uses, so cross-language comparisons don't false-miss on convention renames. Optional. |
| `--out <file>` | where to write the report JSON (defaults to stdout) |

Without `--config`, the built-in TypeScript→Rust naming ruleset is used
(`camelCase`→`snake_case` functions, kebab→snake file segments, PascalCase types
and SCREAMING consts kept). With `--config`, the selected package's rules and
crate prefix are used verbatim — the identical ruleset the port-diff matcher keys
on, so the two commands never disagree about whether `streamText` and
`stream_text` are the same name.

## Matching

For each source public item the engine asks: is there a target item with the same
**name** (after normalization) and a compatible **kind**, and if so does its
**shape** line up?

1. **Normalize names.** Both sides' item names are lowered with the naming rules
   before comparison, so a convention rename is not a difference: `streamText` ↔
   `stream_text` (camelCase→snake_case), `MyType` ↔ `MyType` (PascalCase kept),
   `MAX_N` ↔ `MAX_N` (SCREAMING kept). A **PascalCase function** name (a factory
   like `StringEnum`) is snake-folded rather than kept, since the keep-Pascal rule
   is a *type*-name rule, not a callable one. Field and variant names are
   normalized the same way, so `maxTokens` ↔ `max_tokens` fields do not read as
   missing/extra. Each item's module is anchored on its defining file with the
   same source/target file→module lowering port-diff uses, and is used as a
   match-quality preference (an exact-module match wins) and as evidence.

2. **Classify each source item:**
   - **matched** — a target item matches by name + kind-class *and* the shape is
     compatible.
   - **signatureMismatch** — matched by name + kind-class, but the shape differs.
     The specific differences are reported (parameter count, missing/extra fields,
     missing/extra variants, and — same-language only — parameter/return types).
   - **missing** — no target item matches by name + kind-class.
   - **extra** — a target item with no source counterpart (target-only surface).

3. **Carry evidence.** Every pairing carries the source id / file / line and the
   target id / file / line, so a human or agent can open both and verify.

4. **Grade.** The overall **conformance** is `matched / (matched + missing +
   signatureMismatch)` — the fraction of the source public surface with a
   compatible target match. `extra` items are target-only and are not in the
   denominator. A per-kind breakdown and the raw counts sit beside it.

### The kind-equivalence table

Two items are a candidate pairing only if they share a **kind-class**.
Compatibility is deliberately lenient across languages, because a ported concept
may land in a different-but-equivalent shape:

| kind-class | member kinds |
| --- | --- |
| **Callable** | `function`, `method` |
| **Type** | `struct`, `enum`, `trait`, `class`, `interface`, `typeAlias`, `record` |
| **Const** | `const` |
| **Namespace** | `namespace` |

So a TypeScript `interface` matches a Rust `struct` or `trait`, a `typeAlias`
matches an `enum`, and a free `function` matches a `method` — the exact cross-kind
pairing is recorded in the item's `note` (`"interface↔struct"`). Anything outside
these classes is not graded. `external:*` modules (re-exported third-party
surface, not the package's own declared contract) are skipped on both sides.

### Shape comparison — a signal, not a proof

Types are **rendered strings** and cross-language type equivalence is
*approximate* (`string` ↔ `String`, `number` ↔ `f64`, `Foo[]` ↔ `Vec<Foo>`). So a
`signatureMismatch` is a **signal, not a proof** — exactly like port-diff's
non-DONE bands, it points a human at something to look at. To keep the grade
honest, the classification is driven only by **structural** facts that survive
rendering:

- **Callables** — parameter **arity**. A count difference is flagged; the rendered
  parameter and return types are compared *only* when the two sides share a
  language (a same-language api-diff, e.g. two Rust crates), where the strings are
  actually comparable.
- **Aggregates** (struct / class / interface / record) — the **field-name set**
  (normalized), listing each missing or extra field.
- **Enums** — the **variant-name set**, listing each missing or extra variant.

A source aggregate matched onto a `trait` / `enum` / `typeAlias` (which carry no
comparable named-field shape) is not penalized for fields it could not hold.

### Limitations (stated honestly)

- **Structural, not behavioral.** A `matched` verdict means the name, kind-class,
  and structural shape line up — not that the two implementations behave the same.
- **Cross-language type strings are advisory.** Two callables with the same arity
  but entirely different parameter types read as `matched` cross-language, since
  `string` and `String` are not a real difference. The rendered types travel in
  the evidence for a human to judge.
- **Optional / overloaded parameters make arity approximate.** A TypeScript
  trailing optional argument dropped in the Rust port shows as a `paramCount`
  mismatch even though the port may be faithful; `hinzu api` also captures only
  the first overload. Read a `paramCount` aspect as "look here", not "this is
  wrong".
- **A different shape reads as missing.** A source factory `function` ported as a
  Rust `struct` + registry is `missing` (no same-kind-class target), even though
  the capability exists — the surfaces genuinely differ in shape.

## The schema

Every report is a single JSON object. Field names on the wire are camelCase
(`signatureMismatch`, `sourceId`, `byKind`); the schema version is stamped so a
consumer can branch on it.

### `ApiDiffReport`

| field | type | meaning |
| --- | --- | --- |
| `hinzuApiVersion` | `u32` | schema version (shared with `hinzu api`) |
| `source` | `PackageInfo` | the source package (the contract) |
| `target` | `PackageInfo` | the target package (the port graded) |
| `summary` | `DiffSummary` | the counts + overall conformance |
| `byKind` | `KindBreakdown[]` | the per-kind breakdown, sorted by kind |
| `items` | `ApiDiffItem[]` | every graded item, sorted by `(status, kind, name)` |

`PackageInfo` is the same shape `hinzu api` emits (name, language, root, version).

### `DiffSummary`

| field | type | meaning |
| --- | --- | --- |
| `matched` | `usize` | source items with a compatible target match |
| `missing` | `usize` | source items with no target match |
| `signatureMismatch` | `usize` | source items matched by name + kind but a differing shape |
| `extra` | `usize` | target items with no source counterpart |
| `conformance` | `f64` | `matched / (matched + missing + signatureMismatch)`, 3 dp |

### `KindBreakdown`

`{ kind, matched, missing, signatureMismatch, extra }` — one row per kind seen.

### `ApiDiffItem`

| field | type | meaning |
| --- | --- | --- |
| `name` | `string` | the item's short name (source spelling, or target for `extra`) |
| `kind` | `string` | the item kind (source kind, or target kind for `extra`) |
| `status` | `string` | `matched` / `signatureMismatch` / `missing` / `extra` |
| `sourceId` | `string?` | the source item's id (`null` for `extra`) |
| `sourceFile` / `sourceLine` | `string?` / `u32?` | the source item's file + line |
| `targetId` | `string?` | the matched (or `extra`) target item's id |
| `targetFile` / `targetLine` | `string?` / `u32?` | the target item's file + line |
| `mismatch` | `Aspect[]?` | for `signatureMismatch`: the specific differences |
| `note` | `string?` | a short note (e.g. the cross-kind pairing `"interface↔struct"`) |

Each `mismatch` aspect is `{ aspect, source, target }`, where `aspect` names the
facet (`"paramCount"`, `"missingField"`, `"extraField"`, `"missingVariant"`,
`"extraVariant"`, `"returnType"`, `"paramType[i]"`) and `source` / `target` carry
the two rendered sides (one is empty when the facet exists on only one side).

### Determinism

The same inputs always produce the same bytes: `items` are sorted by
`(status, kind, name)` and then by id, `byKind` by kind. No timestamps and no
absolute paths are introduced — the file paths are whatever the two reports carry.

## Composing with port-diff

`api-diff` and [`port-diff`](./port-diff.md) answer different questions and read
best side by side:

- **port-diff** bands *files* by how much of the source's internal dependency
  **graph** has a target counterpart — DONE / PORTED / STARTED / NOT-STARTED. It
  measures porting **progress**: how far along the work is.
- **api-diff** grades the *public surface* item by item — matched / mismatch /
  missing / extra. It measures **conformance**: whether the thing that was ported
  exposes the same contract.

They share the same naming ruleset (the port config's `[naming]` block), so a
symbol port-diff matches structurally and an item api-diff matches by surface are
normalized identically. A file can be `PORTED` by graph coverage while its public
interface still has `missing` items (internal helpers ported, the exported
contract not yet complete), or fully surface-conformant while port-diff still
bands it `STARTED` (the contract is there, the internal graph not fully
reconciled). Reading both gives progress *and* conformance.

## The pi → pidgin validation

Run against the real `pi` (TypeScript) → `pidgin` (Rust) `ai` package pair — the
same pair port-diff validates on — with the shared naming rules from
[`notes/port-pi-pidgin.toml`](./port-pi-pidgin.toml):

```
summary: matched 124, missing 206, signatureMismatch 58, extra 527
conformance: 0.32   (124 / 388 source public items)
```

The grade reads as genuinely-unported public surface, not convention noise:

- **matched** — `builtinProviders` → `builtin_providers`, `calculateCost` →
  `calculate_cost`, `clampMaxTokensToContext` → `clamp_max_tokens_to_context`, the
  `AssistantMessage` interface (its `responseModel` / `stopReason` / `errorMessage`
  fields all align with the Rust `response_model` / `stop_reason` / `error_message`
  after normalization), and `StringEnum` → `string_enum` (a PascalCase factory
  function correctly snake-folded).
- **signatureMismatch** — `completeSimple` (TS 3 params → Rust 4: the Rust method
  splits out an explicit `Option<&AbortSignal>` the TS API folds into options);
  `builtinModels` (TS takes an options arg, Rust takes none); `AnthropicOptions`
  (interface ↔ struct, a handful of options fields genuinely dropped in the port).
  Each is a real difference a reviewer can confirm.
- **missing** — the provider factory functions (`anthropicProvider`,
  `amazonBedrockProvider`, …) and the model-list consts (`ANTHROPIC_MODELS`, …):
  pidgin exposes providers through registries and models through `models()`
  methods, not free functions and top-level consts, so there is no same-kind-class
  target. Genuinely unported *as that shape*, verifiable in the pidgin source.
- **extra** — 527 target-only items: the Rust port exposes far more inherent
  `method`s (157 vs 11), `struct`s, and `enum`s than the TypeScript surface
  declares. Expected — a Rust port surfaces more named types and methods than a
  structurally-typed TypeScript module does.

The convention normalization is load-bearing: before it was applied to **field**
names, the same run scored 0.271 with 39 `interface` mismatches; normalizing
`maxTokens` ↔ `max_tokens` recovered 18 of them to `matched`, and snake-folding
the one PascalCase function recovered `StringEnum`. What remains is real
divergence, not spelling.

Qualitatively this tracks how port-diff bands the `ai` package: a partially-ported
package where the *internal* graph is further along than the *public contract*,
which is exactly what a conformance grade lower than the structural coverage
should show.
