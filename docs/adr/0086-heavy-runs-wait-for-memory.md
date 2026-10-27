# Heavy runs wait for memory, not for each other

Status: see [the index](README.md).

## Context

[ADR 0072](0072-agents-share-a-host-without-locks.md) dropped [ADR 0057](0057-running-the-rust-migration-unattended.md)'s lock and heavy-job cap because waiting behind another run cost minutes ([#603](https://github.com/shgew/togi/issues/603), [#612](https://github.com/shgew/togi/issues/612)). Since then, gates, checks, simulator shards, VM tests and benchmarks run side by side. Memory, not CPU, decides whether they fit.

One cold `just gate` on a 32 GiB, 32-thread host raised memory in use by about 19 GiB, and one simulator shard used up to 8.7 GiB. Several concurrent runs once exhausted RAM and swap, and the host killed unrelated processes. Most of a gate's memory sits in Nix builds outside the calling process.

## Decision

Recipes still take no lock, and a run never waits for another run to finish merely because it started first. Heavy commands go through the host job queue where the host has one: gates (`just ship`, `just gate`, `just push-fix`), `just check`, `just check-one`, test suites, coverage, benchmarks and Nix builds of checks.

A [host job queue](../../GLOSSARY.md#development) is a wrapper outside this repository. It starts a run once the memory it needs is free and counts the Nix builds the run requests toward the run's memory. When runs together outgrow memory, they slow down first; if they must be killed, they are killed before anything else on the machine. The host job queue does not contain or kill a run for its own size.

Waiting for memory is not a lock. A run starts as soon as it fits, whatever else is running. The repository names no particular host job queue, and no recipe calls one. Where a host has none, commands run bare. A contributor may declare a run's expected peak to the host job queue and may ask for an exclusive run for timing evidence.

This partly supersedes ADR 0072's statement that "agents do not queue checks or benchmarks behind each other": waiting for memory is allowed. Its "no recipe takes a lock" rule and its rejection of a numeric run cap stay.

## Considered Options

- **Keep ADR 0072 unchanged:** rejected. Concurrent runs exhausted RAM and swap and caused unrelated processes to be killed.
- **Restore a lock or a numeric heavy-job cap:** rejected. Runs queue behind each other regardless of whether they fit, which is the cost #603 reported.
- **Bound parallelism inside a run in the repository:** the repository, not the host job queue, owns this bound ([#1324](https://github.com/shgew/togi/issues/1324)); the queue decides only when a run starts.
- **Ship a host job queue in the repository:** rejected. Host policy differs per machine; recipes stay portable.

## Consequences

- A run may wait at the start instead of slowing everyone down.
- Queue time is not run time, so reported wall times exclude it.
- Timing-sensitive measurements run alone, asking the host job queue for an exclusive run where available, or say they were contended. ADR 0072's caveat still applies: benchmarks taken while other runs contend are noisier, and claims state only what was measured.
- The research dispatcher's per-benchmark lock in [Research campaign limits](../benchmarking.md#limits) is unchanged by this decision. [#1320](https://github.com/shgew/togi/issues/1320) decides whether a host job queue's exclusive run replaces it.
