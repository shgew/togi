# Agents merge green pull requests

Status: Accepted.

[ADR 0025](0025-recorded-agent-review.md) decided that the owner merges and agents merge only when the owner asks. In practice the owner kept asking agents to merge, and the `main` ruleset already enforces what a merge needs: CI, robotogi's `review` check on the exact head and resolved review threads. In the last unattended run the owner was asked dozens of questions and accepted about 95% of the recommended answers unchanged. Agents also ran `just gate`, then `just check` from a cold cache before every push, so the suite ran about five times per push between the local ladder and CI. See [#492](https://github.com/shgew/togi/issues/492) and [#498](https://github.com/shgew/togi/issues/498).

## Decision

- An agent merges a pull request once `check`, `review` and resolved threads are green. The owner keeps the merge of a pull request that bumps `journal.Schema` or `tuner.Ruleset`, of a draft, of a pull request whose issue carries `needs-decision`, and of the release.
- An agent decides a reversible question that changes no tuner decisions, records the answer under the issue's Decided as agent-decided and continues. `needs-decision` is for every other question.
- `just gate` runs before every push. `just check` is required only for changes to `flake.nix` or `nix/`. CI stays the definition of green: it runs every flake check on the exact head of every pull request ([ADR 0016](0016-every-pull-request-runs-every-check.md)), and `just check` stays exactly that set ([ADR 0036](0036-flake-checks-stay-hermetic.md)).

This reverses ADR 0025's sentence "The owner merges. Agents merge only when the owner asks." Its recorded review, `review` check and ruleset stay. It also removes the premise of [ADR 0042](0042-opt-in-to-the-check-cache-locally.md) that local `just check` runs before every push; the opt-in cache remains useful for the pushes that need it.

## Considered Options

- **Keep owner-only merges:** rejected. The gate already holds every condition a human would check, and each merge waited on a request.
- **Merge on green with no exceptions:** rejected. A schema or ruleset bump changes what a recorded journal means, a draft is not offered for merge, an open owner question is not settled, and the release is the owner's call.
- **Keep `just check` before every push:** rejected. CI runs the same checks on the same head, so a cold local run repeats work that CI will do anyway; the hermetic checks matter locally only when the flake or the Nix sources change.
- **A faster impure local check counted as green:** rejected by ADR 0036.

## Consequences

A merge needs no owner message once the gate is green, so review and CI latency are the only waits. A defect that only a VM test or the macOS build would catch can reach CI before a local run; CI blocks the merge. Agent-decided answers sit on the issue for the owner to revert.
