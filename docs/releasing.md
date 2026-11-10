# Releasing

Before 1.0, [AGENTS.md](../AGENTS.md#keeping-docs-current) owns delivery: the target desktop follows main, the owner chooses when to update, and there are no releases, new changelog fragments or breaking-change titles/labels. The history of [model.md](model.md) records behavior changes. Incompatible journal or decision changes still bump SCHEMA or RULESET, refuse old journals and say so in the pull request.

## Tooling still present until R23

The manual `.github/workflows/release.yml`, `just release`, `just release-preview` and `togi-tool-release` still exist; they are not steps to run before 1.0. `version.txt` still identifies builds. R23 removes the retired release/changelog machinery without changing safety evidence.

- `quality` still builds its `changes` member, which parses existing files in `changes/`. It does not compare a PR's diff to its fragments or require a new fragment; a documents-only PR, or any other pre-1.0 change, passes without adding one. Leave existing fragments valid until removal.
- The old release tool still assembles fragments into CHANGELOG.md and computes a semantic-version bump. Its formatting rules remain in [changes/README.md](../changes/README.md) solely for those existing files.
- If invoked, the workflow still refuses any open or closed issue labeled `needs-hardware`, dispatches/waits for every flake check on the exact main head, builds the package and quality, then pushes its version/changelog commit and publishes a tag and GitHub Release. Its configured direct-main bypass remains dormant; ordinary changes use PRs.
- These release-only gates do not block the owner's main update. [Hardware evidence](issues.md#work-that-waits-on-the-target-machine) stays visible, and each tuning result is read before designing the next tuning change.
- PR CI has no breaking-title/label gate. Dropping the ceremony does not waive compatibility refusal or exact-head merge gates.

## Historical publication mechanics

This section records what the remaining tools do, not a release instruction. Preview reads origin/main without changing the checkout. Publication uses the exact-head full check; a cached output counts as passed. A retry after a pushed release commit can publish the missing GitHub Release without another commit; a tag pointing elsewhere refuses. Releases carry no binary artifacts. [Development](development.md#ci-and-release-gates) owns CI and [the runtime spec](spec/runtime.md#commands) owns version output.
