# Light checks for review fixes

Status: see [the index](README.md).

[ADR 0047](0047-gate-before-every-push.md) runs `just gate` before every push, and `just ship` pushes only after it. A review's fixes went through `just ship` too. Review is a pull request's critical path, and agent transcripts from 60 review coordinators put the median at 23 minutes, 26 with fixes. Of the 236 gate runs measured, the average took 2.9 minutes under the lock every worktree of a clone shares, and 69 transcripts show a ship waiting for another's lock. A fix to a reviewed pull request usually changes a few lines in a few packages that a reviewer has already read. See [#670](https://github.com/shgew/togi/issues/670).

## Decision

A fix pushed during a review runs `nix fmt`, `just lint` and `just focus` on the Go packages changed since the last pushed head, then pushes, through `just push-fix`. It runs no `just gate`. `just push-fix` refuses where `just ship` does (`main`, a detached HEAD, a dirty tree, a `gh stack` layer) and where origin has no such branch, since a pull request's first push is not a fix. A change to `go.mod` or `go.sum` focuses `./...`, and a fix with no Go file changed focuses nothing.

A pull request's first push still runs `just gate` through `just ship`. CI stays the definition of green ([ADR 0016](0016-every-pull-request-runs-every-check.md), [ADR 0047](0047-gate-before-every-push.md)): the required `check` runs every Linux flake check on the exact head of every push,. `just check` keeps its rule for `flake.nix`, `nix/` and darwin-only files.

The review coordinator reviews a fix diff itself when the diff meets the small rule of [ADR 0041](0041-coordinator-reviews-small-pull-requests.md), whatever size the pull request is. Otherwise the reviewers that covered its files review it, as before. This amends ADR 0041's limit that the coordinator takes only later rounds of a pull request that is itself still small.

## Considered Options

- **Keep `just ship` for fixes:** rejected. It reruns the whole gate and queues behind other worktrees for edits CI will check on the same head.
- **Skip every local check for fixes:** rejected. Formatting and lint failures cost a CI round trip, and the focused tests of the touched packages are cheap and catch most regressions a fix introduces.
- **Run `just gate` only when the fix touches a safety package:** rejected. The coordinator already sends such fixes to reviewers, and CI covers the rest; a second rule for the check ladder buys little for the added decision.
- **Have reviewers review every fix diff:** rejected. A few-line fix outside the safety packages costs a round of revived reviewers and adds no judgment beyond what the coordinator, which already triaged the finding, can give.

## Consequences

A fix can reach CI with a failure that only the gate's other checks (the full suite, `-race`, the integration-tagged tests, the flake checks) would catch; CI blocks the merge, and the fix's push costs one more round. A package the fix did not touch is not retested locally, including one that imports a touched package; `just lint` still covers the whole module. A fix diff that meets the small rule has one agent as both reader and triager, as ADR 0041 accepts for a small pull request. `AGENTS.md` states the exception to the check ladder and `.omp/commands/review-pr.md` the fix push and the review of a small fix diff.
