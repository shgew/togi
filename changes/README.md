# Pending changelog entries

Each pull request with a change a user of togi would notice adds one file here, named by its pull request number: `changes/123.md`. Open the pull request first, then add the file. The release assembles every file into a dated section of [`CHANGELOG.md`](../CHANGELOG.md) and deletes them in the release commit ([ADR 0033](../docs/adr/0033-changelog-fragments.md)).

```markdown
### Fixed

- `togi status` no longer reports a finished core as running.
```

- Headings: `### Added`, `### Changed`, `### Removed` or `### Fixed`, each followed by at least one entry.
- Entries: one line each, starting with `- ` and ending with a period. Leave out the pull request link; the release adds ` ([#123])` before the period.
- A breaking entry starts with `- **BREAKING**`, then what the operator must do or will see. Any breaking entry makes the release bump the minor version before 1.0.
- A pull request that changes something not yet released edits that change's file instead of adding its own.

`just changes` checks these rules, as does the `changes` flake check in CI. `just release-preview` shows the release section these files make.
