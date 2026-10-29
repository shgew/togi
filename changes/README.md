# Pending changelog entries

Each pull request with a change an operator of the shipped `togi` binary would notice adds one file here, named by its pull request number: `changes/123.md`. Tool, simulator, bench, research, review and release tooling changes get no fragment; neither do behavior-preserving refactors, tests or docs. Open the pull request first, then add the file. The [release workflow](../docs/releasing.md) assembles every file into a dated section of [`CHANGELOG.md`](../CHANGELOG.md) and deletes them in the release commit.

```markdown
### Fixed

- `togi status` no longer reports a finished core as running.
```

- Headings: `### Added`, `### Changed`, `### Removed` or `### Fixed`, each followed by at least one entry.
- Entries: one line each, starting with `- ` and ending with a period. Leave out pull request URLs (inline links or bare URLs) and `[#123]` references; the release adds ` ([#123])` before the period. Other links, such as documentation and issues, are allowed; write relative links from `changes/`, as in `../docs/howto.md`, and the release rewrites them for `CHANGELOG.md`.
- A breaking entry starts with `- **BREAKING**`, then what the operator must do or will see. Any breaking entry makes the release bump the minor version before 1.0.
- A pull request that changes something not yet released edits that change's file instead of adding its own.

`just changes` checks these rules, as does the changelog-fragment member of the `quality` flake check in CI. `just release-preview` shows the release section these files make.

## Rationale and rejected alternatives

Per-PR fragments avoid conflicts over shared changelog lines and reference definitions while keeping operator-visible entries in the reviewed diff. Naming by PR number prevents collisions and lets the release generate the link correctly. Whether a change needs a fragment remains a review judgment, not a mechanical check.

Rejected:

- `merge=union` for `CHANGELOG.md`: GitHub ignores merge drivers when deciding mergeability, and local rebases can silently reorder or duplicate entries.
- Inline PR links alone: they remove definition conflicts, not shared-entry conflicts.
- Collecting entries from PR bodies: entries leave the reviewed diff and release depends on the GitHub API and merge style.
- A generated `[Unreleased]` section committed by a recipe or CI: shared lines return, or a bot must commit to `main`.
- Branch-slug fragments with handwritten links: numbers avoid link mistakes and collisions.
- towncrier or changie: another dependency adds no needed capability beyond the existing release tool.
