# Compiler Facts: A Language-Independent Foundation for Effect Analysis

## Overview

This project provides a language-independent analysis layer that sits above existing compilers and type checkers. Rather than introducing a new language, compiler, annotation system, or runtime, it consumes semantic information already produced by language toolchains and transforms that information into a common representation suitable for whole-program analysis.

The initial motivating application is effect analysis: determining where programs observe or modify the outside world and enforcing architectural boundaries such as a functional core surrounded by an imperative shell. However, the system is intentionally designed as a general semantic analysis platform rather than an effect checker. Effects are simply the first analysis built upon it.

The project assumes that existing compilers already contain the deepest understanding of their respective languages. Instead of attempting to reconstruct this understanding from syntax alone, the system extracts semantic facts directly from compiler outputs and performs language-independent reasoning over those facts.

---

# Motivation

Modern software is increasingly written across multiple languages, each with its own compiler, parser, intermediate representations, and analysis ecosystem. While these toolchains differ substantially internally, they ultimately answer many of the same semantic questions:

* Which function is called?
* Which declaration does this identifier refer to?
* What type does this expression have?
* Which values escape a function?
* Which symbols are mutable?
* Which interfaces are implemented?
* Which functions may be reached from this entry point?

These facts already exist inside compilers because they are required for type checking, optimization, diagnostics, and code generation.

Most external tooling ignores this information and instead reconstructs approximations from source code, resulting in duplicated effort, reduced precision, and language-specific implementations.

This project exists to preserve compiler knowledge after compilation and make it reusable.

---

# Philosophy

The system is based on several guiding principles.

## Compilers are authoritative

Whenever semantic information is available from a compiler or type checker, that information is treated as authoritative.

The project does not attempt to replace compiler reasoning.

Instead, it preserves compiler reasoning in a reusable form.

---

## Normalize facts, not syntax

Different languages have fundamentally different syntax and internal representations.

Attempting to standardize abstract syntax trees or compiler intermediate representations inevitably loses information while remaining difficult to implement.

Instead, the project standardizes semantic propositions.

Examples include:

* function A calls function B
* symbol X refers to declaration Y
* variable V is written
* function F returns type T
* callable C escapes
* call site S invokes one of several possible targets

These concepts exist in nearly every language despite radically different syntax.

---

## Analysis is separate from extraction

Compiler adapters are responsible only for extracting semantic facts.

They should not perform architectural reasoning or policy decisions.

Analysis engines consume normalized facts.

This separation keeps language-specific code small while allowing multiple analyses to share the same foundation.

---

## Preserve uncertainty

Static analysis rarely has complete information.

Dynamic dispatch, reflection, generated code, runtime loading, and foreign interfaces introduce uncertainty.

Rather than hiding uncertainty, the system models it explicitly.

Unknown information is represented as data.

Analyses may choose to interpret uncertainty conservatively or optimistically depending on policy.

---

## Explain every conclusion

Every derived property must retain provenance.

If the system concludes that a function performs filesystem writes, it must be able to explain why.

Evidence consists of source locations, intermediate call paths, and originating semantic facts.

Users should never have to trust unexplained conclusions.

---

# Architecture

The system consists of three conceptual layers.

```
Compiler
        │
        ▼
Semantic Fact Extraction
        │
        ▼
Normalized Fact Database
        │
        ▼
Analysis Engines
```

Each layer has a single responsibility.

---

# Compiler Adapters

Compiler adapters translate compiler-specific semantic information into normalized facts.

Adapters intentionally remain thin.

Their responsibility is extraction rather than interpretation.

They should preserve information rather than classify it whenever possible.

An adapter should expose:

* symbols
* declarations
* callable bodies
* call sites
* type information
* inheritance relationships
* interface implementations
* reads
* writes
* allocations
* throws
* captures
* imports
* diagnostics
* compiler configuration relevant to interpretation

Adapters may additionally expose language-specific extensions without requiring the common schema to understand them.

---

# Normalized Facts

The normalized representation forms the semantic backbone of the project.

Facts describe relationships rather than implementation details.

Typical examples include:

* callable ownership
* resolved calls
* possible dispatch targets
* symbol identity
* storage locations
* value flow
* callback relationships
* mutation sites
* allocation sites
* exceptional control flow

Facts intentionally avoid embedding language syntax.

Consumers should not need to know whether information originated from JavaScript, Rust, Go, Java, or another language.

---

# Stable Identity

Compiler object identities are inherently ephemeral.

The system therefore assigns stable identities to semantic entities.

Identity should survive repeated analysis of unchanged code and remain meaningful across analysis sessions.

Stable identities enable incremental analysis, cached summaries, comparison between revisions, and cross-project reasoning.

---

# Analysis Framework

Analyses operate entirely on normalized facts.

Examples include:

* effect inference
* purity inference
* capability inference
* dependency analysis
* architectural verification
* taint propagation
* escape analysis
* ownership analysis
* determinism analysis

New analyses should require no compiler-specific code.

---

# Effect Analysis

Effect analysis is the initial consumer of normalized facts.

The analysis begins by assigning effects to known operations.

Examples include interaction with filesystems, networks, databases, clocks, randomness, operating system processes, or externally observable mutable state.

Effects then propagate through the call graph until a fixed point is reached.

The result is a summary for every callable describing both its direct effects and all effects reachable through transitive calls.

Every propagated effect retains its evidence path.

---

# Functional Core Verification

One intended application is enforcement of a functional core architecture.

Rather than inspecting imports or syntax, verification operates on semantic summaries.

Policies specify which effect categories are permitted within architectural regions.

Verification is transitive.

A function violates policy if it can reach prohibited effects regardless of how many intermediate calls separate it from those effects.

This allows architectural intent to be enforced independently of implementation details.

---

# Policy System

Policies are external to source code.

The system intentionally avoids requiring projects to adopt annotations throughout their implementation.

Instead, policies describe:

* architectural regions
* allowed effect categories
* prohibited effect categories
* trusted external summaries
* ignored paths
* confidence thresholds

Projects remain free to organize policy independently from implementation.

---

# Extensibility

The architecture assumes that analyses will grow over time.

Neither the normalized schema nor compiler adapters should be tightly coupled to any single analysis.

New effect categories, semantic properties, and inference engines should compose naturally with existing facts.

Compiler adapters should rarely require modification when analyses evolve.

---

# Incremental Analysis

The system is designed for repeated execution.

Only semantic changes should invalidate derived summaries.

Stable identities and persistent summaries allow analyses to reuse previous work whenever possible.

This enables practical use within editors, continuous integration, and large monorepositories.

---

# Trust Model

Different facts originate from different sources.

Compiler-derived facts are considered authoritative.

Derived facts inherit confidence from the evidence supporting them.

Analyses should distinguish between:

* proven properties
* inferred properties
* assumed properties
* unknown properties

Consumers may choose how conservative to be when interpreting uncertainty.

---

# Non-Goals

The project does not attempt to:

* replace language compilers
* define a universal intermediate representation
* introduce a new effect-aware programming language
* require source annotations throughout a codebase
* enforce a particular programming paradigm
* prove complete program correctness

Its purpose is to preserve semantic knowledge already produced by compilers and make that knowledge reusable.

---

# Future Directions

Although motivated by effect analysis, the architecture intentionally supports broader semantic reasoning.

Possible future analyses include:

* capability systems
* security policy verification
* concurrency analysis
* resource lifetime analysis
* ownership inference
* dependency visualization
* architectural conformance
* semantic code search
* incremental whole-program indexing
* automated documentation
* semantic differencing between revisions

Each of these analyses builds upon the same normalized semantic foundation.

---

# Conclusion

This project treats compilers as producers of semantic knowledge rather than merely generators of executable code.

By extracting, preserving, and normalizing compiler facts, it becomes possible to build language-independent analyses that are both more precise and more reusable than syntax-based approaches.

The result is a common semantic substrate upon which effect systems, architectural verification, and future program analyses can be constructed without modifying existing languages or requiring widespread changes to application code.
