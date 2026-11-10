---
description: Review pull requests by risk and record their exact-head merge gates
argument-hint: "[PR numbers or URLs… | all]"
---

Review `$ARGUMENTS`. Read `AGENTS.md`, `REVIEW.md` and, for coordinators, `docs/review-record.md`. Keep payloads outside the repository. Coordinators publish evidence; the main session orders merges.

## GitHub identity

Use the owner's account through `gh` for authorship, comments, threads, issues, pushes and merges. Only review records and the `review` check use robotogi through `just bot` or `just as-bot`. GitHub approval is not the gate: the same account authors the pull request, and the App certifies its exact head.

## Review trigger

Start review on a frozen local commit while its checks run, rather than waiting for CI. Save `git diff <base>...<head>` outside the repository and give readers that diff, the full local head/base SHAs and a checkout at that head. After the PR opens, verify its snapshot has that same head and diff before recording the reading. A moved head requires review of its changes. The main session starts review for subagents' PRs and follows `.omp/rules/review-after-open.md` if review has not already begun.

Classify by the effect of the change, not its directory:

- **Safety:** changing a safety rule or its enforcement gets full independent review, including documentation: hardware writes, offsets, journal intent/readback, crash recovery, zeroing, watchdog, containment and teardown. Use the full sizing below wherever the changed files live.
- **Other code:** one independent reader, who may be the coordinator if not the author.
- **Documents:** CI only is the approved rule. **Temporary exception until R21:** current `render` requires reviewers for every included Markdown file. Keep the existing independent review and sizing below; publish a normal record. A reviewer-free "documents only" record is not supported, so do not invent a reader or exclude hand-written documents to pass it.

## Resolve and fan out

Accept space- or comma-separated PR numbers or URLs; `all` or no argument means every open non-draft PR. Fetch every page, deduplicate and refuse other repositories. Report selected numbers, titles, heads and bases; an empty set finishes with an empty table.

Run `just reviews status`; `docs/review-record.md` owns its next-action states. Reuse a resumable coordinator for a PR or stack; otherwise dispatch independent PRs together, one `review-coordinator` per PR and one for all layers of a stack. Tell it the linked Acceptance, worktree and frozen local evidence. Each works only in its PR branch's worktree, never the main checkout. Give writing agents absolute paths inside that worktree and have them skip checks; the coordinator checks once after fixes land.

## Coordinator: freeze and split

Read the PR body and any linked issue's Acceptance; a direct request may have no issue. Account for this layer's scope. Declare its class with `just reviews snapshot --class <safety|code|documents> <N>` after pushing the frozen local commit; omission conservatively selects safety, and unknown values are refused. Classification is the coordinator's judgment of the diff's effects, not an automatic path or text heuristic. The manifest records the class, head/base, the byte-for-byte diff and hunk files. Refuse mismatched local evidence. Context reads use the frozen SHA. Current remote snapshots, previous-record recovery and multipart publication remain until R21; [the record guide](../../docs/review-record.md#tooling) owns their command details and exclusions.

The manifest selects one reader for other code. For full safety review and the temporary document-review exception, it uses included added-plus-removed lines L and file count F: no included files means no reviewer and explicit exclusion evidence; otherwise one reader when L < 100 or F <= 2, two when L < 500, max(2, min(4, ceil(F / 3))) when L < 5000, and min(8, ceil(F / 2)) beyond. Keep tests beside implementation, group by locality/invariant and dispatch all reader seats together.

Coverage is optional evidence a reader can request. **Until R21**, the renderer still requires coverage for each Rust review/fix delta: run `just cover <base> [CRATE ...]` once alongside readers, then import stdout with `just reviews snapshot --cover <file> <snapshot dir>`. Use the PR base for a first record, the previous record's head for later records. Recompute named packages owning changed tool Rust and consumers whose tests prove library behavior; product packages remain selected. Keep diagnostics on stderr. Readers judge uncovered changed ranges, not-built paths and assertion quality before their verdict; an empty report proves neither excluded behavior nor assertion strength. Record package selection and limitations honestly. See [coverage](../../REVIEW.md#coverage).

### Coordinator review

For other code, an independent coordinator may be the one reader. For full safety review, use separate reader seats even on a small diff. For temporary document reviews, the coordinator may read a small PR itself: at most two files and fewer than 100 changed lines over the whole PR, exclusions counted; otherwise use the full sizing. The manifest's `small` flag respects the declared risk class: safety is always false, wherever the changed files live.

Readers use the saved frozen hunks and surrounding context, not a newly fetched diff, and apply `REVIEW.md`. Collect concrete findings, priorities, covered files and verdicts; keep reader identities for fixes. If required independent seats are unavailable, report the blocker rather than substituting the author or pretending a reading occurred.

## Coordinator: findings

Triage by `AGENTS.md`. Fix behavior, safety, security, journal, concurrency, Acceptance, inadequate behavior tests and contradictory-doc defects on the same branch; explain rejected taste, optional refactors/tests and out-of-layer work. Deferred work needs an issue and reason; an open P0/P1 blocks success. Reply to and resolve all review threads.

Save `git diff <previous frozen head> HEAD` before a fix push so a rebase does not import main's work into the fix diff. Use `just push-fix` for code until R22 unifies pushes; document fixes use the quality-only path in [Development](../../docs/development.md#shell-and-checks). Run checks once after all edits, freeze again and review fixes by the same risk class. Reuse readers for their files, adding readers only for uncovered scope. Reclassify if a fix changes safety.

## Coordinator: record and check

Fetch the head again; account for every change before success. Render a truthful findings file and record with `just reviews render <snapshot dir> <findings file>` under [the record format](../../docs/review-record.md). It names readers, files, exclusions, all findings and outcomes, coverage when temporarily required, and blockers. Check resolved threads and no unrelated tracked edits in the main checkout before publication.

Publish with `just as-bot just reviews publish <snapshot dir>`. It checks the unchanged PR head and posts robotogi's record and, on success, the exact-head `review` check. An interrupted multipart publish can be retried; a moved head needs new evidence. Until R21 this check is required even for documents and cannot be replaced by CI or a human-authored check.

## Coordinator: re-review a changed head

Run `just reviews snapshot --class <safety|code|documents> --previous <latest record URL> <N>`, declaring the cumulative fix delta's risk class. Keep that same previous URL on every fix snapshot in the round so the delta stays cumulative. The tool compares old/new patch ranges with `git range-diff`; position/context-only changes count as unchanged. Use its conservative full-layer fallback when the old base cannot be recovered, as [the record guide](../../docs/review-record.md) specifies.

Only an explicitly successful prior record can carry forward. If every patch is unchanged with none added/removed and new merge commits have empty remerge diffs, publish the carry-forward snapshot: a new exact-head check links the old record without claiming fresh reading. Otherwise review changed/added patches, removed effects and nonempty merge-resolution diffs by risk before a new record. Keep old records immutable and link the chain. Resolve current threads before success.

## Aggregate

Report `PR | head | reviewers | P0–P3 findings | fixed | rejected | deferred | review check | blockers`, with record/check links and blockers rather than omitted PRs.

## Main session: merge

The main session alone orders merges. After a successful record/check, read the PR's next action with `just reviews status` or `just land N` under [Workflow](../../AGENTS.md#workflow). `land` waits only for `check` and never merges. Require CI `check`, the class's review (including the temporary universal robotogi gate) on the exact head, resolved threads and an up-to-date branch.

If behind, ask the coordinator to update with `just push-fix` for code or the documentation path, then carry evidence forward or review changed patches. Otherwise leave a review's base frozen unless GitHub reports a conflict. Recheck readiness immediately before `gh pr merge N --merge`; use [stack mechanics](../../docs/development.md#pull-request-stacks) for stacks, checking each layer as lower layers land. If main moves again, repeat the update and exact-head handoff. The owner does not read code or authorize each merge.
