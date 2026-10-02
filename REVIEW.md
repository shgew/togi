# Pull request reviews

These criteria apply to every reviewer. Report concrete defects with their consequence, affected file and line, and priority: P0 blocks all use, P1 needs fixing before merge, P2 is a normal defect, P3 is a minor defect. Review the diff and enough surrounding code to trace its effects. Check the linked issue's `## Acceptance` against the evidence in the pull request.

## Defects

- **Offset writes:** hardware writes go only through `internal/smu`, clamp to [-50, 0], record intent before the write and readback after it. Failure must not claim an offset was applied or verified.
- **Pure decisions:** `internal/tuner` has no I/O, clock or randomness except through injected seams.
- **Traceability:** the journal lets a reader reconstruct actions and decisions. Replay preserves their meaning, validates evidence and handles interrupted or incomplete writes without inventing state.
- **Containment:** trace teardown, cancellation, timeout and startup failure paths, not just successful trials. Follow a second failure while handling the first. No workload escapes containment, survives teardown unnoticed or permits unsafe continuation.
- **Root file opens:** inspect every file opened as root under a workload-writable path. Check symlinks, FIFOs, permissions, umask and relative paths. A path check alone does not secure a later open.
- **Hardware identity:** core IDs, CCDs and hardware slots are validated and mapped explicitly. Enumeration order, contiguity and identity between different numbering systems are not evidence.
- **Concurrency and deadlines:** shared state is synchronized; cancellation reaches blocked work; deadlines bound the whole operation and its failure paths. Cleanup must not wait forever or race continued use.
- **Contracts:** specs describe current behavior, and the linked issue's Acceptance is met for this pull request's scope. Docs may land in the lowest layer of a stack ahead of their code. Flag statements that are wrong or contradict code or other docs, not ones this layer does not implement yet.
- **Breaking changes:** a value change to `journal.Schema` or `tuner.Ruleset` carries a `[BREAKING]` title, `breaking` label and `**BREAKING**` changelog line. Nothing else carries these markers.
- **Tests:** tests pin consumer-visible behavior, boundaries, invariants and transitions. A bug fix has a test that fails before the fix and passes after it, or states why that proof is impractical and provides a scoped reproduction.

Style, wording, naming taste and optional refactors are not findings. Handle findings by the rule in `AGENTS.md`.

## Lessons

When the same kind of finding occurs in two pull requests, add a lesson in the pull request that fixes the second. Cite the source pull requests or issues.

- **Unhappy-path containment:** follow every exit from workload startup through teardown, including a cleanup failure after another failure. Successful stop tests do not prove failed startup, timeout or cancellation contains the workload. Sources: [#174](https://github.com/shgew/togi/pull/174), [#176](https://github.com/shgew/togi/pull/176), [#215](https://github.com/shgew/togi/pull/215), [#255](https://github.com/shgew/togi/pull/255).
- **Root opens workload-writable files:** a workload can replace an expected regular file with a symlink or FIFO before root opens it. Check the open itself, ownership and modes; trace both data and log paths. Source: [#204](https://github.com/shgew/togi/pull/204).
- **Unvalidated core-to-slot mapping:** logical cores and SMU slots are separate identities. Reject unsupported topology rather than writing to an assumed slot. Sources: [#69](https://github.com/shgew/togi/issues/69), [#256](https://github.com/shgew/togi/pull/256).
- **Net release behavior:** Unreleased notes describe the behavior that will ship, not each intermediate commit. Remove notes for features superseded or removed by a later layer of the same release. Sources: [#269](https://github.com/shgew/togi/pull/269), [#285](https://github.com/shgew/togi/pull/285).

## Review record

The author posts one robotogi comment for each reviewed head commit, after fix commits have also been reviewed. A pull request's first record covers its whole diff. Each later record covers only what changed since the previous record, as `.omp/commands/review-pr.md` defines that delta, and links it; it does not repeat earlier files, reviewers or findings. A pull request's review is the chain of its records. Nothing per pull request is committed to the repository. Use this order:

Exception: when a version-1 record's historical base cannot be recovered, the next record links it but covers the whole current layer conservatively, and states why a minimal delta could not be established. It may repeat files and reviewers for that coverage; prior findings are still inherited through the chain.

1. Header: `## Review record`, reviewed full commit SHA, base SHA, and for a later record the previous record's URL and the reviewed range. Then files covered, excluded files with reasons and reviewers (names and agent models when available), all limited to this record's diff.
2. Table: `Source | Priority | Finding | Outcome`. Include every agent finding from this record's rounds, and any earlier finding whose outcome changed. Outcomes are `fixed in <full sha>`, `rejected: <reason>` or `deferred: #<issue number>`. With no findings, leave the table empty and say so outside it.
3. Verdict: `success` only when every finding in the chain has an outcome and no P0/P1 remains open. Deferring a P0/P1 does not close it for this gate. Otherwise use `blocked` and do not post a successful check.
4. End with exactly one hidden block: `<!-- togi-review {json} -->`. Serialize compact JSON on one line; escape `<`, `>` and `&` as Unicode escapes so finding text cannot end the HTML comment.

JSON version 2 has these fields, all required:

- `version`: integer `2`.
- `repository`: `owner/repo`.
- `pull_request`: integer pull request number.
- `head_sha`: full reviewed commit SHA.
- `base_sha`: full SHA of the pull request's base when `head_sha` was reviewed.
- `previous`: `null` in a first record; otherwise an object with the previous record's `url` and `head_sha`.
- `excluded_files`: array of objects with `path` (repository-relative path) and `reason` (string). Empty when all changed files were covered.
- `files`: array of paths covered by this record.
- `reviewers`: array of objects with `name` (string), `model` (string or null) and `files` (covered paths).
- `findings`: this record's findings, as in the table: objects with `source` (reviewer name), `priority` (`P0` through `P3`), `finding` (description), `location` (path and line, or null), `url` (source comment URL or null), and `outcome` (object). An outcome has exactly one of these forms: `{"status":"fixed","sha":"<full sha>"}`, `{"status":"rejected","reason":"<reason>"}`, `{"status":"deferred","issue":123}`.
- `verdict`: `success` or `blocked`.

Human text and JSON carry the same data. Version 1 lacked `base_sha` and `previous`; read a version-1 record as a first record. A successful check named `review` from robotogi points to this record on exactly `head_sha`. A pure rebase can carry it forward only after `git range-diff` proves the layer's own changes unchanged; the new head gets a new check linking the earlier record, not a new record claiming a fresh review.
