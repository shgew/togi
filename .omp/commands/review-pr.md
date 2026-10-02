---
description: Review pull requests in parallel and record their merge gates
argument-hint: "[PR numbers or URLs… | all]"
---

Review `$ARGUMENTS` in this repository. Read `AGENTS.md` and `REVIEW.md` first. Use `just bot` for GitHub actions. Keep temporary payloads outside the repository. Never merge; the owner merges.

## Resolve and fan out

Accept space- or comma-separated PR numbers or URLs. `all` or no argument selects every open non-draft pull request in this repository, without an age window. Fetch all pages, deduplicate explicit references, and reject references to another repository. Print the resolved numbers, titles, heads and bases before dispatch. An empty set finishes with an empty aggregate table.

Identify stack membership from `gh stack` and the PR base/head relationships. Dispatch ONE task coordinator per independent PR in ONE parallel task call, never a serial loop. Exception: send all layers of the same stack to ONE coordinator. Supply each coordinator its PRs, repository, worktree instructions, criteria and the entire workflow below. It must be able to spawn `reviewer` agents; ensure the available recursion depth permits that before dispatch. If it does not, report the configuration blocker instead of substituting self-review.

Each independent coordinator fixes its PR in its own worktree of the PR branch, never the main checkout. Reuse a worktree only if it belongs to that branch and has no unrelated edits; otherwise fetch the head and create a dedicated worktree. Stack coordinators use one worktree per layer, review layers in parallel, then apply fixes bottom-up. Restack once with `gh stack`, push the stack once through the App identity, and review fix commits. This ownership prevents parallel pushes racing on the same stack. A layer whose own changes are unchanged after restacking uses carry-forward below rather than a fresh review.

## Coordinator: freeze and split

For each assigned PR, fetch its title, body, head SHA, base SHA and changed files with added/removed counts. Read the linked issues, including `Refs #N`, and their Acceptance. Identify this layer's criteria and account for each. Save `just bot pr diff <N>` ONCE for that head. If the head changed while fetching, discard the snapshot and fetch a coherent one. Review remote context at the frozen SHA, or a checkout whose HEAD equals it.

List changed files and their +/- counts. Exclude paths excluded by `.coderabbit.yaml`'s `path_filters` (currently `go.sum`, `flake.lock` and `**/testdata/**`) and generated/binary files. List each exclusion and reason separately; records distinguish covered paths from exclusions. Tests not excluded stay with their implementation.

Use omp's bundled `/review` sizing on INCLUDED files: let L be added plus removed lines and F the file count. No included files means no reviewer tasks and explicit exclusion evidence, not an invented review. Otherwise use one reviewer if L < 100 or F <= 2; min(2, F) for L < 500; min(4, ceil(F / 3)) for L < 2000; min(8, ceil(F / 2)) for L < 5000; and min(16, F) beyond that.

Group by locality: same directory/module, related functionality, tests beside implementation. Spawn ALL `agent: "reviewer"` tasks for the PR in ONE parallel task call. Each gets only its assigned files and their frozen hunks, the PR title/body, linked issue Acceptance and the root `REVIEW.md`. Reviewers must use those hunks, not rerun `git diff` or fetch a newer PR diff. They may read surrounding context at the frozen SHA. Collect findings, priorities, files covered and verdicts from every reviewer. Do not define a project agent named `reviewer`, which replaces the bundled reviewer whole.

## Coordinator: CodeRabbit and findings

Fetch all pages of CodeRabbit PR reviews, inline comments, issue comments and review threads, including review bodies and collapsed outside-diff findings. Trial mode never seats bots automatically. An admin assigns `robotogi[bot]` a seat in CodeRabbit's Team Management, where bots are listed separately. Once seated, automatic review runs on PR open and every push, as configured in `.coderabbit.yaml`. Seat assignment does not start review retroactively: a PR opened before assignment needs one owner `@coderabbitai review`. Agents never post as the owner; CodeRabbit ignores App commands. See the [seat-assignment guide](https://docs.coderabbit.ai/management/seat-assignment.md).

While CodeRabbit is installed, wait for its completed review of the head unless it explicitly skips. An explicit skip (missing seat, rate limit or pause) is nonblocking: record its reason and comment URL. Accept a review whose `commit_id` matches the frozen head, or a completed summary comment explicitly identifying that head as reviewed (for example its commit range and review-coverage marker). A zero-finding pass may have only that summary, with no REST review. An old review or an in-progress summary does not count. If not installed, record evidence of absence. Missing output alone is not proof of absence or an explicit skip; a pending automatic or owner-requested review is a blocker. For a pre-seat PR still awaiting its one owner request after assignment, record `not_run` with that trigger evidence.

Triage every agent finding and every CodeRabbit inline or review-body finding by `AGENTS.md`. Fix real defects on the same branch in one push, with relevant proof. Reply with a reason to the rest. A deferred finding needs an issue and reason; it stays open if P0/P1. Reply to CodeRabbit threads through `just bot api repos/{owner}/{repo}/pulls/<N>/comments/{comment_id}/replies`; resolve them with the GraphQL `resolveReviewThread` mutation through `just bot api graphql`. Reply to review-body findings in a PR comment identifying each finding. Record each source, priority and outcome, including rejected or deferred findings.

After fixes, freeze the new head and fix diff once. Apply the same exclusions, file +/- listing, sizing and parallel reviewer split to that fix diff. Wait for CodeRabbit on the new head under the same rule, and collect new findings. Repeat until every finding has an outcome, all threads are resolved and no P0/P1 is open. Recheck Acceptance. A finding repeated in two PRs adds a lesson to `REVIEW.md` in the second fix.

## Coordinator: record and check

Fetch the head again. If it changed outside reviewed fixes, review the new changes before proceeding. Use the latest reviewed SHA, all covered files, exclusions and all findings across review rounds. Prepare the record exactly as defined in `REVIEW.md`, with human text and version-1 JSON carrying the same data. Post ONE record for that reviewed head with `just bot pr comment <N> --body-file <record-file>`; keep its returned URL. Reuse an existing record for the head rather than posting another.

Only when every finding has an outcome and no P0/P1 is open, prepare this payload with the reviewed SHA and record URL:

```json
{"name":"review","head_sha":"<reviewed full sha>","status":"completed","conclusion":"success","output":{"title":"Review recorded","summary":"Review record: <record comment URL>"}}
```

Post with `just bot api --method POST repos/{owner}/{repo}/check-runs --input <check-file>`. Verify the returned run names robotogi as its App, has the reviewed SHA and links the record. Fetch the current head once more; a changed head needs its own gate. Return one summary row per PR, including CodeRabbit status and record/check URLs. Remove coordinator-created clean worktrees after publishing; retain any worktree with unpublished fixes and report why.

## Coordinator: rebase carry-forward

Keep the earlier record, old base/head and new base/head. Run `git range-diff <old-base>..<old-head> <new-base>..<new-head>` for this layer, not the whole stack. Carry forward only if every patch pairs unchanged (`=`), with none added, removed or modified, and no unreviewed commits. Otherwise review the changes above. For unchanged patches, post a new `review` check on the new head; its summary links the earlier record and names the old/new ranges and range-diff evidence. Do not post another record or claim a fresh review. Check that current threads are resolved before success.

## Aggregate

Wait for all PR and stack coordinators. Report ONE table:

`PR | head | reviewers | P0–P3 findings | fixed | rejected | deferred | review check | blockers`

Show per-priority counts, disposition counts and check URLs. Include record links and CodeRabbit status in each row's review-check cell. Report blockers rather than omitting a PR. Never merge.
