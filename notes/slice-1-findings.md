# Slice 1 findings: effect analysis on real Rust code

This records the first end-to-end run of hinzu on a real Rust project. The
pipeline is now whole: a native StableMIR driver extracts a call graph and
standard-library effect roots, a DBSP circuit propagates effects to a fixed
point, and the policy check reports violations with an evidence path. The
target is straitjacket, a single-package file scanner with genuine filesystem
and environment access.

## What ran

`hinzu check /workspace/Straitjacket --policy <policy>` with no pre-extracted
facts. The command builds the `hinzu-rustc-driver` binary under its pinned
nightly, then compiles straitjacket with that driver set as
`RUSTC_WORKSPACE_WRAPPER`. The driver wraps only straitjacket's own crates (its
library and binary); the registry dependencies compile with the ordinary rustc.
Each wrapped crate walks its monomorphized MIR and writes a facts file in
hinzu's schema. The CLI merges the two files, ingests them into the SQLite fact
store, propagates with the DBSP engine, and checks the result against the
policy.

## Counts

The two compilation units combine into one fact set:

| metric | value |
| --- | --- |
| function definitions | 341 |
| distinct call edges | 1171 |
| statically resolved edges | 1170 (99.91%) |
| indirect or unresolved edges | 1 (0.09%) |
| standard-library effect roots | 4 |
| transitively effectful functions | 8 |

The four roots are three filesystem operations
(`std::fs::read_to_string`, `std::fs::read`, `std::fs::write`) and one
environment read (`std::env::current_dir`). The eight effectful functions are
`resolve`, `load_file_config`, `react_sources_by_project`, `build_react_indexes`,
`report_prop_chains`, and `main` in `src/main.rs`, `config::load_config` in
`src/config.rs`, and `project::Projects::discover` in `src/project.rs`.

## Evidence paths

Each path runs from a function down the call graph to the standard-library
operation that carries the effect. These are the report's own output, verified
against the source:

```
[fs]  resolve -> load_file_config -> config::load_config
        -> std::fs::read_to_string::<&std::path::Path>
[env] main -> resolve -> load_file_config -> std::env::current_dir
[fs]  report_prop_chains -> react_sources_by_project
        -> std::fs::read::<&std::path::PathBuf>
[fs]  project::Projects::discover -> std::fs::read_to_string::<&std::path::Path>
```

The source confirms each hop: `resolve` (main.rs:269) calls `load_file_config`
(main.rs:329), which calls `config::load_config` (config.rs:61), which reads a
file (config.rs:63); `load_file_config` also reaches `std::env::current_dir`
(main.rs:336); and `react_sources_by_project` (main.rs:369) reads a file
(main.rs:378).

## The policy and its result

The run used a functional-core policy. The analysis engine — parsing, rules,
graphs, and reporting — must reach no effects. Filesystem and environment access
are allowed only in the IO layer: the binary entrypoint, config loading, and
project discovery.

```toml
[analysis]
ignore = ["**/tests/**"]

[region.core]
paths  = ["src/**"]
forbid = ["fs", "net", "process", "env"]

[region.adapters]
paths = ["src/main.rs", "src/config.rs", "src/project.rs"]
allow = ["fs", "net", "process", "env"]
```

Against this policy hinzu reports no violations, and the command exits zero. All
eight effectful functions sit in the three files the policy designates as
adapters; the analysis core reaches no effect. That is the result the layering
predicts, and hinzu confirms it.

A stricter policy that forbids these effects across the whole of `src` — with no
adapter carve-out — turns the same eight functions into eleven reported
violations (some functions carry two effects), each printed with the evidence
path above. The two runs are the two ends of the tool: one states where effects
are allowed and passes; the other enumerates every function that reaches an
effect and where it enters.

## Honest limits

- **One unresolved edge.** `RegexRule::scan_line` (src/rules/patterns.rs:66)
  calls a function through a stored pointer, `(self.judge)(&caps)`. The callee's
  type is a function pointer, not a concrete function definition, so there is no
  static target to resolve. The driver records the edge as unresolved rather
  than inventing a callee. Any effect reachable only through that pointer would
  be missed; this is the soundness gap, surfaced rather than hidden. No
  `dyn`-trait virtual calls survived to MIR here, because monomorphization
  lowered the generic and trait calls to concrete functions.

- **The driver is pinned to one nightly.** It uses `rustc_private` and links
  against `librustc_driver`, so the binary is valid only for the exact nightly
  it was built against (`nightly-2026-07-18`). A different nightly needs a
  rebuild and possibly source edits, because the `rustc_public` API is still
  unstable. This crate is excluded from the workspace default members, so the
  stable build, the linters, and CI never compile it.

- **Effect roots are matched by name.** The driver tags a callee as an effect
  root when its path begins with a known standard-library prefix
  (`std::fs`, `std::net`, `std::process`, `std::time`, `std::env`) or a random
  crate. Generic type arguments are stripped before matching, so a type such as
  `std::fs::FileType` appearing as a type argument does not register as a
  filesystem effect. The seed list is deliberate and small; widening it is a
  later step.

## Engine cross-check

The DBSP engine is the default. The reference breadth-first engine
(`--engine naive`) is kept as an independent check: on the same facts the two
produce the same effect set for every function, so a divergence would be a bug
in one of them. The unit tests assert this equality on small graphs, and the CLI
exposes both so a run can be reproduced either way.
