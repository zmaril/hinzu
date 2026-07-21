# Finishing a `hinzu model --emit quint` skeleton

`hinzu model --emit quint` lowers the language-neutral body IR (the same
`BodyFacts` the range analysis consumes) into a **Quint model skeleton**: one
`module derived { ... }` whose faithfully-lowerable parts are real Quint inside
`// ---- BEGIN GENERATED ----` / `// ---- END GENERATED ----` regions, and whose
every judgment call is an explicit `// AGENT-TODO` hole.

The skeleton is honest by construction. It never invents a value or a control
flow it cannot derive from the IR: where a faithful lowering needs a modeling
decision, it leaves a comment saying so. Because the holes are `//` comments, the
generated document parses as valid Quint as-is (`quint parse`), so you can fill
the holes incrementally and keep a parsing model at every step.

This note explains what each kind of hole is asking for.

## The generated regions (do not hand-edit)

Regenerate these from the IR rather than editing them in place — a re-run
overwrites them:

- **state vars** — one module-level `var` per local of every function, typed by
  the local's numeric kind. Names are `<fnkey>_l<localid>`, where `<fnkey>` is
  the function's symbol id with every non-alphanumeric character replaced by `_`
  (so no two functions collide). Local `0` is the return place; locals
  `1..=arg_count` are the parameters; the rest are temporaries. Each var carries
  the source function and `file:line` in a comment.
- **init** — every var set to a typed zero (`0` / `false`).
- **step** — `any { ... }` over the per-function actions.
- **the per-function `action` blocks** — the straight-line statements of each
  function's entry block, lowered to Quint assignments.

## The holes (fill these)

### `AGENT-TODO: Quint has no floats — choose an abstraction`
A `Float` local was typed as `int` as a placeholder. Quint has no float type.
Decide how to model it: a fixed-point integer scaling, an uninterpreted sort, an
interval, or (if the float is irrelevant to the property) drop it.

### `AGENT-TODO: unknown type — choose abstraction`
A local the extractor could not classify (`Other`) was typed as `int`. Pick the
abstraction the property needs, or model it as an uninterpreted value.

### `AGENT-TODO: choose real initial state`
`init` sets every var to a typed zero so the module is well-formed. Replace with
the real initial state — often `nondet` choices over the parameter domains, or
concrete constants for a specific scenario.

### `AGENT-TODO: encode control flow — this CFG needs a state-abstraction choice`
The function has more than one basic block (a branch, a loop, an `Assert`, or a
`Call` with a continuation). Rather than invent a program-counter state machine,
the skeleton prints a **CFG summary** — each block's terminator and successors —
and lowers only the entry block's straight-line statements as real derived
content. Encode the control flow the way the property needs: a `pc` variable and
one action per block, a guard-conjunction per path, or an inlined single-path
model when only one path matters. The CFG summary tells you the shape.

### `AGENT-TODO: environment nondeterminism — <var>' = <choose a value>`
An `Unknown` rvalue or a call's destination local: a value that enters from
outside the modeled code (an external call result, a message, a read). This is
where message loss, external results, and races enter the model. Replace the
keep-current-value placeholder with a `nondet` choice over the value's domain,
or with the environment action that produces it.

### `AGENT-TODO: unsupported binop` / `unsupported unary op`
An operator outside the modeled set (bitwise, shifts, and the like). Encode it
with the Quint operator or helper that matches the source semantics, then remove
the placeholder assignment.

### `AGENT-TODO: add environment actions (message loss, races, aborts)`
`step` currently chooses only among the derived function actions. Add the
environment actions the real system exhibits — dropped messages, reordering,
crashes — so the model checker explores them.

### `AGENT-TODO: state invariants`
The properties to prove. Replace the commented `val exampleInv = true` stub with
the real invariants (`val safety = ...`) and temporal properties, then check them
with `quint run` / `quint verify`.

## Workflow

1. `hinzu model --bodies <bodies.json> --emit quint --out derived.qnt`
2. `quint parse derived.qnt` — the skeleton parses as-is (holes are comments).
3. Fill the holes, re-running `quint parse` (then `quint typecheck`) as you go.
4. `quint run` / `quint verify` once the invariants are in.
