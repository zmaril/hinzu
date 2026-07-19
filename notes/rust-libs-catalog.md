# The Rust library effect catalog

hinzu has one flat, shared effect vocabulary. The category names mean the same
thing in every language: `fs` is `fs`, `net` is `net`, and so on. For Rust,
`std.toml` ships the standard library's effect surface and its allocation surface.
This catalog covers the layer above that: `rust-libs.toml`, the shipped pack for
the third-party crates the fleet reaches most often.

The point of the pack is to keep a plain functional-core check honest without
making it noisy. A no-body call into a registry dependency is, by default,
`Unknown` — it fails closed under `on_unknown = fail`, reported as "cannot
certify." That is the right default for a crate nobody has vouched for, but it
means a project that uses `serde_json` or `anyhow` has to write a `[trust]` line
for each such crate before its core certifies. The pack ships those vouches as a
built-in default so the common case works out of the box, while holding to the
one rule that keeps the result sound: a crate that does I/O is never marked pure.

## How it loads

`rust-libs.toml` is merged onto `std.toml` to form the Rust base in
`crates/hinzu-core/src/roots.rs` (`RootSeeds::for_language(Language::Rust)`). It is
the same data format as every other pack — `[roots]` maps a path prefix to one
effect, `[trust]` vouches a crate pure or declares its effects — and it is matched
by the same segment-aligned, most-specific-wins matcher. A project's own
`hinzu.toml` merges last, so a project always overrides the pack: a rule with the
same prefix wins, a new prefix is added.

The resolution order (from `roots.rs`) is what lets a mixed crate be graded
precisely. A pure vouch is consulted *before* the effect table, so the two never
collide: a crate is vouched pure only on the prefixes that are genuinely pure, and
its effectful entry points are listed in `[roots]` under prefixes the pure vouch
does not cover. `chrono` is the clearest example — it is not blanket-pure, because
`chrono::Utc::now` reads the clock, so the pure vouches name `chrono::NaiveDate`,
`chrono::DateTime`, and the other value types, while `chrono::Utc::now` is a
`clock` root.

## The pure crates

These compute over in-memory buffers and do no I/O, so they are vouched pure
wholesale:

- **Serialization** — `serde`, `serde_json`, `serde_yaml`, `serde_derive`,
  `toml`, `toml_edit`. A serializer turns a data model into bytes; when those
  bytes are then written to a file or a socket, that write is a separate standard
  library edge the analyzer already sees.
- **Text, parsing, hashing** — `regex` (and `regex_syntax`, `regex_automata`),
  `sha1`, `sha2`, `digest`, `itertools`. All pure computation.
- **Errors** — `anyhow`, `thiserror`: construction, context, and formatting, no
  I/O.
- **Code generation** — `genco` builds token trees in memory; writing them out
  goes through a caller-supplied writer, whose effect is its own edge.
- **In-memory channels** — `crossbeam_channel`, `crossbeam_deque`: message
  passing, not OS I/O.
- **oxc** — the JavaScript/TypeScript parser, AST, and semantic crates
  (`oxc_parser`, `oxc_ast`, `oxc_semantic`, `oxc_span`, and the rest): arena
  allocation and tree walking, entirely in memory.

## The effectful and mixed crates

- **`fs`** — `ignore` walks the filesystem; `gix` and its disk-backed component
  crates (`gix_odb`, `gix_ref`, `gix_index`, `gix_config`, and so on) read a
  repository's objects, refs, and config from the `.git` directory; the `arrow`
  file and stream codecs (`arrow_ipc`, `arrow_csv`, `arrow_json`, and the
  `arrow::ipc` / `arrow::csv` / `arrow::json` re-exports) read and write files.
  The in-memory columnar surface of arrow (`arrow_array`, `arrow_buffer`,
  `arrow::compute`, and so on) stays pure.
- **`net`** — `gix_transport` and `gix_protocol` open sockets for fetch and push;
  `native_tls` and `rustls` exist to secure network connections, so they are
  marked `net` as the safe over-approximation.
- **`db`** — `duckdb` is wholly a database binding; `postgres` and
  `tokio_postgres` are `db` for their query surface, and their connect entry
  points are additionally `net` because establishing a connection opens a socket
  (the more specific rule wins for those callees, and a function that connects and
  queries collects both effects).
- **`env`** — `clap`'s argv/env readers: the derive `Parser::parse` family and the
  builder `Command::get_matches` family read the process's arguments (and, with
  clap's `env` feature, environment variables). clap's argument *builder* and the
  parsed-match *accessors* are pure and are vouched so, kept out of the pure
  prefixes only for the entry points that actually read argv.
- **`clock`** — `chrono::Utc::now` and `chrono::Local::now` (and their
  `chrono::offset::…` spellings). The rest of chrono — parsing, formatting, date
  arithmetic — is pure.
- **`random`** — `uuid`'s entropy- and time-based constructors (`new_v4`,
  `new_v7`, `now_v7`, `from_entropy`). Parsing and formatting one is pure.
  (`rand` and `rand_core` are already `random` in `std.toml`.)
- **`tokio`** — the async I/O submodules carry their effects: `tokio::fs` is `fs`,
  `tokio::net` is `net` (already in `std.toml`), `tokio::process` is `process`,
  `tokio::time` is `clock`. tokio's runtime, task, and synchronization primitives
  are pure — a future's effects are the future's own edges.

## Why `alloc` is absent here

`std.toml` already carries the standard library's allocation surface, and these
crates allocate the same way all Rust code does. A performance region that forbids
`alloc` is served by that standard library table; the library pack does not repeat
it, so it never flags an ordinary library call as an allocation.

## Why the over-approximations are honest

`gix` is annotated `fs` on its whole umbrella prefix, which also catches gix's
in-memory parsing helpers that touch no disk. That is deliberate: the pack prefers
to over-report an effect than to risk a false pure on a crate whose main job is
reading `.git`. A project that isolates gix's pure helpers can narrow this with
its own `[trust]` line. The same reasoning applies to the TLS crates, marked `net`
though they are sans-I/O in the strict sense — they exist for network security, so
`net` is the safe label.

## Fidelity: call edges plus native reference edges (MIR)

The pack seeds *which* library operations are effects; the StableMIR driver
decides *which functions reach them*. That driver is no longer call-only. Beyond
each body's `Call` terminators it walks the body's statements and operands and
draws a `reference` edge whenever a function item or closure is used as a **value**
rather than called — passed as a callback (`register(foo)`), assigned, returned,
reified to a fn-pointer, stored in a struct field
(`RegexRule { judge: judge_font }`), or captured in a closure handed elsewhere —
and it walks referenced closure bodies (previously recorded as bare, un-walked
definitions) so their effects surface. A `static`/`const` initializer — including a
`LazyLock`/lazy static, the Rust analogue of a module-level import-time effect — is
walked and attributed to the static's own id. Resolution rides the same
`Instance::resolve` → provenance → effect path as calls, so a referenced pack
operation (`register(reqwest::get)`) taints its user exactly as a direct call
would. This is the reference-level rung the Python and TypeScript adapters already
reached, done natively from monomorphized MIR — so **the call-only caveat is lifted
for Rust**. It is sound-additive: reference edges only ever add the higher-order and
import-time effects the call graph could not see, never remove a real one. See
[`getting-started.md`](./getting-started.md) (the straitjacket before/after) for
the measured effect.
