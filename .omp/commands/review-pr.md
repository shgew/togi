---
description: Review an authored pull request and record its merge gate
argument-hint: <PR number>
---

Review pull request $1 in this repository as its author. Read `AGENTS.md` and `REVIEW.md` first. Use `just bot` for GitHub actions. Keep temporary payloads outside the repository. Do not merge.

## Freeze the target

Fetch the pull request's head SHA, base SHA, body, changed files and diff (`just bot pr view $1 --json headRefOid,baseRefOid,body,files` and `just bot pr diff $1`). Read the linked issues, including references such as `Refs #N` that are not closing references, and their Acceptance. Identify which criteria this layer implements; account for each. Save the head and base SHAs and the frozen diff. Review remote content at that head, or a checkout whose HEAD equals it, not an unrelated local working tree.

## Get independent reviews

Spawn omp's bundled `reviewer` agents with the task tool. Give each the frozen head SHA, assigned files and diff, relevant Acceptance and the repository root's `REVIEW.md`. Keep related directories together, tests beside their implementation, and cover every changed file. Do not define a project agent named `reviewer`, which would replace the bundled reviewer whole.

Use the bundled `/review` sizing: let L be added plus removed lines and F the file count. Use one reviewer if L < 100 or F <= 2; otherwise use min(2, F) for L < 500, min(4, ceil(F / 3)) for L < 2000, min(8, ceil(F / 2)) for L < 5000, and min(16, F) beyond that. Group by locality rather than slicing the patch into equal line counts. Collect findings, priorities, files covered and verdicts from every reviewer.

While CodeRabbit is installed, wait for its completed review of this head. Fetch all pages of pull request reviews, inline comments, issue comments and review threads, including review bodies and collapsed outside-diff findings. Match CodeRabbit's review `commit_id` to the frozen head; an old review or an in-progress summary does not count. If it explicitly reports a rate limit or pause for this head, save that comment URL and status instead. If it is not installed, record evidence of its absence. Missing output alone is not proof of absence, a pause or a rate limit. Do not silently time out and certify the pull request.

## Handle every finding

Triage every agent finding and every CodeRabbit inline or review-body finding by `AGENTS.md`. Fix real defects on the same branch in one push, with the relevant proof. Reply with a reason to the rest. A deferred finding needs an issue and a reason; it stays open if P0/P1. Reply to CodeRabbit threads through `just bot api repos/{owner}/{repo}/pulls/$1/comments/{comment_id}/replies`; resolve them with the GraphQL `resolveReviewThread` mutation through `just bot api graphql`. Reply to review-body findings in a pull request comment identifying each finding. Record each source, priority and outcome, including rejected or deferred findings.

Review every fix commit with independent reviewer agents. After a push, wait for CodeRabbit on the new head under the same rule, and collect any new findings. Repeat until every finding has an outcome, all threads are resolved and no P0/P1 is open. Recheck the issue's Acceptance. If a finding of the same kind has appeared in two pull requests, add its lesson to `REVIEW.md` in the second fix.

## Record and check

Fetch the head again. If it changed outside the reviewed fixes, review the new changes before proceeding. Use the latest reviewed SHA, all files covered and all findings across the review rounds. Prepare the record exactly as defined in `REVIEW.md`, with human text and version-1 JSON carrying the same data. Post ONE record comment for that reviewed head with `just bot pr comment $1 --body-file <record-file>`; keep its returned URL. If a record already exists for this head, reuse it rather than posting another.

Only when every finding has an outcome and no P0/P1 is open, prepare this JSON payload, substituting the reviewed SHA and record URL:

```json
{"name":"review","head_sha":"<reviewed full sha>","status":"completed","conclusion":"success","output":{"title":"Review recorded","summary":"Review record: <record comment URL>"}}
```

Post it with `just bot api --method POST repos/{owner}/{repo}/check-runs --input <check-file>`. Verify the returned run names robotogi as its App, has the reviewed SHA and links the record. Fetch the current head once more; a changed head needs its own gate. Report the record URL, check URL, CodeRabbit status and any blocker.

## Rebase carry-forward

For a rebase, keep the earlier record, old base/head and new base/head. Run `git range-diff <old-base>..<old-head> <new-base>..<new-head>` for this layer, not the whole stack. Carry forward only if every patch pairs unchanged (`=`), with none added, removed or modified, and no unreviewed commits. Otherwise review the changes above. For unchanged patches, post a new `review` check on the new head; its summary links the earlier record and names the old/new ranges and range-diff evidence. Do not post another record or claim a fresh review. Check that current review threads are resolved before posting success.
