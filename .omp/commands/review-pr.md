---
description: Review pull requests in parallel and record their merge gates
argument-hint: "[PR numbers or URLs… | all]"
---

Review `$ARGUMENTS` in this repository. Read `AGENTS.md` and `REVIEW.md` first. Use `just bot` for GitHub actions. Keep temporary payloads outside the repository. Never merge; the owner merges.

## Resolve and fan out

Accept space- or comma-separated PR numbers or URLs. `all` or no argument selects every open non-draft pull request in this repository, without an age window. Fetch all pages, deduplicate explicit references, and reject references to another repository. Print the resolved numbers, titles, heads and bases before dispatch. An empty set finishes with an empty aggregate table.

Find each PR's latest robotogi record: the last comment ending in a `togi-review` block. No record means a first review. A record on the current head with its `review` check needs nothing more; report it. A head that moved since the record gets a re-review, normally limited to the delta; legacy records with unrecoverable bases use the conservative fallback below.

Identify stack membership from `gh stack` and the PR base/head relationships. Dispatch ONE task coordinator per independent PR in ONE parallel task call, never a serial loop. Exception: send all layers of the same stack to ONE coordinator. Before spawning, check `read history://` for a resumable coordinator this session already ran for that PR or stack; idle and parked agents count, aborted agents do not. Send it the PR numbers and new heads with `write agent://<id>` instead of spawning another: it already holds the diff, its reviewers and the record. Spawn for PRs without a resumable coordinator. Supply each new coordinator its PRs, repository, worktree instructions, criteria and the entire workflow below. It must be able to spawn `reviewer` agents; ensure the available recursion depth permits that before dispatch. If it does not, report the configuration blocker instead of substituting self-review.

Each independent coordinator fixes its PR in its own worktree of the PR branch, never the main checkout. Reuse a worktree only if it belongs to that branch and has no unrelated edits; otherwise fetch the head and create a dedicated worktree. Stack coordinators use one worktree per layer, review layers in parallel, then apply fixes bottom-up. Restack once with `gh stack`, push the stack once through the App identity, and review fix commits. This ownership prevents parallel pushes racing on the same stack. Every layer the restack moved gets the re-review below, which reviews only that layer's own changes.

## Coordinator: freeze and split

For each assigned PR, fetch its title, body, head SHA, base SHA and changed files with added/removed counts. Read the linked issues, including `Refs #N`, and their Acceptance. Identify this layer's criteria and account for each. Save `just bot pr diff <N>` ONCE for that head. If the head changed while fetching, discard the snapshot and fetch a coherent one. Review remote context at the frozen SHA, or a checkout whose HEAD equals it.

List changed files and their +/- counts. Exclude `go.sum`, `flake.lock`, `**/testdata/**` and generated/binary files. List each exclusion and reason separately; records distinguish covered paths from exclusions. Tests not excluded stay with their implementation.

When the included files change Go code, run `just cover <base>` ONCE in the worktree at the frozen head, with the record's base: the PR's base SHA for a first record, the previous record's `head_sha` for a later one. Each reviewer gets the reported blocks in its files with its hunks and judges them by `REVIEW.md`; reviewers do not rerun it. Collect from each reviewer how many reported blocks fell inside its hunks and how it judged them, for the record's coverage line. Each later frozen head of a fix round that changes Go code gets one rerun with the same base.

Use omp's bundled `/review` sizing on INCLUDED files: let L be added plus removed lines and F the file count. No included files means no reviewer tasks and explicit exclusion evidence, not an invented review. Otherwise use one reviewer if L < 100 or F <= 2; min(2, F) for L < 500; min(4, ceil(F / 3)) for L < 2000; min(8, ceil(F / 2)) for L < 5000; and min(16, F) beyond that.

Group by locality: same directory/module, related functionality, tests beside implementation. Spawn ALL `agent: "reviewer"` tasks for the PR in ONE parallel task call. Each gets only its assigned files and their frozen hunks, the PR title/body, linked issue Acceptance and the root `REVIEW.md`. Reviewers must use those hunks, not rerun `git diff` or fetch a newer PR diff. They may read surrounding context at the frozen SHA. Collect findings, priorities, files covered and verdicts from every reviewer. Keep each reviewer's agent id and files: later rounds send it the changes to those files. Do not define a project agent named `reviewer`, which replaces the bundled reviewer whole.

## Coordinator: findings

Triage every agent finding by `AGENTS.md`. Fix real defects on the same branch in one push, with relevant proof. Reply with a reason to the rest. A deferred finding needs an issue and reason; it stays open if P0/P1. Reply to review threads through `just bot api repos/{owner}/{repo}/pulls/<N>/comments/{comment_id}/replies`; resolve them with the GraphQL `resolveReviewThread` mutation through `just bot api graphql`. Record each source, priority and outcome, including rejected or deferred findings.

After fixes, freeze the new head and fix diff once, with the same exclusions and file +/- listing. Send each fix hunk with `write agent://<id>` to the resumable reviewer that covered its file, all in parallel, telling it the new frozen SHA. Spawn reviewers, sized and grouped as above, only for files no earlier reviewer covered, or when the earlier reviewers are missing or terminal (including aborted agents still registered). Collect new findings. Repeat until every finding has an outcome, all threads are resolved and no P0/P1 is open. Recheck Acceptance. A finding repeated in two PRs adds a lesson to `REVIEW.md` in the second fix.

## Coordinator: record and check

Fetch the head again. If it changed outside reviewed fixes, review the new changes before proceeding. Prepare the record exactly as defined in `REVIEW.md`, with human text and JSON carrying the same data. A first record covers the latest reviewed SHA, all covered files, exclusions and all findings across review rounds; a later record covers only its delta. Post ONE record for that reviewed head with `just bot pr comment <N> --body-file <record-file>`; keep its returned URL. Reuse an existing record for the head rather than posting another.

Only when every finding has an outcome and no P0/P1 is open, prepare this payload with the reviewed SHA and record URL:

```json
{"name":"review","head_sha":"<reviewed full sha>","status":"completed","conclusion":"success","output":{"title":"Review recorded","summary":"Review record: <record comment URL>"}}
```

Post with `just bot api --method POST repos/{owner}/{repo}/check-runs --input <check-file>`. Verify the returned run names robotogi as its App, has the reviewed SHA and links the record. Fetch the current head once more; a changed head needs its own gate. Return one summary row per PR with record/check URLs. Remove coordinator-created clean worktrees after publishing; retain any worktree with unpublished fixes and report why.

## Coordinator: re-review a changed head

Keep the latest record's URL, `base_sha` and `head_sha`, and the PR's current base and head. A version-1 record has no `base_sha`: try to recover the base the PR had at that head from the record text or the PR's timeline; ordinary base-branch advancement does not preserve that tip in the timeline. When the historical base cannot be recovered, conservatively review this layer's entire current diff against its current base, including its relationship to the previous findings. Link the previous record and state that historical-base evidence was unavailable; do not carry forward or claim a minimal delta. Otherwise run `git range-diff <old-base>..<old-head> <new-base>..<new-head>` for this layer, not the whole stack.

If every patch pairs unchanged (`=`), with none added or removed, carry the record forward: post a new `review` check on the new head whose summary links the earlier record and names the old/new ranges and range-diff evidence. Post no comment and claim no fresh review.

Otherwise the delta includes every added commit, the new version of every patch range-diff marks changed (`!`), and the effects of removed patches, not just their names. Freeze these hunks once at the new head and list their files with the same exclusions. For removed patches, include the reversed old-patch hunks and trace their effect in the current layer against its current base, so inherited changes from lower stack layers are not treated as this layer's removals. Review it as fix diffs are reviewed above: resumable earlier reviewers get the hunks for their files; new reviewers only for uncovered files, or sized on the delta when the earlier ones are missing or terminal. Give new reviewers the latest record too. Triage and fix as above, then post an incremental record for the new head.

Check that current threads are resolved before success.

## Aggregate

Wait for all PR and stack coordinators. Report ONE table:

`PR | head | reviewers | P0–P3 findings | fixed | rejected | deferred | review check | blockers`

Show per-priority counts, disposition counts and check URLs. Include record links in each row's review-check cell. Report blockers rather than omitting a PR. Never merge.
