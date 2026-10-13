# Flake checks stay hermetic

Status: see [the index](README.md).

Go changes make local flake checks rebuild from a cold Go cache. Sharing the host's cache would save work, but CI pushes each check's output to the public Cachix cache `togi` ([ADR 0021](0021-cache-check-outputs-on-cachix.md)). A substituted check is meaningful only if it was built from its declared inputs. See [#359](https://github.com/shgew/togi/issues/359).

## Decision

Flake checks stay hermetic. No host Go build cache (`GOCACHE`), module cache or other host state reaches the build sandbox: no `extra-sandbox-paths`, `__noChroot`, impure derivations or `--impure` caches. A check that passed is reused only when its inputs are unchanged.

`just check` stays exactly CI's definition of green. A faster recipe gets its own name and a narrower documented claim.

## Considered Options

- **Host `GOCACHE` in the sandbox:** rejected. Results depend on host state, breaking hermeticity and trust in the public cache.
- **A separate impure "fast check" flake output counted as green:** rejected. It cannot support the same claim as CI's hermetic checks.

## Consequences

A Go change that affects a check's inputs rebuilds it from a cold Go cache. Local speed comes instead from recipes such as `just gate`, which run from the dev shell with warm caches and claim less than `just check`, and from check sources narrowed to what each check needs, so a change reuses every check whose source it does not touch.

## Amendment (#918)

The mechanism after the Rust cutover; the decision and its reasons are unchanged. For the Rust checks the host state kept out of the sandbox is the `target` directory and the `CARGO_HOME` registry and git caches, not `GOCACHE`. The checks build from `Cargo.lock` with crane in the sandbox, and a dependency build is reused only as a derivation whose inputs are unchanged, shared by the checks that name it. A change to the workspace's own crates rebuilds them in each check that compiles them. Local speed comes from `just typecheck`, `just focus` and `just gate` in the dev shell with a warm `target/`, which claim less than `just check`. The `go-tools` check, which builds the four retained Go tools, still starts from a cold Go cache.
