# Coverage lists uncovered changed lines

[ADR 0030](0030-coverage-is-a-review-signal.md) made `just cover BASE` print every uncovered block in a Go file changed since the base, including blocks outside the diff, and left reviewers to match blocks to their hunks. A change touching a large file buried its few new gaps among the file's old ones. ADR 0030 deferred a program that intersects the diff with the profile until reviewers mismatched blocks to hunks; the owner asked for that output before any mismatch was recorded. See [#292](https://github.com/shgew/togi/issues/292).

## Decision

`just cover BASE` runs `tools/cover` on the profile. It reads the new-side line ranges from `git diff -U0` against the merge base and prints only the changed lines inside blocks no test reached, merged into `path:first-last` ranges. A block listed more than once in the profile counts as reached if any listing reached it. With no changed Go lines, or none uncovered, it says so. Without a base the recipe still prints `go tool cover -func`.

Every printed range lies inside the diff. The `/review-pr` coordinator hands each reviewer the ranges in its files, and the review record counts ranges rather than blocks inside the diff, in the same version-3 `coverage` field.

## Considered Options

- **Keep whole-file blocks and reviewer matching:** rejected. Reviewers spend their attention discarding old gaps, and the record's count depends on how carefully each did it.
- **Print whole uncovered blocks that touch the diff:** rejected. A long uncovered block with one changed line would show its unchanged lines as part of the change.
- **diff-cover or other tools outside the Go toolchain:** still rejected for ADR 0030's reasons; the program uses only the standard library and git.

## Consequences

Review sees only gaps the change introduced or touched; an unchanged untested line beside the change no longer shows. The program is code to maintain, tested on fixture diffs and profiles. Lines of a file the profile does not measure, such as `hardware`-tagged code or files built only on another OS, print nothing; reviewers still judge those by reading them.

## Amendment (#918)

The mechanism after the Rust cutover; the decision and its reasons are unchanged. `tools/cover` reads the new-side ranges of `git diff -U0` against the merge base for `*.rs` files and intersects them with a cargo-llvm-cov LCOV trace, where a line listed more than once counts as reached if any listing reached it. With no changed Rust lines it prints `No Rust lines changed since BASE.`; with none unreached, `Every changed Rust line the trace measures ran in a test since BASE.` The Consequences' last sentence no longer holds for Rust: a changed file the coverage build leaves out, or a changed item under a `cfg` predicate the host does not satisfy or cannot decide whose lines the trace does not measure, is printed as `path: not built for <host triple>` instead of nothing. Reviewers still judge those by reading them. The Go profile route remains for the retained Go tools.

## Amendment (#633, #1038)

The program is the `togi-tool-cover` crate, which keeps the LCOV route's flags, output and refusals and exits 1 on a refusal; it differs only in that `-h` exits 0 and a failed `git diff` names git's message. The decision and its reasons are unchanged. The Go profile route is gone with the last Go code.
