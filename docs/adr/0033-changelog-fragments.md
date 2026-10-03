# Changelog entries are per-pull-request fragments

Every pull request with a user-visible change added a line to `[Unreleased]` in `CHANGELOG.md` and a `[#N]:` link definition at the bottom of the file. Pull requests open at the same time appended to the same lines, so whichever merged second conflicted, even when the code did not. Rebases that resolved the conflict by hand sometimes dropped definitions, and [#205](https://github.com/shgew/togi/pull/205) restored three of them. [ADR 0014](0014-release-from-a-workflow.md) has the release read `[Unreleased]`, so the entries have to reach `main` somewhere in the tree.

## Decision

Each pull request with a user-visible change adds one file, `changes/<N>.md`, where `<N>` is its pull request number, after the pull request is opened. The file holds Keep a Changelog `###` headings, limited to Added, Changed, Removed and Fixed, each followed by one-line `- ` entries ending with a period. Entries carry no pull request link. A breaking entry still starts with `- **BREAKING**`. A pull request that changes something not yet released edits that change's file rather than adding a fragment of its own. `changes/README.md` states these rules and is the only other file in the directory.

The release workflow assembles the fragments on `origin/main` into the new dated section of `CHANGELOG.md`. Sections follow the order Added, Changed, Removed, Fixed. Within a section, breaking entries come first, then the rest by ascending pull request number. The release inserts ` ([#N])` before each entry's final period, adds the missing `[#N]:` definitions in numeric order, and deletes the fragments in the release commit. The bump is computed from the assembled entries by the same rules as before. With no fragments the release refuses, or resumes publishing a released version that has no tag, as it did for an empty `[Unreleased]`.

`CHANGELOG.md` holds only released versions. The `changes` flake check runs `go run ./tools/release -check changes` on a source of `go.mod`, `go.sum`, `tools/release` and `changes/`, and fails on a misnamed file, an unknown section, an empty section or file, an entry without its final period, or an entry that links a pull request. Whether a pull request needs a fragment stays a review judgment, because refactors, tests and documentation need none.

This supersedes ADR 0014 only where it reads entries from `[Unreleased]`. The workflow still takes no inputs, the changelog remains the single source of the bump, and the release commit still changes only release files.

## Considered Options

- **`merge=union` for `CHANGELOG.md` in `.gitattributes`:** rejected. GitHub ignores merge drivers when it decides whether a pull request merges cleanly ([community discussion #9288](https://github.com/orgs/community/discussions/9288)), so pull requests still show as conflicting; local rebases could reorder or duplicate lines without anyone noticing.
- **Inline pull request links instead of reference definitions:** rejected alone. It removes the definition conflicts but not the entry conflicts.
- **Entries in the pull request body, collected from merged pull requests at release:** rejected. The entry leaves the reviewed diff, and releasing would depend on the GitHub API and on the merge style.
- **A generated `[Unreleased]` section kept current by a recipe or CI:** rejected. It puts the shared lines back, or makes a bot commit to `main`.
- **Fragments named by branch slug with the link written in the entry:** rejected. Naming by pull request number makes the link impossible to forget or mistype, and two fragments cannot collide.
- **towncrier or changie:** rejected. They add a dependency for what about a hundred lines of Go in the existing release tool do.

## Consequences

Pull requests no longer conflict over the changelog. A fragment can only be added after its pull request has a number, so opening a pull request takes one more push. Pending changes are read from `changes/` or with `just release-preview` rather than from the top of `CHANGELOG.md`. The release commit also deletes the consumed fragments; [ADR 0016](0016-every-pull-request-runs-every-check.md)'s reason for not checking the release commit again still holds, because no package or check other than `changes` reads those files. Each fragment file change reruns only the `changes` check.
