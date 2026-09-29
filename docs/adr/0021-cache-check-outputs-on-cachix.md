# Cache check outputs on Cachix

A `check` run took about 3.3 minutes, set by the `vm` job. Much of it repeated work: the run on `main` after a squash-merge rebuilt the tree its pull request had just passed, and the release waits for that run ([ADR 0014](0014-release-from-a-workflow.md), [ADR 0016](0016-every-pull-request-runs-every-check.md)). A pull request touching only docs rebuilt every check, although no check's source includes the docs. The nixpkgs paths already come from cache.nixos.org, so a cache saves only what togi's flake builds, and it saved none of it while `packages.default` stamped the commit's revision and `lint` and `vm` were built from it: all three changed on every commit.

## Decision

- The checks build a package whose revision is `dev`; `lint` and `vm` build on it. `packages.default` keeps the commit's revision, so the journal still records which build ran, and the release workflow still builds and tests it on the release commit.
- The `check` workflow's build jobs substitute from the public Cachix cache `togi`. After a check builds, the job pushes that check's output, with its closure, and nothing else. It pushes only when `CACHIX_AUTH_TOKEN` is available: runs on `main` and on pull requests from branches in this repository push, Dependabot runs only read.
- The cache serves CI only: nothing configures it for users, the dev shell or local `just check`.

## Considered Options

- **The action's default push of every built path:** rejected because for `vm` it includes the NixOS system and its disk image, about 1 GiB unpacked per run, which fills the free 5 GB within a few runs. Those paths never produce a hit: an unchanged check is substituted from its output alone, and a change that rebuilds the package rebuilds the system, the disk image and the test driver with it.
- **Magic Nix Cache, or a Nix store cache per check (`nix-community/cache-nix-action`):** rejected because both store in the GitHub Actions cache, where a run reads only caches written on its own ref or the default branch, so the run on `main` after a merge could not read what its pull request stored. ADR 0016 also measured the store cache as no faster.
- **Dropping the revision stamp:** rejected because builds between releases, which a flake following `main` installs, would become indistinguishable in the journal.
- **Serving users prebuilt packages:** rejected because a hit needs the user's togi to use togi's own nixpkgs pin instead of following theirs. That duplicates the package's runtime next to the system's (glibc, tzdata, libidn2 and libunistring: about 38 MiB of a 43.7 MiB closure) and adds a second nixpkgs to fetch and evaluate on every rebuild, to save a build that takes about 12 seconds with its tests.
- **Configuring the cache in the flake's `nixConfig`:** rejected because it prompts anyone running nix commands on the flake, and local runs of `just check` are rare enough to rebuild.

## Consequences

- A check whose inputs are unchanged passes without running: the run on `main` after a merge draws no new `-shuffle=on` seed and boots no VM. Flaky tests surface in local `just test` and `just test -race`, and in later pull requests that rebuild the check.
- A pull request run holding the token could push any output under a check's store path, which `main` would then take as a pass. Only collaborators can open pull requests.
- The `check` workflow needs the `CACHIX_AUTH_TOKEN` repository secret to push; without it, runs still pass and only read the cache.
- The `package` check no longer builds the exact derivation that ships. The release commit's build runs the same tests on it.
