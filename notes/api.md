# `hinzu api` — the public interface as language-agnostic JSON

`hinzu api` answers a different question from the rest of the toolkit:

> What is a package's **public interface** — the exported functions, methods,
> types, and constants a consumer can rely on — as one stable, language-agnostic
> JSON document?

Where [`hinzu graph`](./graph.md) and [`hinzu plan`](./plan.md) describe a
package's *internal* dependency structure, and [`hinzu port-diff`](./port-diff.md)
reconciles two codebases file by file, `hinzu api` describes only the **declared
public surface**, with real signatures. It reads one package and emits the
contract that package offers the outside world.

Two consumers drive the shape (see [Consumers](#consumers) below):

1. **Porting** — the source package's `ApiReport` is the contract a port must
   match. It is the natural input to a future **api-diff**.
2. **Binding and agent tooling** (fluessig) — functions become candidate ops and
   aggregates become candidate DTOs; the rendered types tell a generator what to
   expose.

The schema is defined once, in [`hinzu_core::api`](../crates/hinzu-core/src/api.rs),
and is identical across languages. Each language has its own **extractor** (the
process that reads source and produces the pieces) in the CLI; the pure
`build_api` in core normalizes and sorts them. This doc covers the CLI surface
and the JSON schema; each extractor's own rustdoc covers its mechanism.

## Usage

```sh
# Rust — via rustdoc JSON (needs a nightly; see the note below)
hinzu api crates/hinzu-core

# TypeScript — via the compiler-API adapter (run `npm install` in adapters/typescript once)
hinzu api /path/to/pkg --lang typescript
hinzu api /path/to/pkg --out api.json          # write to a file instead of stdout

# Python — via ty over its LSP (needs the `ty` binary on PATH)
hinzu api /path/to/pkg --lang python
```

The language is auto-detected from the project's marker file — a `Cargo.toml`
routes to Rust, a `tsconfig.json` / `package.json` to TypeScript, a
`pyproject.toml` / `setup.py` / `setup.cfg` to Python (Rust wins a tie, matching
the fact adapters). `--lang <rust|typescript|python>` forces one. The report goes
to stdout, or to `--out <file>` (with a trailing newline).

### Toolchain requirements

- **Rust** uses `rustdoc --output-format=json`, which is nightly-only. The
  invocation is `cargo +<nightly> rustdoc --lib -Z unstable-options -- -Z
  unstable-options --output-format json`, run in the crate directory into a
  scratch target dir. The nightly is pinned (matching the StableMIR driver's
  toolchain); `HINZU_RUSTDOC_TOOLCHAIN` overrides it. Install the nightly with
  `rustup toolchain install <nightly>`.
- **TypeScript** spawns `node adapters/typescript/analyze.mjs <project> --api`.
  Run `npm install` in `adapters/typescript` once so its `typescript` dependency
  is present. `HINZU_NODE` overrides the interpreter; `HINZU_TS_ADAPTER` the
  script.
- **Python** spawns `ty server` over its LSP. Install ty (`pip install ty` or
  `uv tool install ty`); `HINZU_TY` overrides the binary and `HINZU_PY_VERSION`
  pins the target Python version (default `3.11`).

When the required toolchain is missing, the command fails with an honest message
naming what to install rather than emitting a partial or faked surface.

## Why rustdoc JSON, not StableMIR

The effect pipeline's Rust source is the StableMIR driver, but StableMIR is the
wrong tool for an API surface: it is **monomorphized and reachability-scoped**, so
it sees what a program *reaches*, not what a crate *declares public*. An unused
`pub fn` may never surface as an instance, and generic signatures arrive already
specialized. `rustdoc --output-format=json` gives the opposite — the declared
public surface with signatures, visibility, generics, doc comments, and type
shapes, without needing a reachability root. That is precisely an API command's
contract, so the Rust path shells out to rustdoc rather than reusing the driver.

The extractor does not depend on the `rustdoc-types` crate pinned to one
`FORMAT_VERSION` (which drifts across nightlies); it deserializes only the fields
it needs and records the observed `format_version` in the report's fidelity block.

## The schema

Every report is a single JSON object. The Rust field names are snake_case; a few
keys on the wire are camelCase, noted below. Every field is always present —
unused payloads are `null` or an empty array — so a consumer never has to probe
for a key.

### `ApiReport`

| field | type | meaning |
| --- | --- | --- |
| `hinzu_api_version` | `u32` | schema version, currently `1` — branch on this |
| `package` | `PackageInfo` | the analyzed package |
| `fidelity` | `Fidelity` | how it was extracted and what it does or does not capture |
| `modules` | `Module[]` | the public modules, sorted by `path` |

### `PackageInfo`

| field | type | meaning |
| --- | --- | --- |
| `name` | `string` | the package or crate name |
| `language` | `string` | `"rust"`, `"typescript"`, `"python"`, or `"go"` |
| `root` | `string` | a label for the analyzed target (usually the path given) |
| `version` | `string?` | the package version, when known |

### `Fidelity`

The honesty block: which extractor ran, and every caveat stated plainly.

| field | type | meaning |
| --- | --- | --- |
| `source` | `string` | `"rustdoc-json"`, `"tsc"`, or `"lsp-ty"` |
| `format_version` | `string?` | the extractor's own version (rustdoc's `format_version`, tsc's version); `null` for ty |
| `complete` | `bool` | whether the surface is believed complete (currently always `false` — the caveats are real) |
| `notes` | `string[]` | human-readable limits (excluded-item counts, what is null and why) |

### `Module`

| field | type | meaning |
| --- | --- | --- |
| `path` | `string` | the module path, the grouping key (Rust `krate::mod`; TS the source-relative file; Python the dotted module) |
| `file` | `string?` | the defining file, when known |
| `doc` | `string?` | the module doc comment, when present |
| `items` | `ApiItem[]` | the public items, sorted by `(kind, name)` |

### `ApiItem`

One exported entity. The common fields apply to every `kind`; the payload fields
apply to the kinds noted.

| field | key | type | meaning |
| --- | --- | --- | --- |
| kind | `kind` | `string` | see the [kind vocabulary](#the-kind-vocabulary) |
| id | `id` | `string` | a stable id — the extractor's fully-qualified path (chosen to cross-reference facts symbol ids) |
| name | `name` | `string` | the short item name |
| visibility | `visibility` | `string` | `"public"`, `"crate"`, `"private"`, `"restricted"` |
| module path | `modulePath` | `string` | the module the item is declared in |
| file | `file` | `string?` | the defining file |
| line | `line` | `u32?` | the first source line |
| doc | `doc` | `string?` | the item doc comment |
| generics | `generics` | `string[]` | rendered generic parameters (`T`, `T: Clone`, `'a`) |
| deprecated | `deprecated` | `bool` | whether marked deprecated |
| signature | `signature` | `Signature?` | for `function` / `method` |
| fields | `fields` | `Field[]` | for `struct` / `record` / `interface` / `class` |
| variants | `variants` | `Variant[]` | for `enum` |
| implements | `implements` | `string[]` | implemented / extended traits or supertypes |
| alias target | `aliasTarget` | `string?` | for `typeAlias` — the aliased type |
| const type | `constType` | `string?` | for `const` — the declared type |
| const value | `constValue` | `string?` | for `const` — the value, when a short literal |

### `Signature`

| field | key | type | meaning |
| --- | --- | --- | --- |
| params | `params` | `Param[]` | parameters, in declaration order |
| return type | `returnType` | `string?` | the rendered return type |
| is async | `isAsync` | `bool` | whether the callable is `async` (or Promise-returning) |
| receiver | `receiver` | `string?` | for a method (`"&self"`, `"self"`, or `self`/`cls` for Python); `null` for a free function |
| error type | `errorType` | `string?` | a Rust `Result<_, E>` → `E`; a TS JSDoc `@throws`; `null` when infallible or unknown |
| generics | `generics` | `string[]` | the callable's own generic parameters |

### `Param`

| field | type | meaning |
| --- | --- | --- |
| `name` | `string` | the parameter name (empty for a positional field) |
| `ty` | `string` | the rendered parameter type |
| `optional` | `bool` | whether optional (an `Option<_>` / `?` / `\| None`, or a defaulted argument) |
| `default` | `string?` | the default value, when the language models one and it is short |

### `Field`

| field | type | meaning |
| --- | --- | --- |
| `name` | `string` | the field name (empty for a tuple field) |
| `ty` | `string` | the rendered field type |
| `visibility` | `string` | the field's visibility |
| `doc` | `string?` | the field doc comment |
| `optional` | `bool` | whether optional |

### `Variant`

| field | type | meaning |
| --- | --- | --- |
| `name` | `string` | the variant name |
| `fields` | `Field[]` | the tuple or struct payload, in order (empty for a unit variant) |
| `discriminant` | `string?` | the explicit discriminant, when set |
| `doc` | `string?` | the variant doc comment |

### The kind vocabulary

`kind` is a plain string so it can carry any language's spelling. The values in
use today:

- `function`: a free function (or a function-typed constant in TS/Python).
- `method`: a method or associated function; its owning type is in `receiver`.
- `struct`, `enum`, `trait`: Rust aggregates and the trait interface.
- `class`, `interface`: TS/Python classes and TS interfaces.
- `typeAlias`: a type alias (`aliasTarget` holds the aliased type).
- `const`: a constant or module-level value (`constType` / `constValue`).
- `namespace`: a TS/Python namespace or module value.

Types are **rendered strings** (`Vec<String>`, `Option<Bar>`, `dict[str, str]`) —
honest and portable for v1. Structured, cross-referenced type references are a
documented follow-up (see [api-diff](#porting-the-contract-a-port-must-match)).

### Annotated examples

Rust — a fallible function whose `Result` error type is lifted into
`errorType`:

```json
{
  "kind": "function",
  "id": "hinzu_core::graph::resolve_roots",
  "name": "resolve_roots",
  "modulePath": "hinzu_core::graph",
  "signature": {
    "params": [
      { "name": "graph", "ty": "&GraphOutput", "optional": false, "default": null },
      { "name": "patterns", "ty": "&[String]", "optional": false, "default": null }
    ],
    "returnType": "Result<RootResolution, String>",
    "isAsync": false,
    "receiver": null,
    "errorType": "String",
    "generics": []
  }
}
```

TypeScript — an interface field with an optional member and a rendered union
type:

```json
{
  "kind": "interface",
  "id": "src/api/anthropic-messages#AnthropicOptions",
  "modulePath": "src/api/anthropic-messages",
  "implements": ["StreamOptions"],
  "fields": [
    { "name": "thinkingEnabled", "ty": "boolean | undefined",
      "visibility": "public", "doc": "Enable extended thinking.", "optional": true }
  ]
}
```

Python — a method whose signature comes from ty's hover, with the receiver
lifted out and a default captured:

```json
{
  "kind": "method",
  "id": "widgets.py#Widget.render",
  "modulePath": "widgets",
  "signature": {
    "params": [ { "name": "indent", "ty": "int", "optional": true, "default": "0" } ],
    "returnType": "str",
    "isAsync": false,
    "receiver": "self",
    "errorType": null,
    "generics": []
  }
}
```

## Per-language source and fidelity

Each language draws from the best cheap source of declared public interface. The
fidelity is uneven and stated honestly in every report's `fidelity.notes`.

| language | source | rich | null / approximate |
| --- | --- | --- | --- |
| Rust | `rustdoc --output-format=json` | signatures, generics, visibility, doc comments, field/variant shapes, trait impls, `Result` error type | `throws` not modeled; types are rendered strings; lifetimes elided |
| TypeScript | `tsc` (compiler API, `analyze.mjs --api`) | signatures via `typeToString`, optional/defaults, `isAsync`, interface/class/enum/alias shapes, extends/implements, docs, `@throws` → `errorType` | only the first overload; interface methods appear as function-typed fields; static members omitted; types are rendered strings |
| Python | `ty` over its LSP (`documentSymbol` + `hover`) | function/method signatures where **annotated**, classes and their methods, constants' types, module docstrings | types only where source is annotated; `errorType` always null (`Raises:` not parsed); no cross-file re-export resolution; item doc comments not extracted |

The public surface is defined per language as the package's real exported
interface, not any `export`/`pub` keyword:

- **Rust** — items rustdoc reports as `visibility: "public"` (public re-exports
  included). Auto-trait, blanket, and negative impls are omitted from
  `implements`.
- **TypeScript** — symbols re-exported from the package's entry points
  (`package.json` `exports`, `dist/*` mapped to `src/*`, wildcard subpaths
  expanded), following re-exports via the checker. An `export` never reachable
  from an entry point is excluded; its count rides in `fidelity.notes`.
- **Python** — a module's `__all__` when present, else top-level names not
  starting with `_`. The excluded count rides in `fidelity.notes`. There is no
  cross-file re-export resolution.

## Consumers

### Porting: the contract a port must match

A port's job is to reproduce a source package's public interface in another
language. The source `ApiReport` **is** that contract: every exported function
signature, every type shape, every field the port must offer.

The natural follow-up is an **api-diff**: compare two `ApiReport`s (source
language vs target language) by module and item name plus shape, and report

- **missing** — in the source surface, absent from the target;
- **extra** — in the target, with no source counterpart;
- **signature-mismatched** — present in both, but the parameter or return shape
  differs.

This composes with `port-diff` rather than replacing it. `port-diff` bands files
by graph structure — how much of the source's *internal* dependency graph has a
target counterpart — which measures porting *progress*. An api-diff would instead
grade the *public-surface match* — whether the thing that was ported exposes the
same contract. Progress and conformance are different questions; the two commands
answer them side by side. Item `id`s are chosen as the extractor's
fully-qualified path (Rust's rustdoc path, `src-file#Name` for TS, `file.py#Name`
for Python — the same convention the fact adapters use) so an api-diff can later
cross-reference the facts symbol ids the graph is built from.

### fluessig and agents: ops and DTOs

For a binding generator or a supervising agent deciding what a generated surface
should expose, an `ApiReport` is a menu:

- a `function` is a candidate **op** — its `signature` gives the parameter names,
  rendered types, return type, and async-ness a binding needs to declare it;
- a `struct` / `record` / `interface` / `enum` is a candidate **DTO** — its
  `fields` or `variants` give the shape to generate.

For example, the Rust item above yields an op:

```
resolve_roots(graph: &GraphOutput, patterns: &[String]) -> Result<RootResolution, String>
```

and a struct like `PackageInfo { name, language, root, version }` yields a DTO
with four fields and their rendered types. The generator reads the rendered type
strings to decide what to expose; where a type is another item in the same
report, the shared `id` scheme lets it link them.

## Determinism and versioning

Diffs and CI gates need a stable byte layout, so `build_api` sorts deterministically:

- `modules` by `path`;
- the top-level `items` in each module by `(kind, name)`.

It **preserves source order** of a struct's `fields`, an enum's `variants`, and a
signature's `params`, where position carries meaning. No timestamps and no
absolute paths leak in; the extractors hand over relative paths. The same input
always produces the same bytes.

`hinzu_api_version` is stamped into every report. It is `1` today and rises only
on a breaking change to the shape, so a consumer can branch on it.
