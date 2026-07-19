# `hinzu graph` — a dependency graph for AI-assisted porting

`hinzu graph <project>` emits a JSON dependency graph of a codebase, shaped for
porting it to another language (or rewriting it) with an AI agent. The core idea:

> Port the **leaves** first — the things that depend on nothing — then work
> upward, so that whenever the agent ports a symbol, everything that symbol
> depends on is already ported and testable.

**Why "graph", not "DAG".** Real code has cycles — mutual recursion,
back-and-forth calls between two modules — so the dependency graph this command
emits is **not** acyclic in general. What *is* acyclic is its
**condensation**: collapse each strongly-connected component (a call cycle) to a
single node and the resulting graph has no cycles, so a dependencies-first
topological order over it is well-defined. The port order and leaves this command
reports are computed over that condensation; the members of a cycle come out as
one contiguous block you port together. Calling the whole thing a "DAG" would be a
lie whenever the code has a cycle — hence `graph`, with the acyclic view named
honestly as the **condensation** (see the `condensation` field below).

For the operational schedule built *on top* of this graph — files organized into
groups (a PR per group) and topological **waves** (batches portable in parallel)
— see [`hinzu plan`](./plan.md), which reuses this graph's file rollup directly.

It reuses the exact facts the effect engine consumes (`FactSet`: definitions,
call/use edges, effect roots). Where [`effects`](../crates/hinzu-core/src/effects.rs)
reasons *up* the graph (which callers a root reaches), `graph` reasons *down* it
(what each symbol depends on) and hands back a **dependencies-first topological
order** (over the condensation), plus the cycles that must be ported as a unit and
per-node metadata to size and prioritize the work.

## Usage

```sh
# live extraction (routes by project marker: Cargo.toml / package.json / etc.)
hinzu graph ./my-project --out graph.json

# from a pre-extracted fact set (no toolchain needed)
hinzu graph ./my-project --facts facts.json --out graph.json

# from an existing SQLite fact store
hinzu graph ./my-project --db facts.db

# scope to the dependency closure of an entry point (repeatable)
hinzu graph ./my-project --from main --out slice.json

# default output is stdout
hinzu graph ./my-project
```

No policy file is required — the graph does not run the effect-propagation gate.
Effect roots are seeded best-effort from the language's built-in annotation base
(`std.toml`, `node.toml`, `python.toml`, `go.toml`), so `effect_roots` fields are
populated without a `hinzu.toml`; project `[roots]`/`[trust]` overrides are *not*
applied on this path.

## Rooted graphs (`--from`)

`--from <pattern>` restricts the graph to the **dependency closure** of an entry
point: the root plus every symbol reachable by following caller→callee
(dependency-direction) edges — *"everything the root needs, and nothing else"*.
External callees are kept as leaves (the assumed-available boundary), and every
derived field (fan-in/out, transitive counts, the file rollup, the SCC
condensation) is recomputed for the sub-graph, so a rooted graph reads exactly
like a full one, only smaller. A stderr note reports `scoped to closure of
<roots>: N symbols across M files (of TOTAL)`.

Patterns resolve in tiers (first non-empty wins): exact symbol id → id-suffix or
display / leaf name → id substring → a file path (all its symbols). A pattern
that matches nothing errors with near-misses; one that matches many is unioned
and reported. The flag is repeatable and the closure is the union of the roots.
`hinzu plan --from` uses the identical closure and resolution — see
[plan.md](./plan.md#rooted-plans---from) for the "what does main() need?" framing.

## Ordering semantics (read this first)

The port order is **dependencies-first**, a.k.a. **leaves-first**:

- A **leaf** is a symbol with no *internal* (non-external) dependency — it calls
  only itself, nothing, or external library functions. Leaves are the first
  batch to port; nothing local must exist before them.
- A symbol appears in `symbol_topo_order` **only after all of its callees**. Pop
  from the front of the list and you can always port safely: everything the
  popped symbol calls is already behind you.
- **Cycles** (mutual recursion, back-and-forth calls) can't be linearized, so
  each non-trivial strongly-connected component (size > 1) is condensed into a
  group. Its members appear **contiguously** in the order and share an `scc`
  group id — port them together, in one pass. Condensing the cycles is exactly
  what turns a graph-with-cycles into the acyclic **condensation** the order is
  computed over.

The same direction applies to `file_topo_order` over the file-dependency graph.

Concretely, for a chain `a → b → c` the order is `["c", "b", "a"]`. For a cycle
`a ↔ b` plus `c → a`, the group `{a, b}` is emitted before `c`.

## JSON schema

Top level (`hinzu_graph_version: 1`):

| field | type | meaning |
| --- | --- | --- |
| `hinzu_graph_version` | int | schema version (currently `1`) |
| `root` | string | the analyzed target label (usually the project path) |
| `language` | string \| null | the dominant source language |
| `fidelity` | object | honesty caveats + counts (see below) |
| `stats` | object | aggregate counts |
| `symbols` | array | symbol nodes (sorted by id) |
| `edges` | array | symbol edges (in fact order) |
| `files` | array | file-rollup nodes (sorted by path) |
| `file_edges` | array | file-rollup edges (sorted by `from`,`to`) |
| `condensation` | object | the acyclic SCC-condensation: the port-order utilities |

### `fidelity`

| field | type | meaning |
| --- | --- | --- |
| `call_only` | bool | always `true`: edges are call/use only |
| `notes` | string[] | human-readable caveats about what the graph misses |
| `unknown_edge_count` | int | edges resolving to an unknown/unresolved target |
| `external_node_count` | int | external (no-local-definition) target nodes |

### `stats`

`symbol_count` (internal symbols), `external_count`, `file_count`,
`edge_count`, `file_edge_count`, `scc_count` (non-trivial symbol SCCs).

### `symbols[]` — a symbol node

| field | type | meaning |
| --- | --- | --- |
| `id` | string | stable symbol id (the graph key) |
| `display` | string | short human name (id itself for an external node) |
| `file` | string \| null | defining file; `null` for external |
| `language` | string \| null | source language; `null` for external |
| `line_start`, `line_end` | int \| null | source span; `null` for external |
| `loc` | int \| null | `line_end - line_start + 1`; `null` for external |
| `external` | bool | true = a call target with no local definition |
| `fan_in` | int | distinct callers (in-degree, full graph) |
| `fan_out` | int | distinct callees (out-degree, external included) |
| `transitive_dep_count` | int | distinct downward-reachable nodes (external included), excluding self |
| `is_leaf` | bool | no *internal* dependency — portable first |
| `effect_roots` | string[] | effect categories this symbol transitively reaches (via propagation over the facts' roots); empty for external nodes and when nothing is seeded |
| `external_packages` | string[] | leading `::`-segment of each external callee, sorted |
| `scc` | string \| null | SCC group id (`"scc:N"`) when in a call cycle |

**External nodes.** Every edge endpoint is resolvable in `symbols`. A callee with
no local definition is emitted as a node with `external: true`, null source
location, always `is_leaf: true`, and empty `effect_roots`/`external_packages`.
Treat these as already-available library calls — **not** port targets.

**`is_leaf` vs `transitive_dep_count`.** `is_leaf` is computed over the *internal*
graph only (external callees don't count against it), so a symbol whose only
dependency is `node:fs::readFileSync` is still a leaf. `fan_out` and
`transitive_dep_count` are over the *full* graph (external included), as a size
estimate for "porting this pulls in N things".

### `edges[]` — a symbol edge

`from`, `to`, `kind` (`call`/`reference`), `resolution`
(`call`/`reference`/`value-flow`/`unresolved`), `evidence_file`,
`evidence_line`, and:

| `provenance` | meaning |
| --- | --- |
| `resolved` | `to` is a local definition |
| `external` | `to` is an external package target |
| `unknown` | `to` is unresolved (indirect call), or seeded as `Unknown` — fail closed |

### `files[]` — a file-rollup node

`path`, `symbol_count`, `loc` (sum), `fan_in`, `fan_out`,
`transitive_dep_count`, `is_leaf`, `effect_roots` (union of members),
`external_packages` (union of members), `scc`. Fan/transitive/SCC are computed on
the file-dependency graph.

### `file_edges[]` — a file-rollup edge

Derived by projecting each internal symbol call edge onto `(caller.file →
callee.file)`, dropping self-loops. `call_edge_count` is how many symbol edges
project onto the pair; `has_unknown` is true if any contributing symbol edge was
itself unresolved.

### `condensation` — the acyclic view + port-order utilities

The dependency graph may contain cycles; **collapsing each strongly-connected
component to a single node yields this acyclic condensation**, and the port order
is a topological sort of it. That is the whole reason the ordering is
well-defined — you cannot topologically sort a graph with a cycle, but you can
sort its condensation.

| field | meaning |
| --- | --- |
| `symbol_topo_order` | every internal symbol in dependencies-first order; SCC members contiguous |
| `file_topo_order` | every file in dependencies-first order |
| `symbol_sccs` | non-trivial symbol SCCs: `{ id, members[] }` |
| `file_sccs` | non-trivial file SCCs |
| `symbol_leaves` | internal symbols with no internal dependency (first batch) |
| `file_leaves` | files with no file dependency (first batch) |

## Fidelity limits (stated honestly)

The graph is **call-only**. An edge means "caller calls or references callee",
from the same facts the effect engine uses. Consequently:

- **Higher-order calls, dynamic dispatch (trait objects / function pointers), and
  unresolved callbacks** are approximated or missed. An edge the adapter could
  not resolve is marked `provenance: "unknown"` and counted in
  `fidelity.unknown_edge_count` — never silently dropped.
- **There is no `textDocument/implementation` or explicit imports table.** File
  edges are *inferred* by projecting symbol call edges onto files, so a file
  dependency that flows only through types or imports (never a call) is not
  represented.
- **External callees** (no local definition) are library boundaries, emitted as
  leaf nodes, not port targets.

These caveats travel inside `fidelity.notes` so a consumer sees them next to the
data.

## How a porting agent walks this graph

A simple, robust loop:

1. **Start from `condensation.symbol_leaves`** (equivalently, the front of
   `condensation.symbol_topo_order`). These depend on nothing local — port and
   test them in isolation.
2. **Walk `symbol_topo_order` front-to-back.** By construction, when you reach a
   symbol every symbol it calls is already ported. Port it, run its tests, mark
   it done.
3. **Port an SCC group together.** When the next entries share an `scc` id (they
   are contiguous), they form a call cycle — port and test them as one unit,
   since none is independently complete.
4. **Treat `external` nodes as already available.** They are library calls in the
   target language; map them to the target's equivalent, don't port them. Use
   `external_packages` / `effect_roots` to see which libraries and side effects a
   symbol pulls in, so you can line up the target-language dependency first.
5. **Size batches with `transitive_dep_count`** and prioritize with `fan_in`: a
   high-`fan_in` leaf is a good early win (many things unblock once it lands); a
   high-`transitive_dep_count` symbol is a large, late chunk.
6. **Roll up to files** (`file_topo_order`, `file_leaves`, `file_edges`) when you
   want to move a whole module at a time instead of symbol-by-symbol.

Because the order is a topological sort of the graph's condensation, the invariant
"everything I depend on is already ported" holds at every step — which is exactly
what makes an incremental, continuously-testable port possible.

To port **in parallel** instead of one symbol at a time — grouping files into
PRs and scheduling them into waves that can each be ported concurrently — layer
[`hinzu plan`](./plan.md) on top of this graph.

To measure **how far a port has actually gotten** — matching this graph against
the graph of the target-language port, file by file and symbol by symbol — see
[`hinzu port-diff`](./port-diff.md). It reuses the same `--from` closure scoping,
so you can ask "of everything this entry point needs, which is unported?"
