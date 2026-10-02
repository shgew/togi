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
