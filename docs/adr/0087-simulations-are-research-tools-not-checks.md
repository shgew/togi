# Simulations are research tools, not checks

Status: see [the index](README.md).

Supersedes the suite-as-check portions of [ADR 0016](0016-every-pull-request-runs-every-check.md), [ADR 0074](0074-pull-request-ci-gates-landing-on-main.md) and [ADR 0085](0085-the-gate-publishes-and-pull-request-ci-runs-every-check.md), and [#627](https://github.com/shgew/togi/issues/627)'s shard-sizing rule; amends [ADR 0047](0047-gate-before-every-push.md)'s gate contents and [ADR 0051](0051-light-checks-for-review-fixes.md)'s fix-push checks. Decided by the owner on 2026-10-28 ([#1321](https://github.com/shgew/togi/issues/1321); refs [#1265](https://github.com/shgew/togi/issues/1265)).

## Context

The simulator session suite, `tools/bench/suite.json`, runs 220 long simulated sessions across the bench's machines and seeds. Under ADRs 0016, 0074 and 0085 it was also a correctness check: the flake checks `sim-suite-0` to `sim-suite-<N-1>`, with N declared in `nix/sim-suite.json`, ran in `just gate`, `just push-fix`, `just check`, pull request CI and the release's dispatched full check, and #627's rule raised N whenever a shard's job became the slowest of a full-check run.

The suite answers research questions: how the tuner behaves across machines and seeds, and whether a change leaves those simulated decisions unchanged. The properties its checks asserted about each session are application behavior: the session concludes the way its scenario expects, its warm resume state equals a cold fold of its journal, the tuner's replay accepts the journal, and the invariant audit finds no violation. Small in-process tests prove that behavior directly; running every long session in every gate and CI run adds substantial work beyond them.

## Decision

**Simulations run on demand for research and development. Ordinary nextest tests are the correctness checks.**

- No flake check runs the simulator suite. The `sim-suite-*` checks, `nix/sim-suite.json` and the build plumbing only those checks used are removed. `just gate`, `just push-fix`, `just check`, pull request CI and the release's dispatched full check do not run the research suite; their ordinary tests may use short in-memory simulations.
- The research tools stay: `just sim-suite` (the whole suite in one shard, then its manifest check), `togi-tool-sim suite` and `togi-tool-sim check`, `just same`, `just smoke` and `just bench`. A contributor runs them when a change raises the question they answer, and cites their result as evidence for that question only.
- Each property the suite's checks asserted is proved by an ordinary nextest test of the crate that owns it; a property that lacks one gets a short in-memory test.
- The gate's Linux heavy stage builds only the VM checks, and is skipped when `/dev/kvm` is not writable; `just push-fix` does the same.
- #627's rule for sizing the suite's shards is retired: there are no shard jobs to size.

## Considered Options

- **Keep the suite as sharded checks:** rejected. Every gate and CI run keeps paying for long simulations whose correctness properties ordinary tests prove.
- **Run the suite only in the release's full check:** rejected. A release would still wait on a research run, and a regression the suite found would surface only at release, after the change landed.
- **Keep a small subset, such as the `smoke` sessions, as a check:** rejected. It is still long simulation in the gate, and the properties belong in in-process tests of the crates that own them.

## Consequences

- A behavior change that only simulated sessions reach is found when someone runs the suite, `just same` or the bench, not by the gate or CI. A tuner or session change that alters simulated decisions calls for one of them before review.
- The gate, `just push-fix`, pull request CI and the release's full check no longer build a release `togi-tool-sim`; `release-deps` holds only `togi`'s locked release dependencies, which the `package` check starts from.
- Off Linux, and on Linux without writable `/dev/kvm`, the gate's heavy stage builds nothing; pull request CI builds the VM checks it does not find cached, as under ADR 0085.
