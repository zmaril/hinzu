# A Rule Layer for hinzu: A Straitjacket for Semantics

## Where this sits

There are two tools in the fleet that read code and complain about it, and they
divide cleanly.

**straitjacket is a pure scanner.** It reads tokens and file shapes — a file too
long, a comment that lies, a hardcoded color, a font stack, an inline SVG, a
`useEffect` sitting in a component body. It never resolves a name, never follows
a call, never asks the type checker anything. That is its discipline and its
speed: point it at a tree and it emits findings with line-and-column evidence,
fails CI on any of them, and reads its config from the repo. A straitjacket for
syntax.

**hinzu is language understanding.** It exists for the questions a scanner
cannot answer: the call graph, the type system, the component tree, which
function an identifier resolves to, what a component renders. Its first analysis
was effect propagation — seed the operations that are inherently effectful,
propagate over the call/use graph to a fixed point, and fail any callable that
reaches a forbidden effect from a region that forbids it, with the evidence path
that explains why.

This note designs the layer that turns hinzu's second capability into
straitjacket's ergonomics: a **rule engine**. It runs over a repo, emits
findings with evidence paths, gates CI, and reads `hinzu.toml` from the repo —
the same contract straitjacket offers — but every rule is allowed to *understand*
the code, because it queries a fact database the compilers filled in. Effects
become the first rule rather than the only analysis. Three React and TypeScript
rules are the first peers, and one of them supersedes a syntactic straitjacket
rule by giving the real semantic evidence the scanner could only guess at.

This is a design note. It specifies the surface, the new facts the rules need,
and an honest fidelity read per rule. The implementation follows in later
pull requests.

## 1. The rule-engine abstraction

### A rule is a query over facts

hinzu already has the substrate. An adapter produces a `FactSet` —
`definitions`, `edges` (each a `call` or a `reference`, with a resolution
provenance), and `effect_roots`. The engine derives an `EffectSummary` per
definition: the set of transitively-reachable effects, each with one evidence
path down the call/use graph to the root. A policy then reports `Violation`s.

A **rule** generalizes exactly that shape. It is a query over the fact database
and the facts the engine derives from it, producing a set of **findings**. A
finding carries the same fields the effect check already reports:

- `rule` — the rule id that produced it (`effect-in-component`, `prop-drilling`).
- `definition` / `location` — the flagged callable or component, with its file
  and line range (what a policy region, or an editor, points at).
- `message` — the human-facing explanation.
- `evidence` — the path that justifies the finding: a chain of `SymbolId`s down
  the graph (an effect path, a prop-forwarding chain, a render chain). This is
  the field that separates hinzu's findings from a scanner's — a scanner reports
  *where*, hinzu reports *why*, hop by hop.
- `severity` — `Error` (fails the run) or `Warning` (reported only), reusing the
  existing `Severity`.

The current `Violation` type is this shape already, specialized to effects (it
carries an `effect` and a `region`). The rule layer lifts it into a shared
`Finding` the reporter and the exit-code gate consume uniformly, so the effect
check and a React rule land in the same report and gate the same way. The effect
region policy is not rewritten — it becomes the first producer of findings, and
the new rules are additional producers.

### The rule seam

A rule plugs into one trait, the way an `EffectEngine` does today:

```rust
/// One analysis over a fact set. The engine builds the shared derived facts
/// once and hands every enabled rule the same context; a rule reads what it
/// needs and emits findings. New rules implement this and register an id.
pub trait Rule {
    /// The stable id used in `[rules]` config and in every finding.
    fn id(&self) -> &'static str;
    /// Run the query and emit findings. `cx` carries the fact set plus the
    /// derived facts (effect summaries, forward adjacency, the component tree,
    /// the prop graph) so a rule never recomputes them.
    fn check(&self, cx: &RuleContext, config: &RuleConfig) -> Vec<Finding>;
}
```

`RuleContext` holds the `FactSet`, the per-symbol `EffectSummary` map, the
`forward_adjacency` used to reconstruct evidence paths, and the
language-aware derived facts the next section specifies (the component index,
the render tree, the prop-flow relation). The engine computes each derived
structure once, lazily, and shares it, so ten rules that all want the component
tree pay for it once.

`hinzu check` runs the flow it runs now — extract facts, seed roots, propagate —
and then folds every *enabled* rule over the shared context, concatenates the
findings, prints them, and exits non-zero when any finding is an `Error`.
Nothing about the CI contract changes: a green run is exit zero, a finding is a
non-zero exit and a printed evidence path.

### Registration and config

Rules register into a table keyed by id. A `[rules]` section in `hinzu.toml`
turns them on and configures them, next to the `[region.*]`, `[roots]`, and
`[trust]` sections the policy already reads:

```toml
[rules]
# Enable rules by id. Effects keep their existing [region.*] surface; the
# named rules below take a per-rule config table.
enable = ["effect-in-component", "prop-drilling", "one-component-per-file"]

[rules.effect-in-component]
# The effects a component's render path may not reach, and the seams where an
# effect is sanctioned (a hook callback, an event handler).
forbid = ["fs", "net", "db", "process"]
allow_seams = ["useEffect", "useLayoutEffect", "event-handlers"]

[rules.prop-drilling]
# Flag a prop forwarded, unused, through at least this many intermediaries.
max_depth = 3

[rules.one-component-per-file]
# Count only exported components; a private helper sub-component is allowed.
exported_only = true
```

Each rule owns the schema of its own `[rules.<id>]` table (its thresholds and
toggles), so a new rule adds a section without touching the ones already there.
The effect region policy keeps its established `[region.*]` / `on_unknown`
surface rather than being folded into `[rules]`, because a region is a
path-shaped concept and the other rules are structure-shaped; forcing them into
one grammar would blur both. `[rules].enable` gates the named rules; the region
policy runs whenever a `[region.*]` is present, as it does now.

### Effects as the first rule

Framed this way, effect propagation is the first and best-proven rule, not a
privileged mechanism. The three new rules are peers that read the same
`FactSet` and, where it fits, the same `EffectSummary` and evidence-path
machinery — `effect-in-component` in particular is almost entirely a
component-aware *view* over the effect summaries that already exist. Rule four
and rule five slot in behind the same `Rule` trait, consuming whatever derived
facts they need; adding one is an `impl Rule` plus, if the fact it needs is new,
one derived-fact builder and (usually) one adapter extension.

## 2. New facts the React and TypeScript rules need

The current `FactSet` carries callables and the call/use graph — enough for
effects, not enough to reason about components. The React rules need three
things the TypeScript adapter does not yet emit. Each is an **adapter
extension** (design only here); all three are decidable by the same tsc program
`analyze.mjs` already builds, using the type checker plus the JSX AST.

### Component identification (`is_component`)

A component is not a naming convention — straitjacket approximates it with
PascalCase because a scanner has no choice, and pays for it with helpers named
`Foo` that return data, and lowercase factory components that it misses. hinzu
can ask the type system. A definition is a component when either:

- its call signature's **return type is assignable to a React element type**
  (`JSX.Element` / `React.ReactElement` / `React.ReactNode`), which the checker
  answers with `checker.isTypeAssignableTo` against the JSX element type it
  already resolves for the program; or
- it is **used in JSX element-name position** somewhere — `<Foo />` resolves,
  via `checker.getSymbolAtLocation` on the opening-element tag, back to this
  definition. A component used but locally typed as returning a union still gets
  caught by its use site.

The adapter records this as a boolean **`is_component`** flag on the
`Definition` (a component is still an ordinary callable; the flag is a semantic
tag layered on it). `forwardRef(...)` and `memo(...)` wrappers are followed: the
call returns a value the checker types as a component and that appears in JSX
position, so the wrapped definition is flagged too — the wrapper is not a second
component. tsc decides all of this; a regex cannot.

### Render edges (the component tree)

To reason about a tree, hinzu needs the parent-to-child edges of that tree. When
a component's body contains `<Child .../>` and the tag resolves to a component
definition, the adapter emits a **render relation** `renders(parent, child)`
with the JSX site as evidence. Two representations are on the table; the design
picks the first:

- a **new edge kind**, `EdgeKind::Render`, alongside `Call` and `Reference`,
  carried in the same `edges` table; or
- a **separate relation** in the fact set.

A distinct `Render` edge kind is the better fit: it rides the existing `edges`
plumbing, the store, and the evidence-path reconstruction unchanged, and it is
kept out of the *effect* propagation (which unions only `call` and `reference`)
so rendering a child never launders the child's effects into the parent. The
render edges compose into the component tree the prop and one-component rules
walk. The JSX tag is resolved by the checker, so a re-exported or aliased child
resolves correctly, which a name match would not.

### Prop facts

The prop rules need a component's props and, per prop, whether it is *forwarded*
to a child or *used* in the body. Three facts, all from the first parameter's
type and the JSX AST:

- **`component_prop(component, name, type)`** — the props a component declares.
  The checker reads them off the first parameter's type members, whether the
  component destructures (`function Row({ user, onPick }: Props)`) or takes a
  named `props` object. This is a type query, not a syntax scan, so an
  interface-typed, imported, or intersected props type is enumerated correctly.
- **`prop_passed(component, prop, child, child_prop)`** — the component forwards
  its own prop into a child under a JSX attribute, unchanged:
  `<Child slot={prop} />` where the checker confirms the attribute expression is
  exactly the prop's parameter symbol (not a computed value). The child prop
  name may differ from the parent's — a *rename* — and because the fact is
  resolved through the symbol, not the spelling, hinzu follows the rename that a
  name-based scanner would lose.
- **`prop_used(component, prop)`** — the prop's parameter symbol is read
  somewhere in the body other than as a bare forward: in an expression, a
  condition, a hook argument, a computed attribute. The checker resolves each
  identifier read back to the parameter symbol, so a shadowing local of the same
  name does not count as a use.

Forwarded-without-use is then `prop_passed ∧ ¬prop_used` for a prop — the atom
the prop-drilling rule chains. What tsc gives cleanly is the *named, explicit*
case: a prop declared, read (or not), and passed by name. What it cannot fully
attribute is the spread — `<Child {...props} />` forwards every prop at once, and
while the checker knows the spread's type, it does not tell you which named
member flowed to which child slot as a single unchanged hop. That boundary is
the honest limit the prop-drilling rule inherits, spelled out below.

## 3. The three rules — mechanism and honest fidelity

### effect-in-component

**What it flags.** A component whose *render path* transitively reaches an effect
root (`fs`, `net`, `db`, `process`, …) outside a sanctioned seam. A component may
touch the world — but through a hook (`useEffect`, `useLayoutEffect`) or an event
handler (`onClick`, `onSubmit`), not synchronously while React is computing the
render. Synchronous I/O in a render body is the smell; the same I/O inside an
effect hook is correct React.

**Mechanism.** This is a component-aware policy view over hinzu's *existing*
effect propagation, and it should be described as exactly that — it invents
almost no new analysis. hinzu already computes, for every definition, the set of
reachable effects and the evidence path to each root. The rule restricts that
computation for a component to the subgraph of its **render path**: the call/use
edges reachable from the component body *without descending into a seam*. A seam
is a definition that is a hook callback (the closure passed to `useEffect`) or an
event-handler prop value; treated as a cut point, an effect that reaches a root
only by passing through a seam is sanctioned and produces no finding, while an
effect on the synchronous render path is flagged with the real evidence path to
the root.

**Why it supersedes straitjacket's version.** straitjacket's
`effect-in-component` matches the token `useEffect` inside a component span — it
can flag an effect hook written in the wrong place, but it is blind to the actual
I/O. It cannot see that a component calls `loadUser()` in its body, which calls
`fetch`; there is no `useEffect` token to match, and the network reach is three
calls away. hinzu's version reaches it through the call graph and prints
`Dashboard → loadUser → global::fetch`. It replaces a token heuristic with the
evidence path to the effect. The syntactic rule can retire.

**Fidelity: high.** The effects machinery is the most-proven part of hinzu, and
this rule is a projection of it, so its confidence tracks the effect analysis
itself (high on resolved calls, honestly `Unknown` on unseen externals, with the
reference-level rung — PRs #21 and #22 — already carrying higher-order and
import-time flows). The one piece of new work it needs is **seam detection**: the
adapter must mark which edges originate inside a hook callback or an event-handler
prop value, so propagation can cut at them. That is a local, decidable tag — the
closure argument to a `use*` call, or the value of a JSX handler attribute — and
until it lands, the rule can only run in a stricter mode that forbids the effect
anywhere in the component including its hooks (correct but noisier). Marking the
seam is what turns it from strict to precise.

### prop-drilling

**What it flags.** A prop threaded through at least `N` intermediate components
that forward it without using it — the classic drill, where a value is passed
down four layers only to be read at the bottom, and every layer in between is
noise that a context or a composition would remove.

**Mechanism.** The component tree (`renders`) plus the prop facts. Build the
forwarding graph whose nodes are `(component, prop)` pairs and whose edges are
`prop_passed` hops where the source prop is *not* in `prop_used` for that
component — a forward with no local use. A maximal chain through that graph is a
drill; its length is the depth. Flag a chain whose depth meets the configurable
threshold `max_depth` (default 3), reporting the chain as the evidence path:
`App.user → Page.user → Panel.user → Row.user`. This is the semantic counterpart
to straitjacket's `prop_graph` chain walk — same chain-length idea, but the hops
are resolved through the type checker (real component identity, real prop
identity, renames followed) rather than matched by attribute name across files.

**Fidelity: honest, with real limits.** This is the rule to be careful about,
because the pattern is easy to state and hard to see completely.

What is *detectable* well: an explicitly named prop, declared on a component,
passed by name into a child component you defined, and not read in between —
including across a **rename**, since the fact is symbol-resolved, not
name-matched. That is the core case and hinzu sees it more precisely than a
scanner.

What it *cannot* see, stated plainly so no one over-trusts a green run:

- **Spread forwarding — `<Child {...props} />`.** The checker knows the spread's
  type but not which named member flowed as a single unchanged hop, so a prop
  drilled purely through spreads is not chained. This is an under-approximation
  (a missed drill), and it is common in exactly the wrapper components that drill
  most.
- **HOCs and wrapper functions.** A prop threaded through `withRouter(...)` or a
  bespoke wrapper is not a JSX attribute forward; the chain breaks at the wrapper
  and the rule sees two short chains instead of one long one.
- **Context is not drilling.** A value delivered through `React.createContext` /
  `useContext` deliberately skips the intermediaries — that is the *fix* for
  drilling, not an instance of it. Because context values are not JSX attribute
  forwards, they never enter the graph, which is correct; the rule must not (and
  does not) count them, and this note records that as intended, not a gap.
- **Computed props — `<Child x={f(prop)} />`.** A prop wrapped in any computation
  is not a forward-unchanged, so it ends a chain. Correct in the common case, but
  an identity-ish wrapper (`x={prop ?? default}`) reads as a use and stops a
  chain that a human would still call drilling.
- **Renaming to a computed value, dynamic children, `cloneElement`, render
  props.** Out of reach; these end or hide a chain.

**Config and thresholds.** `max_depth` sets the reporting floor (a two-hop pass
is usually fine; a four-hop drill is not). The rule reports the deepest maximal
chain per origin prop so one drill is one finding, not one per hop. Given the
spread and HOC blind spots, prop-drilling ships as a rule that is precise when it
fires and silent when it cannot see — it should be read as "these are drills,"
never as "there are no others."

### one-component-per-file

**What it flags.** More than one *semantic* component defined in a single file —
the thing that makes a `.tsx` file hard to find your way around, and that
straitjacket already flags syntactically (`one-component`).

**Mechanism.** Trivial once `is_component` exists: count the definitions in a
file whose `is_component` flag is set; flag the file when the count exceeds one.
Because identification is semantic, this is strictly better than the PascalCase
version — a lowercase or oddly-named component still counts, and a PascalCase
helper that returns data does not.

**Edge cases, and how config resolves them:**

- **Private helper sub-components.** A small `<Row>` defined in the same file and
  used only by the file's main component is a real component by the flag, but
  many teams consider it fine. `exported_only = true` (the default) counts only
  exported components, so a private helper is allowed; a team that wants one file
  to expose exactly one component sets it false.
- **`forwardRef` / `memo` wrappers.** `const Button = memo(function Button() {…})`
  is one logical component, not two. Because `is_component` follows the wrapper to
  the wrapped definition and does not double-count the wrapper, the pair reads as
  a single component — the rule must lean on that identification, not on counting
  function nodes.
- **Non-component exports.** A file exporting one component plus hooks, types, and
  constants is fine — only `is_component` definitions count.

**Fidelity: high.** It rests entirely on `is_component`, which is a direct type
query, so it is as reliable as the component identification beneath it.

## 4. Extensibility and rollout

### The seam for future rules

Every rule is an `impl Rule` reading `RuleContext`. A new rule needs, at most,
one new derived-fact builder (if it reasons over something not yet derived) and
one adapter extension (if it needs a fact not yet emitted); often it needs
neither and is pure query. A few candidates show the shape the seam is meant to
carry:

- **`unused-export`** — an exported definition that nothing references. It needs
  the cross-file **reference edges** that PRs #21 and #22 add; the rule is then a
  reachability query (an export with no incoming reference or call edge), no new
  facts.
- **`missing-key-in-list`** — a `.map(...)` whose returned JSX element carries no
  `key`. Semantic where a scanner guesses: the checker confirms the callee is
  `Array.prototype.map` on a real array and that the returned element is a
  component/host element, so a `.map` over a non-array or a non-rendering body
  does not misfire. Needs a small JSX-in-map fact from the adapter.
- **`hook-rules`** — a hook (`use*`, resolved by declaration provenance, not by
  name) called conditionally or in a loop. Needs a control-flow fact (the call
  sits under a branch), which is a new derived fact but a language-independent
  one once the adapter marks conditional call sites.

Each is the same pattern: a query over facts plus derived facts, emitting the
same findings, gating the same way.

### Rollout

Implement this **after** PRs #21 and #22 merge. Both extend the exact adapters
this layer builds on — #21 brings the TypeScript adapter to reference-level
parity and adds the synthetic `<module>` node for import-time effects, #22 lifts
the same rung for the Rust StableMIR driver — and the component and prop facts
are a further TypeScript-adapter extension that should land on top of that work,
not race it. The reference edges those PRs add are also what a future
`unused-export` rule depends on, so the ordering compounds.

Test the rules on **powdermonkey** and **immersion** — both are real React
codebases the fleet has already swept, so their component trees, prop chains, and
in-render effects are known ground truth to calibrate the thresholds and the seam
detection against, rather than a synthetic fixture.

### Honest overall fidelity

- **`effect-in-component` — high**, because it is a view over the most-proven
  analysis hinzu has. Its one dependency is seam detection; without it the rule
  runs strict-but-noisy, with it, precise. Effect confidence caveats
  (`Unknown` externals, unresolved dispatch) carry over unchanged and are
  reported honestly rather than hidden.
- **`one-component-per-file` — high**, resting on a direct type query for
  component identity.
- **`prop-drilling` — medium, by construction.** It is precise on explicit named
  forwarding and follows renames a scanner cannot, but spread forwarding, HOC
  wrappers, and computed hops are genuine blind spots. It is a rule that is right
  when it speaks and quiet when it cannot see — useful, but never a proof that no
  drilling remains.

The rule engine's value is not any single rule; it is that "understand the code,
then complain about it with an evidence path" is now a repeatable shape. Effects
proved the shape. These three are the first peers. The seam is open for the rest.
