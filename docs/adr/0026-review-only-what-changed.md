# Review only what changed

[ADR 0025](0025-recorded-agent-review.md) records one review per reviewed head, and every record listed all covered files, reviewers and findings. When a lower stack layer changed, each layer above got a new coordinator, new reviewers and a record restating the whole pull request. On [#275](https://github.com/shgew/togi/pull/275), a restack changed context in two files; the second record repeated all 20 files and both earlier reviewers to add one reviewer for those two files. [#276](https://github.com/shgew/togi/pull/276) got two records of similar length. The repetition hides the change a reader needs to see, and fresh agents read again context the earlier agents already held.

## Decision

A pull request's first record covers its whole diff. After that, only changes are reviewed: fix commits, later pushes and the layer's own patches that a restack changed, found with `git range-diff` against the previous record's base and head. A pure rebase still carries the record forward with a check and no comment.

Changes go back to the agents that reviewed them. `/review-pr` sends a changed pull request or stack to the coordinator that reviewed it in the same session, with `write agent://<id>`, instead of spawning another. The coordinator sends each changed hunk to the reviewer that covered its file and spawns reviewers only for files none covered, or when the earlier reviewers are gone, such as in a new session; new reviewers also get the previous record.

Each later record covers only its delta: the reviewed range, its files, reviewers and findings, and earlier findings whose outcome changed. It links the previous record, so a pull request's review is the chain of its records. JSON version 2 adds `base_sha` and `previous` so tools can follow the chain; a version-1 record reads as a first record. The `review` check on each head links that head's record, and success still requires every finding in the chain to have an outcome and no open P0/P1.

## Considered Options

- **Restate the whole review on every head:** rejected. It is what #275 and #276 show: long records that bury the change.
- **Edit the previous record in place:** rejected. The check on the earlier head links that record, which must keep describing that head.
- **Spawn fresh reviewers for each delta:** rejected while the earlier ones exist. They hold the surrounding code and their own findings; a fresh agent reloads both.

## Consequences

A reader sees what changed in each review round without diffing records. Coverage of a pull request is the union of its chain, so reporting tools follow `previous` rather than read one comment. Reusing agents works only within the session that spawned them; a later session reviews the delta with fresh reviewers and the previous record. A reused reviewer may carry an earlier misreading into the next round; the delta is still frozen and checked against the current head.
