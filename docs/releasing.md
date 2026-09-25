# Releasing

The owner decides when to release. `version.txt` is the single source of the package version. shycler uses Semantic Versioning: before 1.0, a breaking change bumps MINOR and every other release bumps PATCH. From 1.0 onward, a breaking change bumps MAJOR, otherwise entries under `### Added` bump MINOR and other releases bump PATCH. A changelog line beginning `- **BREAKING**` makes the release breaking. If there are no `v*` tags and no changelog section for the current version, the first release uses the version already in `version.txt` without a bump.

A pull request marked breaking merges only after the current `[Unreleased]` changes have been released. This ensures every downgrade target has a version.

Set `FORGEJO_TOKEN` to a Forgejo token with repository write access. In a checkout with `remote.origin.url` set to the Forgejo repository:

1. Run `just release` to open the release pull request. The command reads the default branch through the Forgejo API, updates `version.txt` and `CHANGELOG.md` together on `release-<version>`, and opens the pull request. An already open release pull request is reported instead. Agents may review this pull request.
2. Review and merge the release pull request.
3. Run `just release` again. It tags the release pull request's merge commit (not the current default branch head), publishes the release, and uses that version's changelog section as the release notes, including its referenced pull request links.

The command is `go run ./tools/release`. Use `-dry-run` to see the due step without writing to Forgejo. `-repo owner/name` and `-url https://host` override the values derived from `remote.origin.url`; supply both to run without a checkout. The supported remote URL forms are `ssh://git@host/owner/name.git` and `https://host/owner/name` (with or without `.git`). `FORGEJO_TOKEN` is required even for a dry run.

Releases carry no binary artifacts. Flake users build from a tag. Dev builds report `x.y.z+rev` (or `x.y.z+rev-dirty` for dirty flakes); `go run` reports `x.y.z+dev`. `shycler --version` prints the build version and revision.
