# The Go effect catalog

hinzu has one flat, shared effect vocabulary. The category names mean the same
thing in every language: `fs` is `fs` for Rust, TypeScript, Python, and Go,
`net` is `net`, and so on. A language does not get its own namespace and does not
rename a shared category. What a language chooses is which categories it *seeds*
— the subset of the shared vocabulary its runtime actually exposes as a
certifiable effect. A category that does not apply to a language simply does not
appear for it.

Go seeds this subset: `fs`, `net`, `process`, `env`, `clock`, and `random`. The
sections below list what each one is seeded from, keyed on the callee's
declaration provenance — the import path and name gopls resolves a call to,
which is what the adapter reads.

## What Go seeds

- **`fs`** — the filesystem surface. The buffered and streaming I/O packages
  `io`, `io/ioutil`, and `bufio`; the path-walking helpers of `path/filepath`;
  and the file operations of `os` (`os.ReadFile`, `os.WriteFile`, `os.Open`,
  `os.Create`, `os.Mkdir`, `os.Remove`, `os.Stat`, `os.ReadDir`, and the rest).
- **`net`** — `net` (sockets, `Dial`, `Listen`) and the protocol packages built
  on it: `net/http`, `net/rpc`, `net/smtp`, `net/textproto`, and `crypto/tls`.
  `net/url` is deliberately absent — it is pure URL parsing and construction, not
  I/O.
- **`process`** — `os/exec` in whole (`exec.Command`, `Cmd.Run`, `Cmd.Output`,
  `LookPath`), plus the low-level `os.StartProcess`.
- **`env`** — reads of the ambient process environment and working directory:
  `os.Getenv`, `os.LookupEnv`, `os.Setenv`, `os.Environ`, `os.Getwd`, `os.Chdir`,
  `os.Hostname`, and the `os.User*Dir` helpers.
- **`clock`** — the `time` package: its wall-clock reads (`time.Now`,
  `time.Since`) and sleeps (`time.Sleep`, `time.After`, `time.Tick`).
- **`random`** — nondeterminism: `math/rand`, `math/rand/v2`, and `crypto/rand`.

`db` is a shared category, but Go reaches a database through a driver package
behind the `database/sql` interface, so `db` is declared per project with a
`[trust]` line rather than shipped as a built-in — for example
`[trust] "github.com/jackc/pgx/v5" = ["db"]` in `hinzu.toml`.

## Provenance is package-granular, and effects do not inherit

Go's effect map is keyed on the import path, and it is coarser than Python's for
a deliberate reason: Go has no submodule inheritance. A Python whole-module
effect flows down — `urllib.request` is net because `urllib` is — so its config
turns inheritance on. Go is the opposite: `net/url` is pure URL algebra even
though `net` is net, and `path/filepath` is its own package independent of
`path`. So the Go config sets `package_effects_inherit = false`, and each import
path carries exactly its own rule. A call into a nested import path that has no
rule of its own is not lit up by its parent.

Two shapes of rule appear in `crates/hinzu-core/annotations/go.toml`. A bare
import path (`os/exec = "process"`) is a whole-package rule: any call into that
package is the effect. A qualified row (`os::ReadFile = "fs"`) is a specific
rule: only that function is the effect, and it wins over any whole-package rule.

### The over-approximation caveat, stated honestly

For a checker, over-approximating an effect is the safe direction — it never
misses a real one — so a few packages take a whole-package rule even though part
of the package is pure:

- `io` and `bufio` are the `Reader` / `Writer` plumbing most real I/O flows
  through, so they are marked `fs` in whole. This does flag a function that only
  wraps an in-memory buffer through `io.Writer`. That is the safe direction; a
  project that wraps only in-memory streams clears it with `[trust] "io" =
  "pure"`.
- `path/filepath` is marked `fs` in whole for its walking and globbing, even
  though its path algebra (`Join`, `Dir`, `Base`, `Clean`) touches nothing.
- `time` is marked `clock` in whole for its wall-clock reads, even though its
  `Duration` arithmetic and `time.Parse` are pure.

Where a package is effect-*mixed* across two real categories, a whole-package
rule cannot express it, so it takes specific rows instead. `os` is the case that
matters: its file operations are `fs`, its environment accessors are `env`, and
the rest (`os.Args`, `os.Exit`, `os.Getpid`) is pure. So `os` carries a row per
effectful function rather than a blanket rule, and the pure remainder stays pure.

## Why there is no `alloc` for Go

Rust seeds an `alloc` effect: heap allocation is a real, certifiable cost a
performance-sensitive Rust region can forbid, and the standard library marks the
APIs that allocate. Go runs on a garbage-collected runtime where an allocation is
not an observable effect a functional-core policy can meaningfully forbid — the
collector, not the caller, governs it. So `alloc` is absent for Go, exactly as it
is for TypeScript and Python. It is absent, not renamed: there is no `go/alloc`
and no substitute category. Go seeds the subset above and nothing more.

## The extraction mechanism: the generic Rust LSP adapter, over gopls

Go is analyzed by hinzu's **generic Rust LSP extractor** (`crates/hinzu-lsp`) —
the same code that drives Python, with no Go-specific branch anywhere in it.
Everything Go lives as data in two files: the config
`crates/hinzu-lsp/configs/go.toml` (the gopls server command, `**/*.go` globs,
and the provenance rules) and the effect map
`crates/hinzu-core/annotations/go.toml`, which the config and hinzu-core's own
root seeding both read, so there is one source of truth. **gopls** (the Go team's
language server) is the sole resolution backend; the only non-Rust artifact on
the whole path is the external `gopls` binary the client spawns.

The extractor spawns `gopls serve`, opens every `.go` file, settles the first
check pass (plus a ready-probe on `exec.Command` so resolution does not race cold
start), then: `documentSymbol` per file → definitions; `prepareCallHierarchy` +
`callHierarchy/outgoingCalls` per definition → a real, type-resolved call graph.
An external callee's defining-file uri gives the provenance the effect roots key
on, and its qualified name (`os/exec::Command`) is reconstructed from the target
file's own `documentSymbol`. `_test.go` files are analyzed too — a project's
tests are part of what a functional-core policy governs — and `vendor/` and
`testdata/` are excluded.

`hinzu check` routes a directory with a `go.mod` to this path. gopls typechecks
the module to resolve calls into dependencies, so the adapter runs `go mod
download` first, best-effort: a stdlib-only module needs nothing fetched, so a
failure there is a note on stderr, not a hard stop. The honest capability edge is
gopls itself — if the `gopls` binary is absent the run exits nonzero with a clear
message; it never silently degrades and never fakes a resolution. `HINZU_GOPLS`
overrides the gopls binary path.

### Provenance: recognizing every GOROOT and module-cache shape

The extractor reconstructs a callee's provenance from its definition target file
via the config's provenance rules. Three shapes matter for Go, matched in order:

- a **downloaded toolchain's** standard library, shipped as a module under
  `.../pkg/mod/golang.org/toolchain@<ver>/src/...` when `GOTOOLCHAIN` fetches a
  newer Go than the host — still the standard library, so it is classified
  `stdlib`, and it is matched first because it lives under `/pkg/mod/`;
- a **module dependency** in the module cache
  (`.../pkg/mod/<module>@<ver>/<sub>/x.go`) — classified `module`, which is
  Unknown and fails closed unless a `[trust]` line vouches for it;
- the **GOROOT** standard library under `.../src/<import path>/x.go` —
  classified `stdlib`. The GOROOT rule is robust to how the directory is named
  and nested: a plain install (`.../go/src/...`), a versioned install
  (`.../go1.24.7/src/...`, what a `GOTOOLCHAIN` switch leaves on `PATH`), and the
  GitHub `setup-go` toolcache layout, which inserts version and architecture dirs
  between `go` and `src` (`.../go/1.24.7/x64/src/...`).

A pure standard-library call draws no edge, so it never becomes an Unknown. A
call into a third-party module becomes an edge to a `<import-path>::<member>`
symbol with no effect root, so it is Unknown until a `[trust]` line vouches for
it.

### Interface dispatch

A call through a Go interface resolves, at the call site, to the interface
method, not to every concrete implementation. The generic extractor handles this
with its existing `textDocument/implementation` follow-up: it asks gopls for the
implementations of the interface method and threads an edge to each. That is a
class-hierarchy-analysis over-approximation — it may include an implementation a
given call never reaches — which is the sound direction: it never misses a real
implementation's effect.

### Honest fidelity: call-only

`callHierarchy/outgoingCalls` reports only the calls gopls resolved, so the
generic extractor is **call-only**, exactly as it is for Python. It does not see
a function passed as a value (a `func` used as a callback), a call site gopls
could not resolve, or an ambient package-level variable read that is not a call
(`os.Stdout` is a variable, not a call). These need a language body walk,
deferred to a future language-agnostic tree-sitter rung (also Rust).
Unknown-by-default over the calls it does resolve keeps the result sound — a
resolved call into an unvouched third-party package is an `Unknown` that fails
closed, never a silent pure.

### Measured on curlie

Running the shared pipeline over [`rs/curlie`](https://github.com/rs/curlie) (a
small Go CLI that shells out to curl, 13 files) with an illustrative
functional-core policy — the argument-parsing `args/` and output-formatting
`formatter/` packages as the pure core, the root `main` package as the shell —
the extractor collected 30 definitions and 112 call edges (48 local, 11 effect,
47 pure-stdlib, 6 into unvouched third-party packages) and 4 effect roots. The
report flagged the subprocess spawn with its evidence path, `main.go#main ->
os/exec::Command`, and left the third-party `golang.org/x/term` and
`golang.org/x/sys/windows` console calls as Unknown ("cannot certify") that fail
closed. The `io`-as-`fs` over-approximation described above flagged curlie's five
formatter `Write` methods, each of which delegates through `io.Writer.Write` — in
curlie those writers wrap the real output streams, so the flag is defensible; a
project that wants the interface plumbing treated as pure clears it with a
`[trust]` line. The whole run stays a few seconds.

## How the extractor maps provenance to a category

The extractor reconstructs each external callee's canonical symbol from its
definition target file and its qualified name. A call into an owned source file
becomes a normal call edge; its effects propagate through its own body. A call
into one of the built-ins above becomes an effect root, seeded by that
declaration provenance and emitted with a canonical `<import-path>::<member>`
symbol (`os/exec::Command`, `os::ReadFile`, `net/http::Get`) — the same shape
Rust, TypeScript, and Python use, so a project's `[roots]` / `[trust]` overrides
work identically across all four languages.

hinzu-core carries the same table as a shipped annotation set,
`crates/hinzu-core/annotations/go.toml` — the Go counterpart to `std.toml`,
`node.toml`, and `python.toml` — so its Unknown classification agrees with what
the adapter seeds, and a project's `[roots]` / `[trust]` overrides apply
identically across all four languages.
