# The self-check: hinzu turned on itself

hinzu enforces a functional core — effects live only where a policy allows them.
The tool now holds itself to that rule. A CI job runs `hinzu check` on
hinzu-core on every change and fails when an effect reaches a module that is
meant to be pure. This note records what the guard covers and the capabilities
it needed to bite honestly.

## The boundary it enforces

The policy lives in `hinzu-self.toml`. It reads the way any hinzu policy does:

- The functional core — `crates/hinzu-core/src/**` — may allocate and nothing
  else (`allow = ["alloc"]`), so it forbids the filesystem, network, database,
  subprocess, clock, randomness, and environment effects. This is the fact
  schema (`facts.rs`), the propagation engine (`effects.rs`), the policy check
  (`policy.rs`), and the root-seeding config (`roots.rs`).
- A carve-out sanctions the two files that are allowed I/O: `store.rs`, the
  SQLite fact store, and `lib.rs`, whose `check_facts` opens that store. Both
  may use the database and filesystem (and allocate); nothing else may.

A run today reports seven functions that reach the database — six in `store.rs`,
one in `lib.rs` — every one tracing to a `rusqlite` call, plus heap allocation
across the crate. All the database effects sit in the sanctioned files, and
allocation is allowed everywhere, so the core reaches no forbidden effect and
the check passes. If a filesystem, network, subprocess, or other forbidden
effect ever leaks into `facts.rs`, `effects.rs`, `policy.rs`, or `roots.rs`, the
run exits nonzero with the evidence path from the offending function down to the
operation that carries the effect, and the job fails.

## Why allocation is tracked but not forbidden

`alloc` is a first-class effect: hinzu ships annotations for the allocating
standard-library APIs (`Vec::push`, `Box::new`, `String` growth, `format!`,
`.collect()`, `Rc`/`Arc::new`, map and set inserts), so a call into one is a
heap-allocation root that propagates like any other effect. hinzu-core allocates
everywhere, and that is correct for a functional core — purity is about
observable side effects, not about never touching the heap. So the self-check
allows `alloc` in every region while still forbidding the real I/O effects. A
performance-sensitive project can instead forbid `alloc`; see the `[region.hot]`
example in `hinzu.toml`.

## Why the guard needed trusted external summaries

hinzu-core does real I/O, but not through the standard library: its database
access goes through the `rusqlite` crate, which links the bundled
`libsqlite3_sys` C library. The driver extracts a call edge into `rusqlite` but
never sees that crate's body, so a standard-library-only seed found no effects
in hinzu-core at all — the self-check would have passed for the wrong reason, by
seeing nothing rather than by confirming a clean core.

The fix is configurable effect roots and trusted external summaries
(`hinzu-core::roots`). A prefix table turns a call whose path begins with a
known effectful crate into a root of the matching effect — `rusqlite` and
`libsqlite3_sys` map to the database effect. The table has a built-in default
(shipped in `annotations/std.toml`) and `[roots]` / `[trust]` sections in the
policy extend it. With `rusqlite` seeded, the store's calls light up, the effect
propagates to everything that reaches them, and the boundary is a real
constraint rather than a formality.

## Why the guard needed Unknown

An unseen external call used to be read as pure: a call into a foreign crate
whose body the driver cannot walk contributed nothing, so a core function that
reached one looked clean. That is unsound — the foreign code could be doing
anything. Now an unseen callee that no annotation, effect root, or trusted-pure
baseline resolves becomes `Unknown`, an uncertainty marker that propagates up
the call graph like an effect. `hinzu check` fails on `Unknown` by default
(`[analysis] on_unknown = "fail"`): a functional core cannot be certified while
it reaches code hinzu cannot see. The report says so distinctly — "cannot
certify … reaches unknown external `serde_json::from_str`" — so it never reads
like a forbidden-effect violation.

hinzu-core reaches exactly three foreign crates the built-in baseline does not
already cover: `anyhow` (error construction and `.context(...)`), `toml`
(`toml::from_str`), and `serde_json` (`serde_json::from_str`). All three are
pure — they parse or build values in memory — so `hinzu-self.toml` vouches for
them in an explicit, auditable `[trust]` list:

```toml
[trust]
"anyhow" = "pure"
"toml" = "pure"
"serde_json" = "pure"
```

Remove any one line and the calls it covered turn back into `Unknown` failures,
each with the evidence path from the core function down to the external. That is
the guard working: the trust list is the shim, stated outside the source, and it
has to be honest for the check to pass.

## How the CI job stays clear of the stable build

The workspace is pinned to stable 1.96.0, because a newer rustc fails to compile
the `dbsp` dependency. The StableMIR driver needs a specific nightly with the
compiler's internal crates. The `self-check` job installs both toolchains but
keeps them apart: the CLI is built on stable, and the nightly compiles only the
driver and hinzu-core-under-the-driver — neither of which pulls in `dbsp`. The
stable `rust` job never sees the nightly, and the nightly never builds the
workspace. Each toolchain does the one job it is good for.
