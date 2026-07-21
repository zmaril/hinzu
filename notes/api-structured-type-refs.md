# `hinzu api` v2 — structured type references (design note)

> Status: **design, not implemented.** This note proposes an *additive* evolution
> of the [`ApiReport`](./api.md) schema. Nothing here changes an extractor or the
> wire format yet; it records the shape we would build and, honestly, what it does
> and does not buy.

## The problem: types are strings

Every type in an `ApiReport` today is a **rendered string**. A field's `ty`, a
signature's `returnType` and each `Param.ty`, a `typeAlias`'s `aliasTarget`, a
`const`'s `constType`, the entries of `implements` — all are the extractor's
best human-readable rendering of a type (`checker.typeToString` for TypeScript,
rustdoc's rendered path for Rust). The fidelity block says so in as many words:

> Types are rendered strings from the TypeChecker (typeToString), not
> cross-referenced ids.

That is fine for a human reading the report, and it is enough for a diff that
compares byte-for-byte. It is **not** enough for a consumer that needs to know
*which symbol* a type names. A string like `Promise<RpcSlashCommand[]>` or
`Record<string, BashResult | undefined>` carries the answer only implicitly: to
act on it, the consumer has to re-parse the string it was just handed.

Two consumers do exactly this today:

- **The `api-fluessig` converter.** To resolve a field or parameter type to a
  model, it strips comments, peels `Promise<…>` / `Array<…>` / `[]`, splits
  unions, and matches indexed-access and generic forms against the item ids it
  has seen — including across `--context` reports. When the string does not
  parse into something it can model, it degrades to `Json`. Its own coverage
  sidecar names the failure modes bluntly: `unparsable type expression`,
  `unresolved type reference`, `unmodeled generic type`. Each is a place where a
  string had to be re-interpreted and the interpretation then failed.
- **`api-diff` (#34).** A structural API diff wants to say "field `x` changed
  from `Foo` to `Bar`" and, better, "the referenced symbol moved from module `a`
  to module `b`" — not merely "this string differs." With only strings it must
  re-parse both sides and re-derive the reference graph the extractor already
  computed, then guess whether two differently-rendered strings denote the
  same symbol.

In both cases the extractor *knew* the referenced symbol at extraction time —
the TypeChecker (or rustdoc) resolved it — and then threw that knowledge away by
rendering to a string. The consumer reconstructs it, lossily.

## The proposal: emit the reference, keep the string

Add an **additive** structured companion to every rendered type string: a list
of the symbol references the string denotes, captured at extraction time when the
answer is still exact. The string stays, unchanged, for back-compat and for human
eyes; the structured field sits *beside* it.

Concretely, a new optional `typeRefs` array wherever a type string appears
(`Field`, `Param`, `Signature.returnType`, `ApiItem.aliasTarget`,
`ApiItem.constType`, `implements`). Each entry:

| field | key | type | meaning |
| --- | --- | --- | --- |
| name | `name` | `string` | the referenced symbol's short name (`BashResult`) |
| kind | `kind` | `string` | `interface` / `typeAlias` / `enum` / `class` / `primitive` / `builtin` / `typeParam` / `external` |
| id | `id` | `string?` | the referenced item's stable `id` when it resolves to an item in *this* report or a known `--context` report — the same id scheme `ApiItem.id` uses, so a consumer joins on it directly |
| origin module | `originModule` | `string?` | the declaring module path (`src/core/bash-executor`), `null` for primitives/builtins |
| origin package | `originPackage` | `string?` | the owning package when the reference crosses a package boundary (`@earendil-works/pi-coding-agent`); `null` when same-package |

The string `Promise<RpcSlashCommand[]>` on a `returnType` would carry
`typeRefs: [{ name: "RpcSlashCommand", kind: "interface", id:
"src/modes/rpc/rpc-types#RpcSlashCommand", originModule: "src/modes/rpc/rpc-types",
originPackage: null }]` — `Promise` and the array wrapper are structure the
consumer already models; the *reference* is what it could not previously recover
without parsing.

Design constraints that keep it honest:

- **Purely additive.** No existing field changes type or meaning. A v1 consumer
  that never reads `typeRefs` sees byte-compatible reports modulo the new key,
  and can ignore it. `build_api`'s deterministic sort and source-order guarantees
  are unchanged; `typeRefs` sorts with its owner.
- **The string remains authoritative for rendering.** `typeRefs` is a resolution
  aid, not a re-encoding of the whole type. It does not attempt to represent
  arbitrary structural types (mapped types, conditional types, deep generic
  nests) — it lists the *named symbol references* inside the string. Structure
  the consumer already handles (arrays, `Promise`, unions) stays in the string.
- **Resolution degrades honestly, like everything else in `api`.** A reference
  that resolves to a report item gets an `id`; one that resolves only to a name
  (an unmodeled or truly-external type) gets `id: null` with `kind: "external"`.
  The consumer branches on `id` presence instead of on a parse succeeding.

## Versioning: `HINZU_API_VERSION` 1 → 2

[`HINZU_API_VERSION`](../crates/hinzu-core/src/api.rs) is `1` today and, per
[api.md](./api.md#determinism-and-versioning), "rises only on a breaking change."
This change is additive, so v1 consumers are unaffected by construction. We still
bump `1 → 2` — not as a break, but as a **capability signal**: a consumer that
wants structured refs can require `hinzu_api_version >= 2` and know the field is
populated, rather than probing for a key that an older extractor would silently
omit. A v1 consumer keeps working against a v2 report unchanged. The bump is the
cheapest way to make "structured refs are present here" a first-class, branchable
fact rather than a guess.

## What it buys

- **Converter robustness.** The `api-fluessig` string parser — comment
  stripping, `Promise`/array peeling, indexed-access and generic matching — can
  be retired in favor of joining on `typeRefs[].id`. The `unparsable type
  expression` and `unresolved type reference` degradation classes shrink to the
  genuinely-unmodelable cases instead of "the string shape defeated the parser."
  Resolution stops depending on two extractors happening to render the same
  symbol identically.
- **`api-diff` structural matching (#34).** A diff can compare *references*, not
  strings: same `id` ⇒ same symbol even if the rendering drifted; changed
  `originModule` / `originPackage` ⇒ a symbol moved. Rename and move detection
  become data lookups instead of string heuristics.
- **Future consumers.** Anything that wants the reference graph of a public
  surface — an import-impact analysis, a cross-package reachability check, a
  docs cross-linker — reads it directly instead of re-deriving it. The `id`
  scheme already cross-references `ApiItem.id` and the facts symbol ids, so the
  graph is a join, not a parse.

## Honest scope: zero orchestrator parity gain

This is a **robustness** change, not a **coverage** change, and it is worth
stating plainly so it is not mis-sold.

The measured orchestrator conversion plateau — **33/39 ops cleanly typed, 70/72
source items** carried through — is *already reached today via string parsing*.
Structured refs would not move those numbers: the same references that resolve by
string-matching would resolve by id. The gap-2 extractor fix (surfacing
`export { type X }` re-exported types like `BashResult` and `RpcSlashCommand` as
public items) is what actually reduced unresolved references (10 → 8) and pulled
those two into real models — and it did so *within* the string-parsing world,
with no schema change at all.

So the value of v2 is not a higher score. It is that the score stops being
defended by a string parser: the converter gets simpler and less fragile,
`api-diff` gets a structural basis, and future consumers read the reference graph
directly instead of rebuilding it. If the only goal were the orchestrator parity
number, this note would not be worth implementing. The goal is to stop paying,
again and again, to reconstruct a fact the extractor already knew.

## Not in scope

- A full structural type IR (mapped/conditional/deep-generic encoding). `typeRefs`
  lists named references inside the existing string, deliberately; a total IR is a
  separate, much larger design.
- Changing any rendered string, sort order, or existing field.
- Any extractor rewrite: each extractor gains a resolve-and-emit pass over the
  types it already computes; the pure `build_api` gains a versioned field and its
  normalization.
