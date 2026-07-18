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
- **Effect** — the category set: `Fs, Net, Db, Clock, Random, Process, Env, Alloc` (heap allocation), plus `Unknown` — not a real-world effect but an uncertainty marker that propagates like one (see "Unknown and trusted external summaries" below).
- **Confidence** (design's trust model) — `Proven | Inferred | Assumed | Unknown`, attached to edges/summaries so a policy can set a threshold. v0 marks compiler-resolved edges `Proven` and unresolved-dispatch approximations `Assumed`.

Derived by the engine:

- **EffectSummary** per Definition — the set of transitively-reachable effects, plus, per effect, one **evidence path** (the caller chain from this callable to the effectful root).

Storage and analysis engine (decided empirically — see the validation section):

- **Durable fact store: SQLite.** Embedded, portable, no operational overhead; it holds the source-of-truth facts and backs the design's incremental cached summaries and cross-revision comparison. The pi.dev run already writes this schema.
- **Analysis engine: DBSP (Feldera).** One engine covers both modes. The initial whole-repo analysis is a single full step; re-analysis after a code edit is an incremental delta step that recomputes in time proportional to the change, and it supports exact *retraction* — un-tainting a function when an edge is removed. Effect propagation is a recursive fixed point over the edge relation, and each rung of the precision ladder is a rule change in the circuit.

We ran a bake-off of batch Datalog engines (`ascent`, Cozo) as a separate analysis layer, and both reproduced the reference summaries exactly — but DBSP matches that batch answer and adds incrementality, so hinzu uses DBSP alone rather than carrying a second engine. Cozo was additionally dropped as stale.

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

So each `Edge` records *how* it was resolved (`call` / `reference` / `value-flow` / `unresolved`), letting precision tighten later without reshaping the fact schema. hinzu ships reference + call in v0 (the pi.dev run uses exactly this) and climbs the ladder where real code shows it matters. Each rung is a rule change in the DBSP circuit — validated on the pi facts, where switching from reference-plus-call to call-only is a single atom swap (2,484 down to 2,357 pairs).

## Policy file (`hinzu.toml`)

External to source, region-based, matching design.md's policy section (architectural regions, allowed/prohibited categories, trusted summaries, ignored paths, confidence threshold):

```toml
[analysis]
confidence_threshold = "inferred"     # ignore findings weaker than this
ignore = ["**/tests/**", "**/*.test.ts"]
on_unknown = "fail"                   # unseen external => cannot certify (default)

# The functional core: no I/O effects, however deep the call chain.
[region.core]
paths  = ["crates/*/src/**"]
forbid = ["fs", "net", "db", "process"]

# Where effects are allowed to live.
[region.adapters]
paths = ["crates/*/src/adapters/**", "crates/*-cli/src/**"]
allow = ["fs", "net", "process", "env", "alloc"]

[trust]
# Vouch for externals the analyzer cannot see through, on the maintainer's word.
"log" = "pure"
"tracing" = "pure"
"rusqlite" = ["db"]
```

A callable **violates** policy when it sits in a region, that region forbids effect *E*, and its EffectSummary contains *E* at or above the confidence threshold. The report prints the callable, the forbidden effect, and the evidence path to the root — "why," per design.md's "explain every conclusion."

## Unknown and trusted external summaries

The call graph stops at the edge of what the analyzer compiled. A call into a registry dependency, or an indirect call through a function pointer or `dyn` object, has no body to walk. Reading such a call as pure is the unsound choice: the code on the other side could touch anything.

So an unseen callee that nothing accounts for becomes **Unknown** — the fourth confidence level made to propagate. It rides the same machinery as a real effect: it seeds a root at the offending callee, flows up the call graph, and carries an evidence path. Two shapes become Unknown: a foreign, no-body callee that no rule resolved (unknown effect), and an indirect call the driver could not resolve (unknown target). `hinzu check` fails on Unknown by default; `[analysis] on_unknown` can lower that to `warn` or `ignore`.

Each callee resolves in a fixed order — the first rule that matches wins:

1. an explicit `[trust]` pure vouch,
2. an effect rule (`[roots]`, `[trust]` with effects, or the built-in table),
3. the trusted-pure baseline — the standard library, and calls through a standard-library trait,
4. otherwise Unknown.

hinzu ships a built-in annotation set (`crates/hinzu-core/annotations/std.toml`) for the standard library: its I/O surface as effect roots, its allocating APIs as `alloc` roots, and the genuinely-pure remainder left to the baseline. A project clears its own externals in `hinzu.toml`: `"serde" = "pure"` vouches a crate effect-free, `"rusqlite" = ["db"]` declares the effects it does carry. This is the design's "trusted external summaries," stated outside the source so the trust list is explicit and auditable — and the way to turn an Unknown into a certified boundary is to add one honest line, not to weaken the check.

## Slice plan

**Slice 0 — engine + schema (this PR, prototype behind `hinzu run`).** The fact schema types, the propagation engine, and a minimal policy check, exercised on a hand-built synthetic fact set with unit tests (fixed point, a cycle, evidence paths, one policy violation). No adapter, no external toolchain — this proves the *language-independent* core in isolation, which is the novel part.

**Slice 1 — Rust, end to end.** A SCIP adapter (shell out to `rust-analyzer scip`, parse the protobuf, reconstruct edges via `enclosing_range`), the Rust effect-root seed list, the `hinzu.toml` parser, and a `hinzu check <path>` command. Run it on a real guinea pig — disponent is the natural canary (it genuinely dispatches to tmux/remote environments, so it has real `fs`/`process`/`net` to find) — and report real findings. Verify the `enclosing_range` caveat here.

**Slice 2 — TypeScript.** The native compiler-API adapter emitting `calls` + `references` edges, the TS effect-root table (`node:fs`, `child_process`, `fetch`, `Math.random`, `Date.now`), normalized into the same schema and run through the same engine. hinzu is now cross-language.

**Slice 3+ — precision and durability.** `ra_ap_ide` call hierarchy (then MIR/charon) for Rust dispatch precision; confidence grading wired through; SQLite-backed incremental summaries when repeated runs justify them; more effect categories and trusted-summary handling.

## Decisions taken

The forks flagged in the first draft are now settled:

1. **Native adapters for both languages** — a StableMIR (`rustc_public`) driver for Rust, the TypeScript compiler API for TypeScript, normalized to one schema. Not SCIP.
2. **Store and engine: SQLite plus DBSP.** A durable SQLite fact store, with DBSP (Feldera) as the single analysis engine — the initial full step plus incremental delta steps. `ascent` and Cozo were evaluated and dropped: DBSP covers batch and incremental together, and Cozo is stale.
3. **Edge granularity: reference plus call in v0** (over-approximate, bias to flagging), with the value-flow / effect-polymorphic ladder above as the precision path — each rung a circuit rule.
4. **First validation target: pi.dev** (earendil-works/pi) — done; see below. The Rust adapter is validated on straitjacket; see below.

Still genuinely open: whether to run a charon spike first to de-risk the schema before writing the StableMIR driver; whether to drive the Rust workspace crate-by-crate or via `RUSTC_WORKSPACE_WRAPPER` from the start; and the exact `hinzu.toml` region grammar (glob syntax).

## Empirical validation

### TypeScript: pi.dev

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

### Rust: StableMIR on straitjacket

A ~230-line `rustc_public` (StableMIR) driver, run over straitjacket via `RUSTC_WORKSPACE_WRAPPER` on nightly 1.99: **341 functions, 1,912 call edges, 99.95% statically resolved.** The single unresolved edge is an honest stored-function-pointer call (`FnPtr`, not `FnDef`), bucketed rather than faked; no `dyn` calls survived to MIR, because monomorphization lowered them to concrete `FnDef`s. It found 4 std effect roots and 8 transitively-effectful functions, for example `resolve → load_file_config → config::load_config → std::fs::read_to_string`. The costs of rustc_public's youth: the `stable_mir` to `rustc_public` rename dates every existing tutorial, and `rustc_private` plus dylib linkage pins the binary to one exact nightly.

### The analysis engine: DBSP

DBSP (`dbsp` 0.322) reproduced the reference effect summaries exactly on the pi facts (1,357 functions / 2,484 pairs, set-equal to an independent BFS), then showed diff-proportional recompute:

| change | affected | time | vs full |
|---|---|---|---|
| initial full build | 1,357 funcs | ~23 ms | 1x |
| add a leaf call edge | 18 funcs | ~3 ms | ~8x |
| remove that edge (retraction) | 18 funcs | ~2.8 ms | ~8x |
| add a new effect root | 20 funcs | ~1.3 ms | ~18x |
| add an edge into a hub (in-degree 223) | 431 funcs | ~13.5 ms | ~1.7x |

Cost tracks the change: a one-call-site edit recomputes almost instantly, retraction correctly un-taints downstream functions, and a genuinely broad change does proportional work. A bake-off confirmed `ascent` and Cozo reproduce the same batch answer, but DBSP subsumes the batch case and adds incrementality, so it is the single engine.

## TypeScript adapter (shipped, slice 2)

The TypeScript adapter is now in the tree at `adapters/typescript/`, and
`hinzu check <ts-project>` runs it end to end through the same pipeline as Rust.
It is a native compiler-API extractor (TypeScript 5.9): build one program from
the project's `tsconfig`, walk each source file with a stack of enclosing
functions, and resolve each call with `checker.getResolvedSignature()`. It emits
hinzu's `FactSet` JSON directly — definitions, `call` and `reference` edges, and
effect roots seeded by the callee's declaration provenance.

`hinzu check` routes by project type: a `Cargo.toml` takes the StableMIR path, a
`tsconfig.json` / `package.json` the TypeScript adapter (shelled out to
`node analyze.mjs`; `HINZU_TS_ADAPTER` overrides its location, and the run fails
with an honest message when Node or the adapter is missing rather than faking an
analysis). Everything downstream is shared: the SQLite store, DBSP propagation,
and the `hinzu.toml` policy check.

Effect roots use the one flat, shared vocabulary — `fs`, `net`, `process`, `env`,
`clock`, `random` — the same names as Rust; TypeScript seeds that subset, and
there is no `alloc` for a garbage-collected runtime (see
[`typescript-catalog.md`](./typescript-catalog.md)). A third-party npm package the
checker cannot see through is `Unknown` and fails by default, until a `[trust]`
line vouches for it — identical to Rust's unseen-external handling. The built-in
Node annotation set lives in `crates/hinzu-core/annotations/node.toml`, the
counterpart to `std.toml`.

Re-running the shared pipeline over pi (earendil-works/pi) proves it on real
code: the adapter extracted 16,056 definitions and 41,980 edges (137 reference,
11,168 into unresolved npm), seeding 104 effect roots, in about 18 seconds; an
illustrative functional-core policy over the render and prompt layers then flagged
109 forbidden-effect violations (77 net, 16 fs, 16 process) with evidence paths
such as `TUI.doRender -> node:fs::mkdirSync` and `stream -> (anonymous) ->
global::fetch`, plus 661 `Unknown` warnings naming the npm packages the checker
could not see through (chalk, semver, yaml, openai, typebox, and more).

## Python adapter (shipped, slice 3)

The Python adapter is now in the tree at `adapters/python/`, and
`hinzu check <python-project>` runs it end to end through the same pipeline as
Rust and TypeScript. It is a type-directed extractor: the standard-library
`ast` module walks each file with a stack of enclosing functions (the caller),
and ty (Astral's Rust type checker), driven over its LSP, resolves each call
site's callee — the definition's target file plus the enclosing qualname supply
the `full_name` and module path. It emits hinzu's `FactSet` JSON directly —
definitions, `call` and `reference` edges, and effect roots seeded by the
callee's declaration provenance. ty is the sole resolution backend; there is no
fallback resolver.

Project detection adds one row to the routing table: a `pyproject.toml`,
`setup.py`, or `setup.cfg` selects the Python adapter, which the CLI runs as
`python3 analyze.py` (`HINZU_PYTHON` picks the interpreter, `HINZU_PY_ADAPTER`
the script, `HINZU_TY` the `ty` binary; a missing `python3` or `ty` is an honest
nonzero failure, not a faked analysis). From the extracted facts onward nothing
is language-specific — the same store, engine, and policy check that Rust and
TypeScript already feed.

The seeded categories are the shared vocabulary again, minus `alloc`: `fs`,
`net`, `process`, `env`, `clock`, and `random`, keyed on ty's resolved
`full_name`. One Python-specific care is the `pathlib.Path` constructor, which is
pure — only its I/O methods count as `fs`. The shipped annotation set is
`crates/hinzu-core/annotations/python.toml`, and
[`python-catalog.md`](./python-catalog.md) records the full mapping.

Python is still the weakest-resolution adapter, because duck-typed receivers,
decorators, and dynamic import fall outside any resolver. That is where
Unknown-by-default earns its keep — each unresolved site is emitted as an
unknown-target edge, becomes an `Unknown`, and fails closed under the default
`on_unknown = fail`, so a call hinzu cannot see through reads as "cannot certify,"
never as false-pure. Running the shared pipeline over housekeeping (a pure-Python
fleet auditor, 82 files, 486 definitions) with an illustrative functional-core
policy proves it: ty resolved 89.5% of 2,449 call sites and seeded 22 effect
roots, driving `fs` coverage to 117 call edges because a real type system closes
the un-typed `pathlib` gap — `(ctx.workdir / name).is_file()` and
`target.parent.mkdir()` resolve to precise `pathlib::Path` `fs` roots instead of
Unknowns. The policy then flagged 20 forbidden-effect violations (14 process, 6
fs) with evidence paths such as `policy_conflicts -> member_config -> _file_text
-> RepoContext.try_api -> RepoContext.api -> run -> subprocess::run`, plus 86
`Unknown` findings. Because ty runs over its LSP as a subprocess today, a native
in-process ty backend is the planned future resolution primitive behind the same
`FactSet` seam once ty ships a stable Rust library API; see
[`python-catalog.md`](./python-catalog.md).
