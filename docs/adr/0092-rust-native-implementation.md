# togi is a native Rust program; tools follow the same language rule

Status: see [the index](README.md).

## Context

The tuner is a state machine over journal events, and togi coordinates hardware writes and concurrent processes. Exhaustive enums make a missing event or decision case a compiler error; distinct domain types and journal-issued receipts express authority and ordering that string kinds and integers cannot. Safe Rust rules out data races, but types do not prove that cleanup runs or that hardware obeys a write.

Allocation design matters more than a language change alone. The original hot-path probe measured 4.21 s for Go with per-sample allocations, 0.47 s without them, 0.31 s for native Rust and 2.50 s for a straight Rust port using `HashMap`. The native Rust variant used 5.4 MB peak memory against allocation-reduced Go's 12.0 MB on that path. These are scoped probe measurements, not a claim about the finished program or hardware tuning. Rust's expected fit for agent-driven development was also a motivation, not a demonstrated productivity result; the [migration retrospective](../rust-migration.md) records the evidence and its limits.

Reproducing Go's float formatting, math, random streams, JSON quirks, compression and error text would preserve an implementation rather than the intended behavior. Development programs that parse versioned files or enforce refusals also benefit from Rust's types and precise in-memory tests; simple command-output pipelines do not need a crate.

History: [0001](0001-go.md), [0048](0048-rust.md), [0058](0058-native-rust-rewrite.md) and [0071](0071-no-go-in-the-repository.md). These records retain the original language choice and abandoned compatibility contract.

## Decision

**togi is native Rust, not a byte-compatible Go port.** The [specs](../spec/) define behavior; Go is historical evidence of intent, never an oracle. Rust does not read Go-written journals, state files or archives, and there is no promise to reproduce Go's CLI bytes, dashboard cells, random streams or decoding quirks. Use standard Rust libraries and ecosystem conventions rather than compatibility implementations. Current Rust compatibility rules remain owned by [reset and compatibility](../spec/runtime.md#reset-and-compatibility), not by the dropped Go contract.

**The core is synchronous and split into crates by responsibility.** Tokio belongs in crates that wait on the outside world, not in the tuner or journal. The [Rust guidance](../../AGENTS.md#rust) owns concrete idioms. Safety and durability are not relaxed by choosing native implementations: the [write transaction](../spec/runtime.md#offset-types-and-the-write-transaction), [journal rules](../spec/journal.md#rules), [emergency path](../spec/runtime.md#exit-codes) and [containment](../spec/workloads.md#containment) own those requirements and the limits of the type-level proof.

**There is no Go implementation or Go toolchain in the repository.** Allocate development programs by what they do:

- A program with a parser, a versioned format or tests pinning refusals is a Rust `togi-tool-*` crate, following the workspace's error, lint and test conventions. Release, review and coverage programs follow this rule.
- A script that turns `gh` or `git` output into text is Nushell; the board report is such an orchestration pipeline.
- A Rust tool calling GitHub uses `gh api` rather than adding its own HTTP and TLS stack. [Development](../development.md#crates-and-toolchain) owns crate registration and checks.

## Considered Options

- **Stay in Go with allocation fixes:** rejected. Those fixes captured most of the probe's speedup, but retained string- and integer-typed kinds without compiler-enforced exhaustiveness, garbage collection and the remaining measured memory and execution cost. Go's fast compiles, test-result caching, formatter and familiar packaging were real benefits, but no longer decided the language.
- **A verbatim port followed by refactoring:** rejected. The probe's straight port was much slower than its native design, and carrying payload registries, maps plus sorting and wrapped-error shapes would require a second rewrite.
- **A Go–Rust FFI bridge:** rejected. Cross-language integration was a major cost in the migration experience considered by the original decision; togi has no need to retain such a bridge.
- **Keep byte-identical journals and a comparison guard, or drop only the guard:** rejected. Either keeps crates bound to obsolete Go formats and forces Go-specific formatting, error and JSON behavior into the implementation; removing the guard does not remove that contract.
- **Migrate Go journals and state:** rejected. Readers for every Go schema would keep those quirks alive. The native rewrite accepts a fresh session instead.
- **Nushell for every tool:** rejected. Its lack of a type checker for a changelog parser and slower, less precise end-to-end refusal tests do not fit format-owning programs.
- **Rust for every tool:** rejected. Reshaping `gh` JSON into a board report is expressed directly as a pipeline; a crate adds a build and test harness without corresponding value.
- **Keep Go development tools:** rejected. A second toolchain, linter, formatter configuration, dependency feed and set of conventions is not justified for a few small programs.

## Consequences

There is one typed implementation and no Go-compatibility layer to maintain. Tool dependencies use the workspace lockfile and checks. Tests pin specified behavior, invariants and safety rather than another implementation's bytes; native libraries and domain types can express that behavior directly. Compile costs and productivity claims still require evidence: the language choice alone does not establish either a speedup or correct hardware behavior.
