# Review record

The review coordinator writes this record for each reviewed head; `.omp/commands/review-pr.md` says when. Reviewers apply the criteria in [`REVIEW.md`](../REVIEW.md) and do not need this document.

The author posts one robotogi comment for each reviewed head commit, after fix commits have also been reviewed. A pull request's first record covers its whole diff. Each later record covers only what changed since the previous record, as `.omp/commands/review-pr.md` defines that delta, and links it; it does not repeat earlier files, reviewers or findings. A pull request's review is the chain of its records. Nothing per pull request is committed to the repository.

Exception: when a version-1 record's historical base cannot be recovered, the next record links it but covers the whole current layer conservatively, and states why a minimal delta could not be established. It may repeat files and reviewers for that coverage; prior findings are still inherited through the chain.

A record is written for a human first: the verdict leads, findings follow, and data kept for agents is collapsed. Write commit SHAs, `#N` references and URLs bare, never in code spans or code blocks, so GitHub links them. Use this order:

1. Verdict: `## Review record`, then one line with the verdict and the reviewed full commit SHA. The verdict is `success` only when every finding in the chain has an outcome and no P0/P1 remains open. Deferring a P0/P1 does not close it for this gate. Otherwise use `blocked`, name what blocks, and do not post a successful check.
2. Findings: with findings, a table `Source | Priority | Finding | Outcome`. Include every agent finding from this record's rounds, and any earlier finding whose outcome changed. Outcomes are `fixed in <full sha>`, `rejected: <reason>` or `deferred: #<issue number>`. With no findings, one sentence says so and there is no table.
3. Review data: one collapsed `<details>` block with `<summary>Review data</summary>`, holding the base SHA; for a later record, the previous record's URL and the reviewed range; files covered; excluded files with reasons; reviewers with names and agent models when available, the coordinator itself when it reviewed a small pull request; all limited to this record's diff. When the diff changes Go or Rust code, one coverage line: how many ranges `just cover` reported and how they were judged. The fallback's reason for covering the whole layer goes here too.
4. End with exactly one hidden block: `<!-- togi-review {json} -->`. Serialize compact JSON on one line; escape `<`, `>` and `&` as Unicode escapes so finding text cannot end the HTML comment. In the human Markdown, write `&`, `<` and `>` of finding, outcome, blocker, reviewer and judgment text as HTML entities, so text such as `<!-- x -->` shows as written, hides nothing and adds no second comment; the JSON carries it unchanged. A Markdown code span in that text (a backtick run closed by a run of exactly as many) keeps showing its content verbatim, written as `<code>` with every `&`, `<`, `>` and ASCII punctuation character as an entity, so `` `<nil>` `` still reads `<nil>` and no marker appears as raw bytes; an unmatched backtick run is literal text.

A first record with no findings:

```markdown
## Review record

**success** on <full head sha>.

No findings.

<details>
<summary>Review data</summary>

- Base: <full base sha>
- Files: `crates/togi-tuner/src/decide.rs`, `crates/togi-tuner/src/optimum.rs`
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
| reviewer-1 | P1 | Readback skipped on retry (`crates/togi-session/src/write.rs:42`) | deferred: #123 |
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
- `coverage`: `null` when this record's diff changes no Go or Rust code; otherwise an object with `uncovered` (integer: ranges `just cover` reported for this record's diff; records before [ADR 0032](adr/0032-coverage-lists-uncovered-changed-lines.md) counted blocks inside the diff) and `judgment` (string: how they were judged, naming any that became findings).
- `verdict`: `success` or `blocked`.

Human text and JSON carry the same data. Version 1 lacked `base_sha` and `previous`; read a version-1 record as a first record. Version 2 lacked `coverage`; read it as unknown. A successful check named `review` from robotogi points to this record on exactly `head_sha`. A pure rebase can carry it forward only after `git range-diff` proves the layer's own changes unchanged; the new head gets a new check linking the earlier record, not a new record claiming a fresh review.

A record longer than GitHub's comment limit, 65,536 characters, is posted as several robotogi comments, its parts, in order. Each part ends with a note line, `<sub>Review record, part <i> of <n>.</sub>`, and a hidden block `<!-- togi-review-part {"head_sha":"<full sha>","part":<i>,"parts":<n>,"data":"<slice>"} -->`. The text before the note, less the one newline that separates a non-empty slice from it, is the part's slice of the record's Markdown above its `togi-review` block: whole lines while they fit, a line cut only when it alone exceeds a part. `data` is the part's slice of that block's JSON. The record is the parts' Markdown slices joined in order, then `<!-- togi-review ` with the joined `data` slices and ` -->`, exactly as rendered. The record's URL is its first part's, and it counts as posted when its last part is; a split record missing a part is not a record. Readers join the parts before reading the record.

## Tooling

The `togi-tool-reviews` crate does the mechanical steps; the coordinator keeps the judgment: grouping files, triage, fixes and re-review reasoning. Run it as `just reviews <subcommand>`; with no subcommand it prints the track record of merged pull requests.

- `just reviews snapshot [--previous <record comment URL> [--previous-base <full lower-case 40- or 64-hex SHA>]] [--cover <file or ->] <PR>` freezes the pull request's head and base and writes `$TMPDIR/togi-review/<PR>-<short head>/`: `manifest.json`, `pr.diff` (the frozen diff, byte for byte) and `files/<index>.diff`, one hunk file per included file, named in the manifest. The diff comes from `gh pr diff`; when GitHub refuses it as over its 20,000-line limit, the snapshot builds the same merge-base diff itself, in a temporary bare repository made from the `origin` URL, with the complete history of the frozen base and head fetched into it whatever the checkout's depth, and `git diff -M <base>...<head>` run there. That repository ignores the caller's Git configuration, attributes and `GIT_*` environment, and no textconv, external diff or prefix setting applies, so the same frozen SHAs give the same hunks on any host. The temporary repository is removed on every exit, and a failure at any of those steps is an error that writes no snapshot. Any other `gh` failure is an error. It refuses when the head or base moved while it ran. The manifest holds the head and base SHAs, every changed file with its status and added and removed counts, the exclusions with reasons, L and F, the reviewer count by the sizing rule in `.omp/commands/review-pr.md` and whether the pull request is small by that command's rule. `--previous-base` needs `--previous` and gives the base the PR had at the previous record's head, recovered by a coordinator for a record that names no `base_sha` (version 1, or version 2 lacking it); a record naming a different base errors. It applies before the fallback, and without it an unrecoverable base stays conservative. With `--previous` it also fetches both ranges, runs `git range-diff` and writes `delta.diff`, the new version of every changed patch, every added patch and the reverse of every removed one, with each patch classified unchanged, changed, added or removed. A patch whose added and removed lines are unchanged and whose only difference is its context lines counts as unchanged. When every patch is unchanged the manifest says `carry_forward`. A previous record without a base gets the whole layer and a `fallback` reason instead of a delta. The hunk files of a delta hold the delta's patches, so line numbers of a patch that a later patch shifts can differ from the head's.
  GitHub's 300-file diff limit uses the same isolated Git fallback as its line limit.
- `just reviews snapshot --cover <file or -> <snapshot directory>` adds `just cover` output, concatenated, to an existing snapshot, which `snapshot --cover` also does when it creates one. The manifest keeps the uncovered ranges that lie inside the frozen hunks, the delta's on a re-review, and the not-built files among the covered ones, and records how many ranges it was given.
- `just reviews render <snapshot directory> <findings file>` writes `record.md` and `record.json` there from one model: the Markdown and the version 3 block carry the same data. The findings file is JSON, read strictly: it must hold exactly one object with known fields, and render refuses a second object or other data after it (trailing whitespace is fine), writing nothing:

  ```json
  {
    "reviewers": [{"name": "reviewer-1", "model": "<model or null>", "files": ["crates/togi-tuner/src/decide.rs"]}],
    "findings": [{"source": "reviewer-1", "priority": "P2", "finding": "…", "location": "crates/togi-tuner/src/decide.rs:42", "url": null,
                  "outcome": {"status": "fixed", "sha": "<full sha>"}}],
    "coverage_judgment": "How the uncovered ranges were judged; required when the diff changes Go or Rust code.",
    "blocked": "A blocker the findings do not show, such as an unresolved review thread."
  }
  ```

  The reviewers must cover every included file. The verdict is `blocked` when a P0 or P1 finding is deferred or `blocked` is set, and `success` otherwise.
- `just as-bot just reviews publish <snapshot directory>` posts `record.md` and, for a `success` verdict, the `review` check, both with one robotogi App token. A `record.md` over GitHub's comment limit is posted as its parts, as described above, and the check is posted only once every part is there. It refuses when the pull request's head is no longer the snapshot's, reuses a record or part already posted for the head only when robotogi's existing comment there has the same body and data as the one `record.md` makes, and refuses when it differs; a rerun after an interrupted publish posts only the missing parts. It verifies the returned check run is robotogi's on the reviewed SHA and links the record, and prints the record URL. For a `carry_forward` snapshot it posts only the check, linking the previous record; it refuses unless that previous record's verdict was `success`, and the snapshot carries forward automatically only from such a record: eligibility is the explicit `success` verdict, not the record's version, and a record whose verdict is not `success` never carries forward; an all-unchanged delta then gets a normal new record.
