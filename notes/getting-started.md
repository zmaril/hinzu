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

### Rust — a native StableMIR (`rustc_public`) driver

Native, no SCIP. A custom rustc driver built on `rustc_public` (the stabilizing StableMIR API) walks each monomorphized function's MIR and reads `TerminatorKind::Call`, resolving the callee with `Instance::resolve` — so you get **monomorphized, trait-resolved** caller-to-callee edges directly, closures as real instances, and (because MIR is post-expansion) macro-expanded calls resolved automatically. The driver is a ~150-line template copied from the upstream demo and conformance tests; the only harness work is running it across a cargo workspace via `RUSTC_WORKSPACE_WRAPPER` (the clippy/miri trick), unioning per-crate facts.

Fidelity to promise honestly: **exact** on static calls, generic calls (per instantiation), static trait dispatch, direct closure calls, and macro-expanded calls; **`dyn Trait` virtual calls and function-pointer indirect calls are unresolved** — modelled as the design's "call site invokes one of several possible targets" (over-approximate to the trait method's impls, or mark unknown; never silently dropped). Cost: a pinned nightly with the `rustc-dev` component, and the API is explicitly unstable, so expect to re-pin and occasionally patch on toolchain bumps until it lands on crates.io.

Ruled out (native but worse fits): **charon** (AeneasVerif) is the fastest path to raw JSON — a worthwhile one-day spike to de-risk the fact schema — but it does not monomorphize, so it relocates concrete-impl resolution into hinzu, and it is alpha with breaking changes planned; **rust-analyzer-as-a-library** (`ra_ap_ide` call hierarchy) gives real edges on a *stable* toolchain but only at HIR level (generic and `dyn` calls resolve to the declared trait method, not the concrete impl); **cargo-call-stack** works on LLVM-IR with types erased — too lossy. StableMIR is the foundation; charon an optional spike.

### TypeScript — a native compiler-API adapter

scip-typescript exists and would give one uniform SCIP shape for both languages, but it inherits SCIP's weakness *and adds one*: it emits `enclosing_range` only on top-level function definitions, and **locally-scoped / nested functions and inline closures get `local` symbols with no range** — so they can never be reconstructed as callers. On a real codebase full of inner helpers and callbacks, that is a material blind spot exactly where effects hide.

The TypeScript compiler API gives a real call graph. Drive `ts.createProgram` with the project's tsconfig, walk each source file keeping a stack of enclosing functions (the caller), and at each call-like node use `checker.getResolvedSignature()` to reach the callee's declaration, then its symbol and file. This resolves method calls on typed receivers, imported/re-exported/aliased symbols, node builtins (`fs.readFile` to `@types/node/fs.d.ts`), and — crucially — **ambient globals** (`fetch`, `Math.random`, `Date.now` to `lib.*.d.ts`), which are not imports and which only a real checker can seed as roots by declaration provenance.

Emit **two edge kinds**: `calls` (from `getResolvedSignature`) and `references` (a bare identifier reference to an effectful symbol, for example passing `fs.readFile` as a callback). Propagate over the union. This makes propagation robust to higher-order flow without pretending to resolve `any`-typed dynamic dispatch, `require(variable)`/`import(expr)`, or `eval` — those stay honestly unresolved. ts-morph is a fidelity-neutral ergonomic wrapper over the same API; use it or raw `typescript`, it is not a fidelity decision.

### Native for all, one shared schema

Both languages go through native compiler APIs — a StableMIR driver for Rust, the TypeScript compiler API for TypeScript — normalized into one fact schema. We are not unifying on SCIP: its reconstructed call structure is weak on exactly the propagation hinzu exists for, and native APIs additionally let us record *how* each edge was resolved (a real call versus a bare reference versus unresolved-conservative), which the precision ladder below depends on.

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

Storage: **a SQLite fact database from day one** — tables for definitions, edges, effect roots, and the derived effect summaries. The facts are relational and the analysis is naturally reachability/queries over them (a Datalog-style formulation is a clean future fit), and persistence is what makes the design's incremental cached summaries and cross-revision comparison possible. The pi.dev run below already writes this schema.

## The propagation engine

Multi-source breadth-first search backward from the roots over the reverse graph, monotone set-union lattice per effect category:

1. Seed each root symbol's summary with its effect (evidence path = `[root]`).
2. Worklist: pop a symbol; for every caller of it, union the callee's effects in; on any new effect, record the evidence path `[caller, ...callee's path]` and re-enqueue the caller.
3. Converges because inserts are monotone — cycles and recursion just accumulate and stop when nothing new is added. Breadth-first order yields short evidence paths for explanations.

`Reference` and `Call` edges both carry effects (over-approximate on purpose). Confidence propagates as the minimum of the path's edge confidences.

## Edges: reference vs call, and the precision ladder

Whether an edge exists is really a question about function *values* and indirect calls, on a precision ladder:

- **Reference-level (v0).** Draw an edge wherever the callee symbol appears in a body — called or merely passed as a value. Over-approximate, cheap, safe-by-default: it catches callbacks (passing `fs.readFile` taints you) at the cost of false positives. The right default for a functional-core policy, where a false "pure" is the expensive error.
- **Call-only.** An edge only at real invocation sites. Precise, but it misses higher-order flow — a callback invoked elsewhere never connects.
- **Value-flow / points-to-resolved calls + effect-polymorphic summaries (target).** Resolve indirect call sites by tracking where function values flow (the CHA to RTA to points-to to k-CFA ladder), and give higher-order functions a summary parameterized over their function-typed arguments (`runner`'s effect is its callback's effect, discharged per call site). This recovers call-only precision *and* callback coverage — what CodeQL/Semgrep-style taint and Datalog points-to (Doop) do in practice. Unresolved indirect calls fall back to the conservative over-approximation, preserving "every ambiguity degrades to an effect."

So each `Edge` records *how* it was resolved (`call` / `reference` / `value-flow` / `unresolved`), letting precision tighten later without reshaping the fact schema. hinzu ships reference + call in v0 (the pi.dev run uses exactly this) and climbs the ladder where real code shows it matters.

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

## Decisions taken

The forks flagged in the first draft are now settled:

1. **Native adapters for both languages** — StableMIR for Rust, the TypeScript compiler API for TypeScript, normalized to one schema. Not SCIP.
2. **Storage: SQLite from day one** (a persistent fact database), not in-memory/JSON.
3. **Edge granularity: reference + call in v0** (over-approximate, bias to flagging), with the value-flow / effect-polymorphic ladder above as the precision path.
4. **First validation target: pi.dev** (earendil-works/pi) — done; see below.

Still genuinely open: whether to run a charon spike first to de-risk the schema before writing the StableMIR driver; whether to drive the Rust workspace crate-by-crate or via `RUSTC_WORKSPACE_WRAPPER` from the start; and the exact `hinzu.toml` region grammar (glob syntax).

## Empirical validation: pi.dev

To pressure-test the TypeScript adapter before committing to the design, we ran a native compiler-API extractor (TypeScript 5.9.3, `@types/node` 22.19.19) over all five packages of pi (earendil-works/pi, ~207k LOC), effect roots seeded by declaration provenance, facts written to SQLite.

- **5,998** function definitions; **22,042** call sites walked, **96.8%** resolved to a declaration.
- **1,357 functions (22.6%) transitively effectful.** By category: env 733, fs 699, clock 310, process 300, random 223, net 219. Densest package: `orchestrator` (55.5%).
- Fact DB: definitions 5,998; edges 10,312 (10,253 call + 59 reference); effect_roots 969; effect_summaries 2,484.

What it surfaced (hand-verified):

- **Reference-level taint, in the wild:** `detectCapabilities` takes `probeTmuxHyperlinks` as a *default parameter value* it never calls directly; that function runs `execSync("tmux …")`. Caught via a reference edge (`terminal-image.ts:65`) — the exact higher-order case call-only would miss.
- A "pure-looking" `buildSystemPrompt` reaches the filesystem: `buildSystemPrompt → getReadmePath → getPackageDir → existsSync`.
- A tool-output *formatter* transitively spawns a subprocess: `renderToolPath → linkPath → getCapabilities → detectCapabilities →(reference) probeTmuxHyperlinks → execSync`.
- A functional-core probe over an illustrative "pure" boundary (256 functions across message/prompt/template/render/serialization modules, forbidding fs/net/process) flagged **14 forbidden-effect leaks** (fs 9, process 3, net 2). pi was not written to this policy, so some are boundary-choice artifacts — the point is the analysis pinpoints them with evidence paths.

Honest limits observed: 3.2% of calls were unresolved (any-typed / dynamic / third-party), so effects flowing only through those are missed (an under-approximation); third-party effect libraries are invisible without `node_modules` and were caught by a name-based import fallback; dynamic `import()`/`require()` and pointer aliasing are not followed.

Takeaway: the native TypeScript path produces a real, queryable effect map on a large real codebase, the SQLite schema holds, and reference-level taint earns its keep on actual higher-order code.
