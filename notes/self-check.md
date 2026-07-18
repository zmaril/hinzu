# The self-check: hinzu turned on itself

hinzu enforces a functional core — effects live only where a policy allows them.
The tool now holds itself to that rule. A CI job runs `hinzu check` on
hinzu-core on every change and fails when an effect reaches a module that is
meant to be pure. This note records what the guard covers and why it needed one
new capability to bite.

## The boundary it enforces

The policy lives in `hinzu-self.toml`. It reads the way any hinzu policy does:

- The functional core — `crates/hinzu-core/src/**` — forbids the filesystem,
  network, database, subprocess, and environment effects. This is the fact
  schema (`facts.rs`), the propagation engine (`effects.rs`), the policy check
  (`policy.rs`), and the root-seeding config (`roots.rs`).
- A carve-out sanctions the two files that are allowed effects: `store.rs`, the
  SQLite fact store, and `lib.rs`, whose `check_facts` opens that store. Both
  may use the database and filesystem; nothing else may.

A run today reports seven effectful functions — six in `store.rs`, one in
`lib.rs` — every one of them a database effect that traces to a `rusqlite`
call. All seven sit in the sanctioned files, so the core reaches no effect and
the check passes. If an effect ever leaks into `facts.rs`, `effects.rs`,
`policy.rs`, or `roots.rs`, the run exits nonzero with the evidence path from
the offending function down to the operation that carries the effect, and the
job fails.

## Why the guard needed third-party roots

hinzu-core does real I/O, but not through the standard library: its database
access goes through the `rusqlite` crate, which links the bundled
`libsqlite3_sys` C library. The driver extracts a call edge into `rusqlite` but
never sees that crate's body, so a standard-library-only seed found no effects
in hinzu-core at all — the self-check would have passed for the wrong reason,
by seeing nothing rather than by confirming a clean core.

The fix is configurable effect roots (`hinzu-core::roots`). A prefix table
turns a call whose path begins with a known effectful crate into a root of the
matching effect — `rusqlite` and `libsqlite3_sys` map to the database effect.
The table has a built-in default and a `[roots]` section in the policy extends
it. With `rusqlite` seeded, the store's calls light up, the effect propagates to
everything that reaches them, and the boundary is a real constraint rather than
a formality.

## How the CI job stays clear of the stable build

The workspace is pinned to stable 1.96.0, because a newer rustc fails to compile
the `dbsp` dependency. The StableMIR driver needs a specific nightly with the
compiler's internal crates. The `self-check` job installs both toolchains but
keeps them apart: the CLI is built on stable, and the nightly compiles only the
driver and hinzu-core-under-the-driver — neither of which pulls in `dbsp`. The
stable `rust` job never sees the nightly, and the nightly never builds the
workspace. Each toolchain does the one job it is good for.
