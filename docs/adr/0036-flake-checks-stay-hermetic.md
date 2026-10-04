# Flake checks stay hermetic

Status: Accepted.

Go changes make local flake checks rebuild from a cold Go cache. Sharing the host's cache would save work, but CI pushes each check's output to the public Cachix cache `togi` ([ADR 0021](0021-cache-check-outputs-on-cachix.md)). A substituted check is meaningful only if it was built from its declared inputs. See [#359](https://github.com/shgew/togi/issues/359).

## Decision

Flake checks stay hermetic. No host Go build cache (`GOCACHE`), module cache or other host state reaches the build sandbox: no `extra-sandbox-paths`, `__noChroot`, impure derivations or `--impure` caches. A check that passed is reused only when its inputs are unchanged.

`just check` stays exactly CI's definition of green. A faster recipe gets its own name and a narrower documented claim.

## Considered Options

- **Host `GOCACHE` in the sandbox:** rejected. Results depend on host state, breaking hermeticity and trust in the public cache.
- **A separate impure "fast check" flake output counted as green:** rejected. It cannot support the same claim as CI's hermetic checks.

## Consequences

A Go change that affects a check's inputs rebuilds it from a cold Go cache. Local speed comes instead from `just gate`, which runs every non-VM check from the dev shell with warm caches, and narrower check sources: the VM tests take only shipped source, so changes to tests, tools or the simulator reuse their cached result.
