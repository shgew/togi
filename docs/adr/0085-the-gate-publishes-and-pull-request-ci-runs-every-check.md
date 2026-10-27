# The gate publishes and pull request CI runs every check

Status: see [the index](README.md).

Supersedes [ADR 0074](0074-pull-request-ci-gates-landing-on-main.md)'s exclusion of VM tests and simulator shards from pull request CI; amends [ADR 0021](0021-cache-check-outputs-on-cachix.md) (the gate also pushes to the cache, and a result published by the owner's gate counts as passed in CI), [ADR 0047](0047-gate-before-every-push.md)'s gate contents and [ADR 0051](0051-light-checks-for-review-fixes.md)'s fix-push checks ([#1269](https://github.com/shgew/togi/issues/1269)).

## Context

Under ADR 0074 the simulator shards and VM checks never run in pull request CI, so CI does not run what the local gate runs, and a regression in them from an ungated pull request first surfaces at release. Running them there as they are would double runner time and bring back the queueing ADR 0074 removed: modelled at the busiest observed hour, 574 s median and 863 s p90 to green and 35.9 runner-minutes per pull request, against 353 s, 571 s and 17.2 runner-minutes with ADR 0074.

The gate already builds identical derivations. Of 154 check identities, 154 matched between the author's head and CI's merge commit across five recent pull requests. A result the gate built is therefore the result CI would build, and CI's Cachix prefilter ([ADR 0021](0021-cache-check-outputs-on-cachix.md)) already treats an output in the `togi` cache as a pass without a job.

## Decision

**The flake's checks are the one list of checks. Pull request CI selects all of them behind the Cachix prefilter, and the gate publishes what it builds.**

- `check.yml` selects every `checks.x86_64-linux` attribute in pull request mode as in dispatch mode, so a new check, including a new simulator shard or VM test, is run without a list edit. Evaluation starts a build only for outputs absent from the `togi` cache. Uncached cheap checks (`fmt`, `changes`, `deny`, `shear`, `module`, `board-test`, `sql-test`, `doc-links`, `package`) still share one `light` job, a packing choice; every other uncached check gets its own job, so a new check defaults to its own job. The release's exact-head dispatch is unchanged.
- `just gate` and `just push-fix` end, after the Nix builds of the checks and before any push, with a private `_publish` recipe. On x86_64 Linux it pushes with `cachix push togi` the output of every flake check of the tree that is in the local store, so what the gate built or substituted, `cargo-deps` and `release-deps` among them when built, reaches the cache. `clippy` and `doc` keep running through Cargo in the gate; CI builds them itself unless the gate had them in the store.
- The push needs a token on the host: `CACHIX_AUTH_TOKEN` in the environment, or the `authToken` that `cachix authtoken` stored in cachix's own config (`${XDG_CONFIG_HOME:-~/.config}/cachix/cachix.dhall`). The recipe tests for presence without reading the token into output. Without a token the gate still runs every check and prints one line saying nothing was published. A failed push prints a line and fails nothing: the gate's checks passed, and CI builds what it does not find.
- A published result built by the owner's gate counts as passed in CI: a cache hit is the same pass whether CI or the gate built it. ADR 0021's trust model is unchanged: whoever holds a push token can publish any output under a check's store path, and only the owner's host and the repository secret hold one.

## Considered Options

- **Keep ADR 0074 and add nothing:** rejected. CI keeps running less than the gate, and an ungated pull request's VM and simulator regressions wait for the release.
- **Run the heavy checks in CI and publish nothing:** rejected. It is the doubled runner time modelled above; the gate has built the same derivations already.
- **Publish from CI only:** rejected. CI's push happens after it builds, so it cannot spare the build the gate has already done.
- **A hand-kept list of pull request checks:** rejected. The flake's checks are one list; a second one would drift, as ADR 0074 observed for a second input map.
- **Select by changed paths:** rejected for the reason ADR 0074 gives: Nix already tracks each check's inputs exactly.

## Consequences

- A pull request whose head the gate built and published has no nextest group, simulator shard or VM check to build in CI: the eval step reports them cached. A pull request the gate did not publish, such as Dependabot's, builds them, up to about twenty jobs at once, at the free plan's concurrency limit, and takes longer to green than ADR 0074's six jobs.
- The gate depends on a token for its publish step; without one, CI does the building the gate could have spared.
- An owner's gate run on a different head than the merged one publishes outputs no one asks for; the cache holds them harmlessly within the free tier's 5 GiB until Cachix evicts them.
- Time to green and runner-minutes are measured after merge on one gated and one ungated pull request, against the modelled figures above.
