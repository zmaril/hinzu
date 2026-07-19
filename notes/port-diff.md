# `hinzu port-diff` — cross-language port-progress diff

`hinzu port-diff` answers the question you ask *during* a port, not before it:

> Of a source package, how much has actually been ported into the target
> language — file by file, symbol by symbol — and which of it is still missing?

It matches a **source** package's symbol [graph](./graph.md) + [plan](./plan.md)
against the **target** port's symbol graph, in a way that survives file
**decomposition** and **relocation** (a source file whose contents were split
across, or moved into, a differently-named target subtree). Where `hinzu graph`
and `hinzu plan` describe the source alone, `hinzu port-diff` is the only command
that reads *both* codebases and reconciles them.

The matching engine is [`hinzu_core::portdiff`](../crates/hinzu-core/src/portdiff/);
this doc covers the **CLI surface** — the config file, the input modes, `--from`
scoping, and how to read the bands. The engine's algorithm (normalization,
distinctive-leaf clustering, tiered matching, graph-confirm) is documented in that
module's own rustdoc.

## Usage

```sh
# pre-extracted graphs (the common path — extraction, especially Rust, is slow)
hinzu port-diff --config notes/port-pi-atilla.toml --package ai \
  --source-graph pi-ai-graph.json --source-plan pi-ai-plan.json \
  --target-graph atilla-ai-graph.json \
  --out report.json --html dashboard.html

# live extraction (source via the TS adapter, target via the Rust StableMIR driver)
HINZU_RUSTC_DRIVER=/path/to/hinzu-rustc-driver \
  hinzu port-diff --config notes/port-pi-atilla.toml --package ai --out report.json

# scoped to what one entry point needs, and which of it is unported
hinzu port-diff --config notes/port-pi-atilla.toml --package ai \
  --source-graph pi-ai-graph.json --target-graph atilla-ai-graph.json \
  --from 'src/providers/all#builtinProviders' --out scoped.json
```

## What the CLI needs

To run a diff, `hinzu port-diff` needs four things, three of which the config
supplies and one you point it at:

1. a **config** (`--config`) naming the source/target language pair, the naming
   rules, and the per-package paths;
2. a **package** (`--package`) selecting one entry from the config;
3. a **source graph + plan** — either extracted live from the package's
   `source_dir`, or supplied pre-extracted via `--source-graph` / `--source-plan`;
4. a **target graph** — either extracted live from the package's `target_dir`, or
   supplied via `--target-graph`.

The conformance manifest (named in the config) is read by the CLI and its
*contents* handed to the engine — the engine never opens a file, so the analysis
core stays a pure function of its inputs (this is what keeps the `self-check` CI
job green; see [notes/self-check.md](./self-check.md)).

## Input modes

Extraction is expensive — the Rust target extraction runs the StableMIR driver
over a full cargo build — so `port-diff` supports **both** live extraction and
pre-extracted overrides, mixable per side:

| flag | effect |
| --- | --- |
| *(none)* | extract the source live from `source_dir`, the target live from `target_dir` |
| `--source-graph <json>` | load a pre-extracted source graph (`hinzu graph --out`) instead |
| `--source-plan <json>` | load a pre-extracted source plan (`hinzu plan --out`); used only when **unscoped** (a `--from` run always rebuilds the plan from the closure) |
| `--target-graph <json>` | load a pre-extracted target graph instead |

Live **Rust** target extraction requires the StableMIR driver: set
`HINZU_RUSTC_DRIVER` to a prebuilt `hinzu-rustc-driver` binary (built on its
pinned nightly). If `target_kind = "rust"` and the variable is unset, the command
fails with an honest message rather than faking an analysis — pass
`--target-graph` to use a graph you extracted earlier.

## `--from` — scope to an entry point's closure

`--from <pattern>` (repeatable) scopes the **source** to the transitive
dependency closure of an entry point *before* the plan is built and the diff is
run, reusing the same resolver as `hinzu graph --from`. The report then covers
**only what that entry point transitively needs**, and which of it is unported —
"everything `builtinProviders` depends on, and nothing else."

A pattern resolves as: exact symbol id → id-suffix / display name → id substring →
file path (all its symbols); the closure is the union over every `--from`. Because
scoping happens before `build_plan`, the wave view is the plan for *just* that
closure, and a scoped run always rebuilds the plan from the closure (any
`--source-plan` override is ignored, with a note on stderr). The resolved closure
size is printed to stderr, e.g.:

```
scoped to closure of src/providers/all#builtinProviders: 61 symbols across 53 files (of 3517)
```

## The config file

One toml describes several same-shape package ports with a single shared naming
ruleset. See [`notes/port-pi-atilla.toml`](./port-pi-atilla.toml) for the working
`pi` → `atilla` config. Structure:

### Top-level

| key | meaning |
| --- | --- |
| `source_kind` | source language / ecosystem tag (`"ts"`); selects the normalization ruleset |
| `target_kind` | target language / ecosystem tag (`"rust"`) |
| `ported_threshold` | symbol coverage at or above which a (non-native) file is banded **PORTED** (`0.6`) |
| `cluster_vote_retain` | fraction of the winning weighted-vote mass a clustered target subtree must retain (`0.6`) |
| `base_dir` | the directory every **relative** path below is resolved against (absolute paths are used as-is) |
| `conformance_manifest` | the conformance manifest, relative to `base_dir`; the CLI reads it and hands the text to the engine |
| `native_status` | the manifest `status` value that marks a module test-verified (`"native"`) |

### `[naming]` — shared, identical across packages

| key | meaning |
| --- | --- |
| `file_segment_case` | how a source file path segment is normalized (`"kebab_to_snake"`: `google-shared` → `google_shared`) |
| `fn_case` | how a function / method leaf is normalized (`"camel_to_snake"`: `convertMessages` → `convert_messages`) |
| `keep_pascal_types` | keep PascalCase type names verbatim (`AnthropicModel`) |
| `keep_screaming_consts` | keep SCREAMING_SNAKE constants verbatim (`MAX_TOKENS`) |
| `strip_suffixes` | compound file suffixes stripped before the extension (`[".lazy"]`: `anthropic-messages.lazy.ts` → `anthropic-messages`) |
| `source_src_prefix` | the source package's leading source directory (`"src"`) |

### `[packages.<name>]` — one per package

`<name>` is the `--package` selector. Each table has:

| key | meaning |
| --- | --- |
| `source_dir` | the source package directory, relative to `base_dir` (the source extraction root) |
| `target_dir` | the target crate directory, relative to `base_dir` (the target extraction root) |
| `strip_crate_prefix` | the target crate prefix on target ids (`"atilla_ai"`) |
| `target_src_prefix` | the **workspace-relative** prefix a target file carries in the emitted graph (`"crates/atilla-ai/src"`); NOT resolved against `base_dir` |
| `conformance_package` | the manifest `package` this crate's conformance modules are filed under; the manifest `src` prefix stripped to recover a source path is `packages/<conformance_package>/` |

The CLI merges the shared `[naming]` block with the selected package's
crate-specific fields into one `PortDiffConfig`. The code is structured so a loop
over `packages` (an all-packages sweep) is a small addition; today a single
`--package` is required, and omitting it lists the available names.

## Reading the report — the bands

Every source file lands in one of four bands. The crucial distinction is
**structural** (graph-derived) vs **conformance-verified**:

| band | meaning | backing |
| --- | --- | --- |
| **DONE** | in the conformance native set | **test-verified** — the only band with test backing |
| **PORTED** | symbol coverage ≥ `ported_threshold`, not native | structural |
| **STARTED** | ≥ 1 symbol matched (or a target subtree mapped), below threshold | structural |
| **NOT-STARTED** | nothing matched | structural |

STARTED and PORTED are **structural**: they come from cross-language symbol
matching, which is a *name-and-structure* signal, not a correctness proof. They
**under-count by design** — an unmatched source symbol is only ever a missed
match, never a fabricated one — so `DONE + PORTED` is a file-level *upper bound*
on what might pass, never a claim that it does. `hinzu port-diff` deliberately
sits between naive file-existence (which over-counts thin wrappers) and
test-verified reality (the DONE oracle), and the report's `conformance_crosscheck`
reconciles the two.

## Honest fidelity limits

The report carries a `fidelity` block; the load-bearing caveats:

- **Structural, not a correctness proof.** Only DONE has test backing. A high
  symbol-match % means the names and call structure line up, not that behavior
  matches. **Graph-confirm** (edge overlap) labels confidence — of a matched
  symbol's internal callees that also matched, what fraction does the target
  counterpart also call — but it never drops a name-match, it only annotates it.
- **The matchable denominator excludes synthetic symbols.** Anonymous /
  callback / positional source symbols are not counted (reported separately as
  `symbols_synthetic_excluded`); the denominator is named, in-source-tree,
  non-external symbols only.
- **A clustered file mapping points at a subtree, not a single file.** When a
  source file's symbols were decomposed or relocated across several target
  modules, its `mapped_target` is the cluster root (a subtree), and coverage is
  still per-symbol but the anchor is broader than one file.
- **DONE depends on the conformance manifest.** If the manifest is missing or
  unreadable, no file is banded DONE and a note is recorded — the structural
  bands are still meaningful, but the test-verified oracle is absent.

## The HTML dashboard

`--html <file>` writes a self-contained dashboard (inline CSS, no external
assets): the headline match % and graph-confirm rate, the DONE / PORTED counts,
the file-band bar, a naive-vs-graph recovery panel (which relocated / decomposed
files the clustering recovers), the conformance cross-check, the per-wave band
mix, the ready-frontier (unported files whose source-deps are all ported), and a
graph-confirmed-vs-name-only table separating structure-preserving ports from
likely name coincidences.
