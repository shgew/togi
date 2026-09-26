# Releasing

The owner decides when to release. `version.txt` is the single source of the package version. shycler uses Semantic Versioning: before 1.0, a breaking change bumps MINOR and every other release bumps PATCH. From 1.0 onward, a breaking change bumps MAJOR, otherwise entries under `### Added` bump MINOR and other releases bump PATCH. A changelog line beginning `- **BREAKING**` makes the release breaking. If there are no `v*` tags and no changelog section for the current version, the first release uses the version already in `version.txt` without a bump.

A pull request marked breaking merges only after the current `[Unreleased]` changes have been released. This ensures every downgrade target has a version.

Releasing needs permission to start workflows in the repository ([ADR 0014](adr/0014-release-from-a-workflow.md)).

1. Run `just release-preview` to see the version, the reason for the bump and the release notes the release would get from `origin/main`.
2. Run `just release`. It starts the `release` workflow on `main` with `gh workflow run` and follows the run until it ends. The workflow computes the next version from `[Unreleased]` and commits `Release x.y.z` on top of `main`, updating `version.txt` and `CHANGELOG.md` together; with nothing under `[Unreleased]` it fails at that step. It then runs `just check --race` and `just vuln` on that commit and pushes it to `main`; if `main` moved since the run started, the push is refused and the run fails, and running `just release` again starts over. Last, it creates the `v<version>` tag and the release on that commit, with that version's changelog section, including its pull request links, as the notes.

If a run pushed the release commit but failed to publish, run `just release` again: it finds the dated version without a tag and publishes it without a new commit.

Releases carry no binary artifacts. Flake users build from a tag. Dev builds report `x.y.z+rev` (or `x.y.z+rev-dirty` for dirty flakes); `go run` reports `x.y.z+dev`. `shycler --version` prints the build version and revision.
