# Heavy checks run on CI runners, not on the contributor's machine

Status: see [the index](README.md).

Amends [ADR 0074](0074-pull-request-ci-gates-landing-on-main.md) and [ADR 0085](0085-the-gate-publishes-and-pull-request-ci-runs-every-check.md)'s local-heavy-gate policy, and [ADR 0036](0036-flake-checks-stay-hermetic.md)'s claim that `just check` covers the full CI inventory; hermeticity stays in force. Keeps ADR 0085's all-check PR discovery, publication and cache trust. [ADR 0016](0016-every-pull-request-runs-every-check.md)'s rejected “VM only at release” remains rejected: VM checks still gate every PR in CI. Implements [#1328](https://github.com/shgew/togi/issues/1328).

## Context

A default local edit/check loop must be usable on a 16 GiB laptop. Whether a host has writable KVM says whether it can run VM tests, not whether every edit should pay for them. Hosted CI can retain the real-boot evidence without making a contributor's local gate rebuild it.

## Decision

A flake check is **CI tier** exactly when its derivation carries `passthru.tier = "ci"`, exposed as `checks.<system>.<name>.tier or null`. Otherwise it is local tier. `nix/vm-test.nix` marks every VM check with the attribute while preserving its `driver` and other passthru attributes.

`just gate`, `just push-fix` and `just check` do not request CI-tier checks on any host. The local nextest discovery also filters the tier, so additional CI-tier nextest checks require no justfile edit. Local checks must not depend on CI-tier checks. The KVM-dependent heavy-gate stage is removed, not retained as another selection policy.

`just vm [names]` explicitly builds all or selected VM checks on Linux with writable KVM. It discovers them by `driver` and builds the selected checks. Other heavy check families may add their own opt-in recipe and use the same tier attribute.

`just check` remains: it builds the full local flake inventory, including the package and cargo-deny's advisory audit, which the gate does not cover. It selects local checks instead of using unfiltered `nix flake check`.

PR CI and the release's exact-head dispatched check still select every flake check from `builtins.attrNames checks`. Their cache prefilter and failure gate are unchanged. The release's separate package/nextest build does not replace that full dispatch. A cached CI-tier result counts as passed just as a cached local result does.

## Safety-evidence ledger

This table records the evidence that leaves the default local tier and the named default witnesses that remain. They prove mechanisms, not all VM environments or every physical power-loss outcome. Later tier migrations append rows here and name every moved test; no test or assertion is deleted by this decision.

| Evidence moved to CI tier | Proof retained in CI | Named default witnesses that remain |
|---|---|---|
| `vm-grub`: `tuning-boot`, `restart-limit`, `night` | Real PID1 watchdog/reset, GRUB selection and enter/reset/leave lifecycles; topology-specific scope execution | `crates/togi-session/src/whole.rs::a_night_continues_across_a_crash_reboot` continues the same night after a write crash/reboot. It does not replace bootloader integration. |
| `vm-runtime`: `host-lock`, `trial-scope`, `emergency-stop` | Installed root/delegated lock setup, deployed SIGKILL hook ordering, real systemd scope teardown, zero/readback before restart | `crates/togi-hostlock/src/tests.rs::real_process_excludes_until_it_exits`; `crates/togi-trial/src/tests/teardown.rs::teardown_shares_one_fifteen_second_budget_across_instances`; `crates/togi-session/src/restore/tests.rs::without_the_proof_that_nothing_runs_nothing_is_written`; `crates/togi-session/src/emergency/tests.rs::{a_gated_emergency_zeroes_every_core_reads_each_back_and_reports,an_interruption_of_any_emergency_access_keeps_the_original_error_and_appends_nothing}`; `crates/togi/tests/emergency_stop/main.rs::a_stop_sweeps_then_zeroes_every_core_and_reads_each_back`. |
| `vm-runtime`: `day-observer` | Real peer credentials and service activation; journal reopen after VM reset | `crates/togi-journal/src/journal/tests.rs::an_event_is_one_write_then_one_fsync` pins the durability call order; `crates/togi-journal/tests/disk.rs::real_process_an_abrupt_exit_loses_no_acknowledged_event` retains acknowledged events across process death, not loss of the kernel page cache. |
| `vm-display`: `kmscon-display` | GPU/VT/OCR and installed display fallback | `crates/togi-watch/src/frames/frame_tests/bounds.rs::mapping::screens` and the other staged frame goldens/bounds remain local; they do not prove the real display integration. The safety mechanisms named in the other rows are unchanged. |
| `vm-efi`: `night-systemd-boot` | Real EFI variables, immutable attributes and systemd-boot enter/reset/leave lifecycle | The session crash/reboot witness above remains local; it does not prove EFI state restoration. |
| No SMU transaction or receipt test moves | VM deployed integration remains additional evidence, not authority for the transaction rules | `crates/togi-journal/src/journal/tests.rs::{a_receipt_follows_a_durable_intent_and_the_write_cites_it,a_receipt_is_refused_by_another_journal_and_by_a_reopened_one,a_receipt_from_before_a_reboot_is_refused}`; `crates/togi-session/src/write/tests.rs::a_readback_that_disagrees_is_durable_then_an_smu_dead_end`. |
| No reset-kind witness moves | Recovery sweeps exercise reset kinds as inputs; they are **not** credited with asserting reset-kind semantics | `crates/togi-simrun/tests/sim/crash.rs:43–135`: `a_power_button_reset_during_a_trial_is_a_conclusive_failure`, `a_thermal_trip_reset_during_a_trial_dead_ends_without_a_failure`, `a_thermal_trip_while_a_trial_runs_dead_ends`, `a_power_button_between_trials_is_inconclusive` assert the recorded reason, conclusiveness, failure presence/absence and dead-end behavior. |

## Consequences

A successful local gate is not a claim that every PR check has run locally. CI still refuses landing on a failed uncached VM check. Module and VM work can opt into VM builds before pushing. Publishing already-built CI-tier outputs remains useful and does not build them implicitly.

Moving tests between tiers does not bound allocations inside the remaining tests.

## Rejected

- KVM-dependent defaults: identical edits should not acquire heavy checks merely because the host can run them.
- Removing VMs from PR CI or running them only at release: that loses the exact-PR boot evidence.
- A hand-maintained CI-check skip list: the derivation owns its tier, so new heavy families use the same selection rule.
- Removing `just check`: the gate does not build its full local inventory.
