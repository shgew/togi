# Review record

The review coordinator writes this record for each reviewed head; `.omp/commands/review-pr.md` says when. Reviewers apply the criteria in [`REVIEW.md`](../REVIEW.md) and do not need this document.

The author posts one robotogi comment for each reviewed head commit, after fix commits have also been reviewed. A pull request's first record covers its whole diff. Each later record covers only what changed since the previous record, as `.omp/commands/review-pr.md` defines that delta, and links it; it does not repeat earlier files, reviewers or findings. A pull request's review is the chain of its records. Nothing per pull request is committed to the repository.

Exception: when a version-1 record's historical base cannot be recovered, the next record links it but covers the whole current layer conservatively, and states why a minimal delta could not be established. It may repeat files and reviewers for that coverage; prior findings are still inherited through the chain.

A record is written for a human first: the verdict leads, findings follow, and data kept for agents is collapsed. Write commit SHAs, `#N` references and URLs bare, never in code spans or code blocks, so GitHub links them. Use this order:

1. Verdict: `## Review record`, then one line with the verdict and the reviewed full commit SHA. The verdict is `success` only when every finding in the chain has an outcome and no P0/P1 remains open. Deferring a P0/P1 does not close it for this gate. Otherwise use `blocked`, name what blocks, and do not post a successful check.
2. Findings: with findings, a table `Source | Priority | Finding | Outcome`. Include every agent finding from this record's rounds, and any earlier finding whose outcome changed. Outcomes are `fixed in <full sha>`, `rejected: <reason>` or `deferred: #<issue number>`. With no findings, one sentence says so and there is no table.
3. Review data: one collapsed `<details>` block with `<summary>Review data</summary>`, holding the base SHA; for a later record, the previous record's URL and the reviewed range; files covered; excluded files with reasons; reviewers with names and agent models when available, the coordinator itself when it reviewed a small pull request; all limited to this record's diff. When the diff changes Go code, one coverage line: how many ranges `just cover` reported and how they were judged. The fallback's reason for covering the whole layer goes here too.
4. End with exactly one hidden block: `<!-- togi-review {json} -->`. Serialize compact JSON on one line; escape `<`, `>` and `&` as Unicode escapes so finding text cannot end the HTML comment.

A first record with no findings:

```markdown
## Review record

**success** on <full head sha>.

No findings.

<details>
<summary>Review data</summary>

- Base: <full base sha>
- Files: `internal/tuner/step.go`, `internal/tuner/step_test.go`
- Excluded: `go.sum` (generated)
- Reviewers: reviewer-1 (<model>): both files
- Coverage: 0 uncovered ranges.

</details>

<!-- togi-review {...} -->
```

A later record with findings:

```markdown
## Review record

**blocked** on <full head sha>: P1 open.

| Source | Priority | Finding | Outcome |
|---|---|---|---|
| reviewer-1 | P1 | Readback skipped on retry (`internal/smu/write.go:42`) | deferred: #123 |
| reviewer-2 | P3 | Stale comment | fixed in <full sha> |

<details>
<summary>Review data</summary>

- Base: <full base sha>
- Previous: <previous record URL>, range <old head sha>..<new head sha>
- Files, excluded files, reviewers and coverage as above.

</details>

<!-- togi-review {...} -->
```

JSON version 3 has these fields, all required:

- `version`: integer `3`.
- `repository`: `owner/repo`.
- `pull_request`: integer pull request number.
- `head_sha`: full reviewed commit SHA.
- `base_sha`: full SHA of the pull request's base when `head_sha` was reviewed.
- `previous`: `null` in a first record; otherwise an object with the previous record's `url` and `head_sha`.
- `excluded_files`: array of objects with `path` (repository-relative path) and `reason` (string). Empty when all changed files were covered.
- `files`: array of paths covered by this record.
- `reviewers`: array of objects with `name` (string), `model` (string or null) and `files` (covered paths).
- `findings`: this record's findings, as in the table: objects with `source` (reviewer name), `priority` (`P0` through `P3`), `finding` (description), `location` (path and line, or null), `url` (source comment URL or null), and `outcome` (object). An outcome has exactly one of these forms: `{"status":"fixed","sha":"<full sha>"}`, `{"status":"rejected","reason":"<reason>"}`, `{"status":"deferred","issue":123}`.
- `coverage`: `null` when this record's diff changes no Go code; otherwise an object with `uncovered` (integer: ranges `just cover` reported for this record's diff; records before [ADR 0032](adr/0032-coverage-lists-uncovered-changed-lines.md) counted blocks inside the diff) and `judgment` (string: how they were judged, naming any that became findings).
- `verdict`: `success` or `blocked`.

Human text and JSON carry the same data. Version 1 lacked `base_sha` and `previous`; read a version-1 record as a first record. Version 2 lacked `coverage`; read it as unknown. A successful check named `review` from robotogi points to this record on exactly `head_sha`. A pure rebase can carry it forward only after `git range-diff` proves the layer's own changes unchanged; the new head gets a new check linking the earlier record, not a new record claiming a fresh review.
