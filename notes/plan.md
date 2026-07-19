# `hinzu plan` — a wave/group porting plan for AI-assisted porting

`hinzu plan <project>` takes the dependency [graph](./graph.md) and turns it into
an **operational schedule**: it organizes files into **groups** (a PR / an agent
thread per group) and lays those groups out into **waves** (batches that can be
ported fully in parallel). Where `hinzu graph` answers *"in what order can a
symbol be ported so its dependencies come first?"*, `hinzu plan` answers the
question a porting *orchestrator* asks:

> Which files can I hand to parallel threads/PRs **right now**, and what does
> finishing them **unlock**?

Porting happens file-by-file (a PR per group), so the plan works over the graph's
**file rollup** — it reuses the exact `files` / `file_edges` the graph already
computed and never re-walks the raw facts.

## Usage

```sh
# live extraction (routes by project marker: Cargo.toml / package.json / etc.)
hinzu plan ./my-project --out plan.json

# from a pre-extracted fact set or an existing SQLite store
hinzu plan ./my-project --facts facts.json --out plan.json
hinzu plan ./my-project --db facts.db

# compose off a graph you already emitted (no re-extraction)
hinzu graph ./my-project --out graph.json
hinzu plan  ./my-project --graph graph.json --out plan.json

# grouping knobs
hinzu plan ./my-project --group-max-loc 300   # raise the coalescing ceiling
hinzu plan ./my-project --no-coalesce         # cycle-SCCs + singletons only

# default output is stdout
hinzu plan ./my-project
```

No policy file is required — like `hinzu graph`, the plan does not run the
effect-propagation gate; effect roots are seeded best-effort from the language's
built-in annotation base so the per-group `effect_roots` are populated.

### CLI flags

| flag | default | meaning |
| --- | --- | --- |
| `<path>` | — | the project to analyze (positional) |
| `--facts <file>` | — | pre-extracted facts JSON, in place of a live run |
| `--db <file>` | — | an existing SQLite fact store to read from |
| `--graph <file>` | — | a previously emitted `graph.json` — build the plan straight from it, skipping extraction |
| `--out <file>` | stdout | where to write the plan JSON |
| `--group-max-loc <n>` | `200` | the loc ceiling a coalesced group is kept under |
| `--no-coalesce` | off | disable small-file coalescing (SCC-only grouping) |

## Wave semantics (read this first)

A **wave** is a **topological layer** over the group-dependency DAG, assigned by
longest-path (ASAP) layering:

```
wave(g) = 0                       if g depends on no other group
        = 1 + max(wave(d))        over the groups d that g depends on
```

This gives three guarantees that make waves the unit of parallel work:

- **Wave 0 is the leaves.** A group in wave 0 depends on nothing internal — port
  it first; nothing local must exist before it.
- **Same wave ⇒ independent ⇒ parallel.** Two groups in the same wave never have
  a dependency between them (if they did, one would be a wave later). So an
  orchestrator can port an entire wave concurrently — one thread/PR per group.
- **Each wave is "what the previous wave unlocked."** A group lands in wave `k`
  exactly when its last dependency lands in wave `k-1`. Finishing wave `k-1`
  makes every wave-`k` group portable; the `unlocks` field names them.

`stats.largest_wave` is the **peak parallelism** (the most groups in any one
wave — your thread/PR budget); `stats.critical_path_length` (= `wave_count`) is
the **minimum number of sequential rounds**, no matter how many threads you throw
at it.

## Group semantics

A **group** is a set of files ported together as one unit. Three formation
reasons:

- **`cycle` (mandatory).** Files in the same file-level dependency cycle (a
  file-graph SCC of size > 1) *must* be one group — none is independently
  complete, so they are ported together in one thread. Derived from the graph's
  `condensation.file_sccs`.
- **`coalesced-small` (heuristic, tunable).** With coalescing on (the default),
  small groups (total loc `< --group-max-loc`) are greedily merged with an
  **adjacent** group (one connected by a file dependency edge) as long as (a) the
  merged loc stays under the ceiling and (b) the merge keeps the group
  condensation a **DAG** — a merge that would route a cycle through a third group
  is skipped. This chains up tiny, tightly-coupled files so an orchestrator isn't
  spinning up a thread per one-liner.
- **`singleton`.** A lone file that is neither in a cycle nor coalesced.

**Independent small files are not force-merged.** Coalescing only merges files
that are *adjacent* in the dependency graph. Two small files that don't depend on
each other are left as separate singleton groups — but they'll land in the **same
wave**, so an orchestrator is free to batch them into one thread at dispatch time
if it wants. The plan doesn't presume that batching.

Turn coalescing off with `--no-coalesce` for the purest schedule: one group per
file (plus mandatory cycle groups), maximum parallelism, maximum PR count.

## JSON schema

Top level (`hinzu_plan_version: 1`):

| field | type | meaning |
| --- | --- | --- |
| `hinzu_plan_version` | int | schema version (currently `1`) |
| `root` | string | the analyzed target label (usually the project path) |
| `language` | string \| null | the dominant source language |
| `granularity` | string | always `"file"` — the plan is file-by-file |
| `grouping` | object | the knobs used: `max_group_loc`, `coalesce_small` |
| `fidelity` | object | honesty caveats (see below) |
| `stats` | object | aggregate counts |
| `groups` | array | the groups, in numeric id order |
| `waves` | array | the waves, `0..wave_count` |

### `grouping`

| field | type | meaning |
| --- | --- | --- |
| `max_group_loc` | int | the loc ceiling coalescing respected |
| `coalesce_small` | bool | whether small-file coalescing ran |

### `fidelity`

| field | type | meaning |
| --- | --- | --- |
| `call_only` | bool | always `true`: the plan is built over the call-only dependency graph |
| `notes` | string[] | caveats — the graph's call-only limits, plus the grouping/coalescing heuristics |

### `stats`

| field | type | meaning |
| --- | --- | --- |
| `file_count` | int | distinct files scheduled |
| `group_count` | int | total groups |
| `wave_count` | int | total waves |
| `cycle_group_count` | int | groups formed because their files are in a dependency cycle |
| `coalesced_group_count` | int | groups formed by small-file coalescing |
| `singleton_group_count` | int | lone-file groups |
| `largest_wave` | int | the most groups in any one wave — peak parallelism |
| `critical_path_length` | int | the longest dependency chain in waves (= `wave_count`) — minimum sequential rounds |

### `groups[]` — a group

| field | type | meaning |
| --- | --- | --- |
| `id` | string | group id (`"group:N"`, numbered in wave-then-path order) |
| `reason` | string | `"cycle"`, `"coalesced-small"`, or `"singleton"` |
| `files` | string[] | member file paths, sorted |
| `loc` | int | total lines of code across the members |
| `symbol_count` | int | total local symbol definitions across the members |
| `wave` | int | the wave this group lands in (`0` = ported first) |
| `depends_on` | string[] | group ids this group depends on — all in strictly **earlier** waves |
| `unlocks` | string[] | group ids that depend on this one — all in strictly **later** waves |
| `external_packages` | string[] | union of the members' external package prefixes, sorted |
| `effect_roots` | string[] | union of the members' reachable effect categories, sorted |
| `has_unknown_edges` | bool | a file dependency edge incident to this group was unresolved — a place the plan is guessing |

### `waves[]` — a wave

| field | type | meaning |
| --- | --- | --- |
| `wave` | int | the wave number (`0` = first) |
| `group_ids` | string[] | the groups in this wave, in numeric id order |
| `files` | string[] | every member file of every group in the wave, flattened and sorted |
| `loc` | int | total loc across the wave |
| `group_count` | int | how many groups are in this wave |

## Fidelity limits (stated honestly)

The plan is built over the **call-only** dependency graph and inherits every one
of its caveats — an edge means "caller calls or references callee"; file edges are
*inferred* by projecting symbol call edges onto files (there is no imports /
implementation table), so a file dependency that flows only through types or
imports is not represented; higher-order calls, dynamic dispatch, and unresolved
callbacks are approximated or missed and marked (`has_unknown_edges`) rather than
dropped. On top of that:

- **Grouping and wave assignment are only as good as those edges.** A missed edge
  can wrongly split a wave or under-constrain the port order.
- **Coalescing is a size heuristic, not a correctness requirement.** Only `cycle`
  groups are mandatory. `--group-max-loc` / `--no-coalesce` tune the rest, and
  independent small files are never force-merged (they simply share a wave).

These notes travel inside `fidelity.notes` so a consumer sees them next to the
data.

## How a porting orchestrator runs the plan

Walk the waves in order; within a wave, go wide:

1. **For each wave, in order `0..wave_count`:**
   - Spin up **one thread / PR per group** in `waves[k].group_ids`.
   - Port every group in the wave **in parallel** — by construction no two of
     them depend on each other, so there's no ordering to respect within a wave.
   - For a `cycle` group, port all of its `files` **together in one thread** —
     none is independently complete.
   - **Wait for the whole wave to land** (merge/verify), then proceed to wave
     `k+1`. Each landed wave is exactly what the next wave needed; a group's
     `depends_on` (all earlier waves) is satisfied the moment its wave opens.
2. **Budget parallelism with `largest_wave`** — you never need more than that many
   concurrent threads/PRs at once.
3. **Set expectations with `critical_path_length`** — that's the minimum number of
   sequential rounds this port takes, however many threads you have. It's the
   longest dependency chain; no amount of parallelism shortens it.
4. **Use `unlocks` to see the payoff** of finishing a group, and `external_packages`
   / `effect_roots` to line up the target-language dependencies and side-effect
   surfaces a group pulls in before you start it.

Because waves are a topological layering of the dependency graph, the invariant
"everything a group depends on is already ported" holds the moment its wave
opens — which is what lets the port run wide *and* stay continuously testable.
