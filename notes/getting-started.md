# Getting Started: Effect Propagation as hinzu's First Slice

## Goal

hinzu enforces a **functional core**: effects — filesystem, network, database, clock, randomness, process, ambient environment — are allowed only inside designated files and file trees, and the policy that says where lives *outside* the source, not in annotations. The mechanism is the one [`notes/design.md`](./design.md) describes: seed the operations that are inherently effectful, propagate effects over the call graph to a fixed point, and report every callable's direct and transitive effects with an evidence path. A policy then fails any callable that can reach a forbidden effect from a region that forbids it.

Rust and TypeScript are the first two adapter targets. This note turns the design into a concrete first slice: the fact sources we will actually use per language, a fact schema v0 that carries exactly what propagation needs, the policy-file shape, and a sliced implementation plan with an honest first cut.

## The problem, reduced

Effect propagation is reachability. Two ingredients per language, everything else language-independent:

1. **A graph** of "callable A uses callable/symbol B" — the call/use graph.
2. **A seed set** of effectful roots — the standard-library and runtime operations that *are* an effect (`std::fs::*`, `tokio::net`, `node:fs`, ambient `fetch`, `Math.random`, and so on), each tagged with an effect category.

Given those, the engine is uniform: propagate categories backward along edges to a fixed point, keep an evidence path, check the summary against a policy. The hard, language-specific work is producing (1) and (2) faithfully. That is what the adapters do; the design deliberately keeps them thin ("extraction, not interpretation").

## Fact sources — what each language actually gives us

We evaluated the real options against one crux: **can the source attribute a use to the function it occurs in** (caller to callee), and how precisely does it resolve the callee?

### Rust — start with `rust-analyzer scip`

`rust-analyzer scip <project>` emits a SCIP index: stable, structured symbols (`crate version descriptor`) plus occurrences with roles. SCIP has **no call role and no call-site structure** — an occurrence of a function symbol is just a "reference," indistinguishable from taking its address or a type mention. But it carries `enclosing_range`, and rust-analyzer's emitter populates it with the **enclosing definition's body range**. So caller attribution falls out of geometric nesting: the innermost definition whose body-range contains a reference is the caller; the reference's symbol is the callee. Effect roots are a clean prefix/provenance match on the callee symbol (`std fs`, tokio, `std process`).

Fidelity to promise honestly: **static, name-resolved use-edges.** Direct calls and function-pointer/callback *references* are captured; imports and non-body references are filtered out (their `enclosing_range` is empty). Generic calls resolve to the generic definition (pre-monomorphization). Dynamic dispatch resolves only to the trait *method*, not the concrete impl; indirect calls through function pointers or closures-in-variables are not resolved. Conservative on `dyn`/closures — not a sound call graph.

Why start here: one binary, one protobuf, no nightly, no heavy dependency, no build-driver integration. Upgrade paths, in order, when precision demands: **`ra_ap_ide::Analysis::outgoing_calls`** (first-class call hierarchy, real call-vs-reference edges — same resolution engine, but a heavy, explicitly-unstable `0.0.x` dependency to pin), then a **`stable_mir`/charon rustc driver** for monomorphized, trait-resolved calls (sound on dispatch, but nightly, ABI-unstable, and it must compile the crate).

One thing to verify empirically in slice 1: confirm rust-analyzer's `scip` emits a non-empty `enclosing_range` for in-body reference occurrences on a real target crate. The emitter source says it does; a single check de-risks the whole attribution scheme.

### TypeScript — a native compiler-API adapter

scip-typescript exists and would give one uniform SCIP shape for both languages, but it inherits SCIP's weakness *and adds one*: it emits `enclosing_range` only on top-level function definitions, and **locally-scoped / nested functions and inline closures get `local` symbols with no range** — so they can never be reconstructed as callers. On a real codebase full of inner helpers and callbacks, that is a material blind spot exactly where effects hide.

The TypeScript compiler API gives a real call graph. Drive `ts.createProgram` with the project's tsconfig, walk each source file keeping a stack of enclosing functions (the caller), and at each call-like node use `checker.getResolvedSignature()` to reach the callee's declaration, then its symbol and file. This resolves method calls on typed receivers, imported/re-exported/aliased symbols, node builtins (`fs.readFile` to `@types/node/fs.d.ts`), and — crucially — **ambient globals** (`fetch`, `Math.random`, `Date.now` to `lib.*.d.ts`), which are not imports and which only a real checker can seed as roots by declaration provenance.

Emit **two edge kinds**: `calls` (from `getResolvedSignature`) and `references` (a bare identifier reference to an effectful symbol, for example passing `fs.readFile` as a callback). Propagate over the union. This makes propagation robust to higher-order flow without pretending to resolve `any`-typed dynamic dispatch, `require(variable)`/`import(expr)`, or `eval` — those stay honestly unresolved. ts-morph is a fidelity-neutral ergonomic wrapper over the same API; use it or raw `typescript`, it is not a fidelity decision.

### The unification decision

Normalize **both** languages to one fact schema — but populate it from **native, per-language sources**, not a single SCIP adapter. SCIP's reconstructed-call-structure weakness sits on the critical path for the one thing hinzu is for. A SCIP adapter (Rust via rust-analyzer; optionally TS) is a legitimate *cheap v0 bootstrap* to prove the engine end to end — just label its fidelity "reference-within-body, top-level callers, approximate" and plan to replace the TS side with the compiler-API adapter.

## Fact schema v0

The minimum that supports effect propagation and matches design.md's "normalize facts, not syntax":

- **Definition** — a callable. `id: SymbolId` (stable), `display`, `language`, source provenance (`file`, line range — the range is what policy regions match on).
- **SymbolId** — a stable, structured string in the SCIP symbol style (package/crate + version + descriptor path). This *is* the design's "stable identity that survives repeated analysis"; SCIP gives it to us for Rust directly, and we synthesize the same shape for TS from the declaration's package + module + descriptor.
- **Edge** — `{ caller: SymbolId, callee: SymbolId, kind: Call | Reference, evidence: (file, line) }`.
- **EffectRoot** — `{ symbol: SymbolId (or prefix pattern), effect: Effect }`. The per-language seed lists.
- **Effect** — the closed category set: `Fs, Net, Db, Clock, Random, Process, Env`.
- **Confidence** (design's trust model) — `Proven | Inferred | Assumed | Unknown`, attached to edges/summaries so a policy can set a threshold. v0 marks compiler-resolved edges `Proven` and unresolved-dispatch approximations `Assumed`.

Derived by the engine:

- **EffectSummary** per Definition — the set of transitively-reachable effects, plus, per effect, one **evidence path** (the caller chain from this callable to the effectful root).

Storage v0: in-memory fact tables, serde-serializable to JSON for inspection. Defer SQLite until incremental cached summaries actually pay for themselves (design's "incremental analysis" section) — it is an open decision, not a decided one.

## The propagation engine

Multi-source breadth-first search backward from the roots over the reverse graph, monotone set-union lattice per effect category:

1. Seed each root symbol's summary with its effect (evidence path = `[root]`).
2. Worklist: pop a symbol; for every caller of it, union the callee's effects in; on any new effect, record the evidence path `[caller, ...callee's path]` and re-enqueue the caller.
3. Converges because inserts are monotone — cycles and recursion just accumulate and stop when nothing new is added. Breadth-first order yields short evidence paths for explanations.

`Reference` and `Call` edges both carry effects (over-approximate on purpose). Confidence propagates as the minimum of the path's edge confidences.

## Policy file (`hinzu.toml`)

External to source, region-based, matching design.md's policy section (architectural regions, allowed/prohibited categories, trusted summaries, ignored paths, confidence threshold):

```toml
[analysis]
confidence_threshold = "inferred"     # ignore findings weaker than this
ignore = ["**/tests/**", "**/*.test.ts"]

# The functional core: no I/O effects, however deep the call chain.
[region.core]
paths  = ["crates/*/src/**"]
forbid = ["fs", "net", "db", "process"]

# Where effects are allowed to live.
[region.adapters]
paths = ["crates/*/src/adapters/**", "crates/*-cli/src/**"]
allow = ["fs", "net", "process", "env"]

[trust]
# Treat these unresolved externals as pure, on the maintainer's word.
assume_pure = ["log::*", "tracing::*"]
```

A callable **violates** policy when it sits in a region, that region forbids effect *E*, and its EffectSummary contains *E* at or above the confidence threshold. The report prints the callable, the forbidden effect, and the evidence path to the root — "why," per design.md's "explain every conclusion."

## Slice plan

**Slice 0 — engine + schema (this PR, prototype behind `hinzu run`).** The fact schema types, the propagation engine, and a minimal policy check, exercised on a hand-built synthetic fact set with unit tests (fixed point, a cycle, evidence paths, one policy violation). No adapter, no external toolchain — this proves the *language-independent* core in isolation, which is the novel part.

**Slice 1 — Rust, end to end.** A SCIP adapter (shell out to `rust-analyzer scip`, parse the protobuf, reconstruct edges via `enclosing_range`), the Rust effect-root seed list, the `hinzu.toml` parser, and a `hinzu check <path>` command. Run it on a real guinea pig — disponent is the natural canary (it genuinely dispatches to tmux/remote environments, so it has real `fs`/`process`/`net` to find) — and report real findings. Verify the `enclosing_range` caveat here.

**Slice 2 — TypeScript.** The native compiler-API adapter emitting `calls` + `references` edges, the TS effect-root table (`node:fs`, `child_process`, `fetch`, `Math.random`, `Date.now`), normalized into the same schema and run through the same engine. hinzu is now cross-language.

**Slice 3+ — precision and durability.** `ra_ap_ide` call hierarchy (then MIR/charon) for Rust dispatch precision; confidence grading wired through; SQLite-backed incremental summaries when repeated runs justify them; more effect categories and trusted-summary handling.

## Open decisions

Flagged for review rather than silently decided (defaults taken in this draft PR as noted):

1. **Adapter uniformity** — native per-language adapters normalized to a shared schema (the recommendation), versus one SCIP adapter for both (uniform, weaker call structure). Default: native, with a SCIP Rust bootstrap.
2. **First guinea pig** — disponent (real effects, useful findings) versus hinzu itself (dogfood, tiny) versus entl. Default: disponent.
3. **Storage** — in-memory + JSON for v0, SQLite deferred. Confirm, or persist from the start?
4. **Taint granularity** — taint on symbol *references* (catches callbacks, over-approximate) versus call-only edges (precise, misses higher-order). Default: reference-taint, bias to flagging.
