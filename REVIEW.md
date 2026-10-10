# Pull request reviews

These criteria apply to every reviewer. Report concrete defects with their consequence, affected file and line, and priority: P0 blocks all use, P1 needs fixing before merge, P2 is a normal defect, P3 is a minor defect. Review the diff and enough surrounding code to trace its effects. Check the linked issue's `## Acceptance` against the evidence in the pull request.

The review coordinator writes each review's record in the format of [`docs/review-record.md`](docs/review-record.md).

## Defects

- **Offset writes:** hardware writes go only through `internal/smu`, clamp to [-50, 0], record intent before the write and readback after it. Failure must not claim an offset was applied or verified.
- **Pure decisions:** `internal/tuner` has no I/O, clock or randomness except through injected seams.
- **Traceability:** the journal lets a reader reconstruct actions and decisions. Replay preserves their meaning, validates evidence and handles interrupted or incomplete writes without inventing state.
- **Containment:** trace teardown, cancellation, timeout and startup failure paths, not just successful trials. Follow a second failure while handling the first. No workload escapes containment, survives teardown unnoticed or permits unsafe continuation. Sources: [#174](https://github.com/shgew/togi/pull/174), [#176](https://github.com/shgew/togi/pull/176), [#215](https://github.com/shgew/togi/pull/215), [#255](https://github.com/shgew/togi/pull/255).
- **Root file opens:** inspect every file opened as root under a workload-writable path, including data and logs. Check symlinks, FIFOs, ownership, permissions, umask and relative paths. A path check alone does not secure a later open. Source: [#204](https://github.com/shgew/togi/pull/204).
- **Hardware identity:** physical core IDs, logical CPUs, CCDs and SMU slots are validated and mapped explicitly. Enumeration order, contiguity and identity between different numbering systems are not evidence. Sources: [#69](https://github.com/shgew/togi/issues/69), [#256](https://github.com/shgew/togi/pull/256).
- **Concurrency and deadlines:** shared state is synchronized; cancellation reaches blocked work; deadlines bound the whole operation and its failure paths. Cleanup must not wait forever or race continued use.
- **Contracts:** specs describe current behavior, and the linked issue's Acceptance is met for this pull request's scope. Docs may land in the lowest layer of a stack ahead of their code. Flag statements that are wrong or contradict code or other docs, not ones this layer does not implement yet.
- **Breaking changes:** a value change to `journal.Schema` or `tuner.Ruleset` carries a `[BREAKING]` title, `breaking` label and `**BREAKING**` changelog line. Nothing else carries these markers.
- **Public content:** the diff, commit messages, pull request body and review record follow the Public rule in `AGENTS.md`. A breach is P1. Say what leaked and where without quoting it, since the review is public too.
- **Tests:** tests pin consumer-visible behavior, boundaries, invariants and transitions. A bug fix has a test that fails before the fix and passes after it, or states why that proof is impractical and provides a scoped reproduction. Evidence cited for a change must come from a check that exercises the changed package: [what each check proves](docs/benchmarking.md#what-each-check-proves).
- **Porting pull requests:** a pull request that ports Go to Rust is reviewed against the checklist in [`docs/porting.md`](docs/porting.md#checklist-for-reviewers). Weakening a comparison (a wider ignore list, a skipped session, a looser tolerance) is a P1.
- **Untested changed code:** each reviewer gets the changed lines `just cover` reports in its Go files: lines of the diff inside blocks no test reached. A reported range is a finding only when a consumer could see it break unnoticed: behavior, a boundary, an error the operator sees. Defensive paths that cannot occur are not findings, and neither is a missing test that would only reach a line. Code that only the VM checks, the `hardware` tests or the target machine reach shows as uncovered; judge it as such. `just cover` also names the changed files the reviewing host does not build, such as `_linux.go` files on macOS: no profile measured them, so judge their tests by reading them, and rely on CI for the platform that builds them.

Style, wording, naming taste and optional refactors are not findings. Handle findings by the rule in `AGENTS.md`.

## Lessons

When the same kind of finding occurs in two pull requests, add a lesson in the pull request that fixes the second. Cite the source pull requests or issues.

- **Net release behavior:** pending changelog fragments in `changes/` describe the behavior that will ship, not each intermediate commit. A later layer of the same release edits or deletes the fragment of a feature it supersedes or removes. Sources: [#269](https://github.com/shgew/togi/pull/269), [#285](https://github.com/shgew/togi/pull/285).
- **Hermetic package source:** package unit and integration tests run without the full checkout, its Git metadata or non-shipped documentation. Fixtures must own their inputs, including any temporary Git repository, or use files in the package source set rather than assuming checkout-only files exist. Check invariants about non-shipped documentation at repository review level. Sources: [#659](https://github.com/shgew/togi/pull/659), [#667](https://github.com/shgew/togi/pull/667).
