# A native Rust rewrite, not a byte-compatible port

Status: see [the index](README.md).

Supersedes the Go-compatibility contract of [ADR 0048](0048-rust.md) and the comparison protection and Go-reference rules of [ADR 0057](0057-running-the-rust-migration-unattended.md). On 2026-10-11 the owner reversed the contract of the migration ([#634](https://github.com/shgew/togi/issues/634)): "I want my tool to be written in rust, not carefully replay everything that was happening in go. If some things change so be it." The change is recorded in [#737](https://github.com/shgew/togi/issues/737).

## Context

ADR 0048 made the port a replay of Go: the same journal bytes on every suite session, the same schema, config, CLI and dashboard cells, and a session on the target machine that continues across the cutover. ADR 0057 protected that comparison with a hash manifest, a `guard` status and a rule that Go bugs are ported byte for byte. Stage 2 then spent its crates on matching Go: a crate that reproduced Go's float formatting, `exp`, `log`, `pow` and `ExpFloat64` bit for bit and checked them against vectors Go generates; a port of Go's gzip and DEFLATE readers and `*os.PathError` texts; JSON decoding that folded case, kept the last duplicate key, limited nesting and replaced invalid bytes with U+FFFD the way `encoding/json` does. Each of these is code the finished tool would discard. togi is not 1.0, its users are its owner and a few others, and the owner accepts restarting the session.

## Decision

**Go is a reference for intent, not an oracle.** The Rust tool does what [`docs/spec/`](../spec/) and the README describe. A Go bug found while porting is fixed in the port and named in its pull request; a spec line that no longer matches is changed in the same pull request.

**Free to change:** the journal format and schema (Rust starts its own), `state.json`, archives, CLI flags and help, the config format, and the dashboard and other rendered output. Rust reads no Go-written journal, state or archive: the cutover ([#643](https://github.com/shgew/togi/issues/643)) starts a fresh session and leaves the old state directory behind. Std math, the `rand` crate's `StdRng` for every random stream, serde and `serde_json` defaults, std `io::Error` and flate2 replace the Go-compatible equivalents.

**Not negotiable:**

- **Hardware safety.** Offsets reach hardware only through the smu crate, clamped to [-50, 0], with a durable intent event before every write and a readback after, failure and joint marks honored before any write, and zeroing and stopping on journal failure. The typestate and receipt designs stay: target and observed offsets are different types, and the `smu.intent` receipt is minted by the journal crate and consumed by the session's write transaction.
- **Journal durability.** Append-only, an fsync per event, intent before action, torn-tail repair and the lock.
- **Architecture.** A synchronous core, with tokio only in trial, hw, watch and the binary, and one crate per responsibility.
- **Tests pin behavior:** rules, boundaries, invariants, crash and resume, and safety, as [`AGENTS.md`](../../AGENTS.md) says. A port carries over what each Go test proves, not its bytes or its random streams.

**Deleted:** the Go-compatibility crate and its vector generator and flake checks, the comparison harness and its flake checks, and the guard (its tool, its workflow, its recipes and `just land`'s wait for it). The Go reference stays in the tree until the cutover for reading, and the Go tools and `just same` stay for Go work. Reviewers and `/review-pr` are the safety net; a pull request merges when CI's `check` and robotogi's `review` pass and every thread is resolved. The cutover merge stays the owner's. Before it, the simulator benchmark compares the Rust suite's scores with Go's as a sanity check, not an equality gate.

## Considered Options

- **Keep the comparison and guard, and fix only the worst Go-specific code:** rejected. Every crate would stay bound to journals nobody will read again, and the protection machinery would keep costing a hash manifest entry for each fixture.
- **Keep byte-identical journals but drop the guard:** rejected. Without the guard the manifest is unenforced, and the byte contract is what forced Go's float formatting, error texts and JSON quirks into the crates.
- **Migrate old journals and state at the cutover:** rejected. It needs a reader for every Go schema and keeps Go's quirks alive in the format; the owner accepts a fresh session.

## Consequences

- ADR 0048's contract (byte-identical journals, unchanged schema, config, CLI and dashboard, a session that continues across the cutover, the 252-session suite figure) and its rule that comparison assets change only in pull requests the owner approves no longer apply. Its decision to move to Rust, its reasons and its considered options stand.
- ADR 0057's manifest, additive rule, guard, comparison CI shards, "Go is the reference" rule, the owner-approved label and the agent merge condition on the `guard` status are gone, with the merge rule above in their place. Its macOS decision (on `main`, the job also builds the Rust workspace and runs its tests, which partly supersedes [ADR 0046](0046-macos-checks-the-package-on-main.md)) and its parking, stop-list, heavy-job cap and dependency-update rules stay.
- [`docs/porting.md`](https://github.com/shgew/togi/blob/45c2df3f01a3cf06d96e894324e660efef8f6942/docs/porting.md) was reduced to the idiom map and the safety boundaries. The guide is preserved at that revision; its lasting rules moved to [AGENTS.md](../../AGENTS.md#rust) and the specs in [#918](https://github.com/shgew/togi/issues/918). The migration issues' Go-compatibility acceptance criteria are replaced by the contract above.
- Existing journals, state and archives of a Go session cannot be resumed by the Rust build, and the first Rust session starts from the BIOS values again.
