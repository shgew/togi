# Coverage is a review signal, not a target

Agents add branches faster than anyone reads them, and review had no evidence of which changed code no test reaches. The common remedy, a mandated coverage percentage, rewards executing lines rather than checking them: agents meet it with tests that assert nothing, mocks that echo themselves, or code reshaped to reach paths that cannot occur. Coverage only weakly predicts how many faults a suite catches once suite size is controlled for ([Inozemtseva and Holmes, ICSE 2014](https://www.cs.ubc.ca/~rtholmes/papers/icse_2014_inozemtseva.pdf)). togi had no target and 83.4% of statements covered under `go test ./...` on Linux; the gaps sat mostly in code that talks to the machine and in the `main` programs under `tools/`. See [#292](https://github.com/shgew/togi/issues/292).

## Decision

Coverage of changed code is evidence for reviewers. Nothing gates on it: no threshold, ratchet or tracked overall percentage.

`just cover [base]` runs the tests the `package` check runs with `go test -coverprofile` and reads the profile with Go's own tools. With a base, it prints the blocks no test reached in Go files changed since the merge base; without one, `go tool cover -func` for the whole repository.

For a reviewed head that changes Go code, the `/review-pr` coordinator runs it once against the record's base and gives each reviewer the blocks in its files. The reviewer matches blocks to its hunks and raises a finding only when a consumer could see the uncovered code break. Defensive paths that cannot occur are not findings. The record states how many uncovered blocks fell inside the diff and how they were judged, so the owner can check that judgment.

## Considered Options

- **A mandated percentage, or a ratchet against `main`:** rejected. It turns the signal into a target that agents meet with tests that check nothing.
- **Metrics posted from CI:** rejected. The reviewers act on the evidence; a CI comment adds a second place to read it and more automated traffic on every pull request.
- **diff-cover over Cobertura or LCOV:** rejected. It needs a Python tool and a converter, gcov2lcov or gocover-cobertura, to filter Go's profile by a diff.
- **Mutation testing with gremlins:** rejected. It finds tests that run code without checking it, but adds a tool outside the Go toolchain; reviewers judge test strength.
- **A program in `tools/` that intersects `git diff -U0` with the profile:** deferred. It prints exact uncovered changed lines but is code to maintain. Revisit if reviewers mismatch blocks to hunks.

## Consequences

Review sees untested changed code without anyone writing tests to move a number. The output lists every uncovered block in a changed file, including blocks outside the diff, so reviewers do the matching, and the quality of the signal rests on their judgment. The record's JSON becomes version 3 to carry the coverage judgment. Code reached only by the VM checks, the `hardware` tests or the real machine shows as uncovered; reviewers judge it with that in mind.
