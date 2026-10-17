# Agents share a host without locks

Status: see [the index](README.md).

[ADR 0057](0057-running-the-rust-migration-unattended.md) capped heavy jobs at 2 per host and kept `just ship`'s lock, which every worktree of a clone shares. AGENTS.md also told agents to run `just gate`, `just check` and benchmarks under a host-wide `flock`. Waiting behind another agent's run cost minutes per short check ([#603](https://github.com/shgew/togi/issues/603)). The owner decided in [#612](https://github.com/shgew/togi/issues/612) to drop the cap, the lock and the instruction.

## Decision

No recipe takes a lock. `just ship` runs `just gate` without one, then pushes. Agents do not queue checks or benchmarks behind each other. The dev shell no longer ships `flock`. This supersedes ADR 0057's heavy-job cap and `just ship`'s lock.

## Considered Options

- **Keep the shared lock:** rejected. Every run queues behind the one holding it, which is the cost [#603](https://github.com/shgew/togi/issues/603) reported.
- **Keep a numeric heavy-job cap:** rejected. It queues runs the same way, and only the lock and the instruction ever enforced it.

## Consequences

Parallel runs on one host contend for it and run slower. A busy host slows a check down instead of delaying its start. Benchmarks and timings taken while other agents run are noisier, so a claim drawn from them states only what was measured. Locks that serve other purposes are unaffected: togi's [host lock](../spec/runtime.md#host-lock) and `just same`'s lock on its target directory stay.
