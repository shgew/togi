# The coordinator reviews small pull requests

[ADR 0025](0025-recorded-agent-review.md) has every review coordinator split its pull request's hunks among parallel `reviewer` agents, so even a one-line change gets a coordinator and a reviewer. `just reviews` showed, on 2026-10-06, eleven reviewed pull requests with at most 2 files, fewer than 100 changed lines and no file under `internal/smu`, `internal/trial`, `internal/session`, `internal/journal` or `nix/`: a median of 6 minutes from opening to the first record, three P2 findings among them and no P0 or P1. [#366](https://github.com/shgew/togi/issues/366) and [#301](https://github.com/shgew/togi/issues/301) drop the reviewer spawn for such pull requests.

## Decision

The review coordinator reviews a small pull request itself instead of spawning reviewers. Small means at most 2 files and fewer than 100 changed lines, with no changed file under `internal/smu`, `internal/trial`, `internal/session`, `internal/journal` or `nix/`, measured on the pull request's whole diff and counting every changed file and line, including those the review excludes. `.omp/commands/review-pr.md` and `.omp/agents/review-coordinator.md` state the path; the record names the coordinator as the reviewer.

ADR 0025's gate holds: the coordinator is independent of the pull request's author, applies `REVIEW.md`, triages findings by `AGENTS.md`, and posts the review record and the `review` check. Review of only what changed ([ADR 0026](0026-review-only-what-changed.md)) is unchanged: the coordinator reviews later rounds of a pull request that is still small, and one that grows past small gets reviewers for its delta.

`AGENTS.md` keeps the review's trigger, identity and finding disposition and leaves its execution to `.omp/commands/review-pr.md`; `REVIEW.md` keeps the criteria and the record. No review rule is restated in all three.

## Considered Options

- **Keep reviewers for every pull request:** rejected. A coordinator that already reads the diff to size and split it adds a second agent only to read the same few lines again.
- **The author reviews its own small pull request:** rejected. ADR 0025's gate is an independent agent review; the coordinator keeps that independence at no extra spawn.
- **Small by size alone:** rejected. Offset writes, containment, the run loop, the journal and the NixOS module are where the serious defects in `REVIEW.md`'s criteria live; a few lines there still get a reviewer of their own.

## Consequences

A small pull request reaches its `review` check without a reviewer spawn. One agent both reads and triages its diff, so a defect that agent misses has no other agent reader before the owner. A pull request that starts small and grows past small mid-review gets reviewers only for the delta; its earlier rounds stay the coordinator's.
