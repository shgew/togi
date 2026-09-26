# Releasing

The owner decides when to release. `version.txt` is the single source of the package version. shycler uses Semantic Versioning: before 1.0, a breaking change bumps MINOR and every other release bumps PATCH. From 1.0 onward, a breaking change bumps MAJOR, otherwise entries under `### Added` bump MINOR and other releases bump PATCH. A changelog line beginning `- **BREAKING**` makes the release breaking. If there are no `v*` tags and no changelog section for the current version, the first release uses the version already in `version.txt` without a bump.

A pull request marked breaking merges only after the current `[Unreleased]` changes have been released. This ensures every downgrade target has a version.

Releasing needs push access to the repository and nothing else: no token, no Forgejo client.

1. Run `just release`. It fetches `main`, computes the next version from `[Unreleased]`, and builds a `Release x.y.z` commit on top of `origin/main` that updates `version.txt` and `CHANGELOG.md` together, without touching your checkout. It pushes that commit to `refs/for/main` with the push option `topic=release`, which Forgejo turns into a pull request ([AGit workflow](https://forgejo.org/docs/latest/user/git-cli/agit-support/)): the commit subject is its title, the reason for the bump its description, and the push prints its link. `check` runs on it like on any pull request. Agents may review it. `just release -dry-run` prints the version, the reason and the release notes without pushing.
2. Review and merge the release pull request.
3. CI publishes. On a push to `main`, once `just check --race` and `just vuln` pass, the last step of the `check` job runs `go run ./tools/release -publish` with the workflow's automatic token; pull requests skip that step. When `version.txt` names a version with a dated changelog section and no `v<version>` tag, it creates the release on the first-parent commit that last changed `version.txt`, the release pull request's merge commit, with that version's changelog section, including its pull request links, as the notes. Forgejo creates the tag with the release. A failed publish is retried by the next push to `main` or by re-running the job.

Only one release pull request is open at a time. While one is open, Forgejo rejects the next `just release` push, and the command says so: merge or close the open one first.

Releases carry no binary artifacts. Flake users build from a tag. Dev builds report `x.y.z+rev` (or `x.y.z+rev-dirty` for dirty flakes); `go run` reports `x.y.z+dev`. `shycler --version` prints the build version and revision.
