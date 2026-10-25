# How to tune a machine with togi

For a Zen 5 (Granite Ridge) desktop running NixOS with GRUB. Every command below runs on that machine.

## 1. Install the module

Add togi to your flake's inputs, pinned to a release, and pass the inputs to the host's configuration:

```nix
# flake.nix
{
  inputs.togi = {
    url = "github:shgew/togi/v0.5.0";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { nixpkgs, ... }@inputs: {
    nixosConfigurations.example = nixpkgs.lib.nixosSystem {
      specialArgs = { inherit inputs; };
      modules = [ ./configuration.nix ];
    };
  };
}
```

Keep your existing `nixpkgs` input and host name. `v0.5.0` is an example; the [releases page](https://github.com/shgew/togi/releases) lists the current tags. `specialArgs` is what gives the host's configuration its `inputs` argument; a flake that already passes its inputs another way keeps doing that.

Then import the module in the host's configuration:

```nix
# configuration.nix, the host's NixOS configuration
{ inputs, ... }:
{
  imports = [ inputs.togi.nixosModules.default ];
  services.togi = {
    enable = true;
    tuning.enable = true;
    backends.mprime.enable = true;
    backends.ycruncher.enable = true;
  };
}
```

mprime and y-cruncher are unfree: allow them with `nixpkgs.config.allowUnfree = true`, or with an `allowUnfreePredicate` naming `mprime` and `y-cruncher`.

The module loads `ryzen_smu` through `hardware.cpu.amd.ryzen-smu.enable`. A host that already loads its own `ryzen_smu` build sets `hardware.cpu.amd.ryzen-smu.enable = false`.

Other settings go in `services.togi.settings`, rendered as JSON to `/etc/togi/config.json`; [runtime.md](spec/runtime.md#configuration) lists the keys and shows a hand-written example.

When upgrading from TOML configuration, module users rebuild to generate the JSON file. If you maintain a hand-written configuration, convert its contents to JSON and save it as `/etc/togi/config.json`, or update your explicit `--config` path. Keep the same snake_case keys and values, turn tables into nested objects, and write core IDs as quoted decimal object keys. JSON comments are not accepted. This file-format change does not require a journal reset ([Configuration](spec/runtime.md#configuration)).

The module also creates the unprivileged `togi-trial` system user and group and sets `"backend_user": "togi-trial"` in the configuration. Togi itself still runs as root; mprime and y-cruncher use that account, which can write only their own trial instance directory within togi's state ([Backends](spec/workloads.md#backends)). Rebuild after upgrading to this behavior so the account exists before the next run. A hand-written configuration must set `backend_user` to an existing unprivileged account; [Privileges](spec/runtime.md#privileges) and [Preflight](spec/runtime.md#preflight) say what is refused.

To move to a newer release, change the tag in `url`, run `nix flake update togi` and rebuild. An update whose changelog line starts with **BREAKING** starts a seeded session on the next run ([section 7](#7-after-a-breaking-update)).

The module installs `inputs.togi.packages.${pkgs.stdenv.hostPlatform.system}.default`, the `togi` built with crane. Moving a host from the earlier Go build of togi takes the steps in [Go to Rust cutover](#go-to-rust-cutover). From a checkout, `nix build .#default` builds the package, `just togi ARGS` runs the binary from the tree and `just check-one package` checks its version and completion scripts.

mprime and y-cruncher come from the host's `nixpkgs`. Updating one of them, for example with `nix flake update nixpkgs`, can cost that backend's passes: [Evidence](spec/tuner.md#evidence) says when a pass still counts. Update the backends when the extra trials are worth it, not as routine.

### Host lock and delegated hardware tests

`run`, `reset` and hardware tests share one [host lock](spec/runtime.md#host-lock); a busy lock exits 3. Do not delete `/run/lock/togi.lock` to unblock it: replacing an inode while another process holds it would allow two owners.

The module provisions the lock during boot, before user logins. On an upgrade from the public-readable lock, stop all tuning and hardware-test processes and close their lock descriptors, or rebuild and reboot, to complete the security migration: changing permissions cannot revoke descriptors opened earlier. A refused lock (foreign-owned, symlink, special or multiply linked) is not deleted or repaired blindly; on Linux, rebooting into the rebuilt normal system recreates `/run` and provisions the trusted lock before users can create one. [Host lock](spec/runtime.md#host-lock) states the rules.

Delegated hardware tests need an explicitly authorized dedicated group, as well as their existing SMU/SMN permissions and delegated cpuset controller. Do not give this access to the unprivileged backend account. For example:

```nix
users.groups.togi-hardware = { };
users.users.operator.extraGroups = [ "togi-hardware" ];
services.togi.hardwareTestGroup = "togi-hardware";
```

Rebuild, then reboot to provision the lock for that group; log in again for group membership to take effect. A rebuild alone does not change an existing lock ([Host lock](spec/runtime.md#host-lock)). Run `just hardware` sequentially as before; ordinary users without this group cannot open the lock, even if they can read togi's state.

For a manual Linux install, protect the lock's parent from unprivileged writes, or provision it during trusted early boot before users can precreate its name in a sticky shared directory. After quiescing all lock users, the descriptor-based program [`nix/provision-lock.py`](../nix/provision-lock.py) provisions or migrates a single root-owned regular inode without following a lock symlink. The example requires an existing `togi-hardware` group; replace the final argument with `-` for root-only mode 0600. An existing foreign or multiply linked inode is refused without changing it, and a held lock makes the program fail rather than replacing it.

The program creates the lock only under a parent that [Host lock](spec/runtime.md#host-lock) accepts. Run it from a checkout of the togi revision you install:

```sh
nix-shell -p python3 --run 'sudo "$(command -v python3)" nix/provision-lock.py /run/lock/togi.lock togi-hardware'
```

On Darwin, only copied-journal `reset` uses a private lock; [Host lock](spec/runtime.md#host-lock) says who may reuse it. A different operator cannot rely on the old public-readable file; use the owning operator or explicitly provision root-owned group access after quiescing all old descriptors. Hardware tuning remains Linux-only.

## 2. Rebuild, then boot fresh

Rebuild (`nixos-rebuild boot`, or your usual way), then reboot. In BIOS, set every Curve Optimizer offset to 0. This is a recommendation, not a check: [preflight](spec/runtime.md#preflight) only requires that every core's offset reads back, and a nonzero offset only produces a notice ([Session start](spec/tuner.md#session-start)). Do not run another tool that writes Curve Optimizer offsets in the boot you tune from.

How togi treats the BIOS values:

- It reads every core's offset from the SMU once, at the first `run` of a session, and keeps that baseline for the whole session. Search normally starts there; configuration or carried candidates can supply another start ([Session start](spec/tuner.md#session-start)). A BIOS profile that crashes before togi runs surfaces as a boot-loop dead end ([ADR 0006](adr/0006-start-from-bios-values.md)).
- Every write sets the core's absolute offset; the SMU does not add it to the BIOS value. Firmware restores BIOS values on reboot; togi's trial writes replace those offsets.
- The BIOS context that ties a session to a BIOS (BIOS version, board, CPU model, microcode and boost limit) holds no Curve Optimizer offsets. Changing a Curve Optimizer offset in BIOS does not start a new session, and togi keeps the baseline it captured.
- A clean stop uses the session's original baseline for restoration, not later BIOS values. The safety rules can restore a shallower offset instead ([Exit restoration](spec/runtime.md#exit-codes)). It does not capture the baseline again.

Then check that the machine is ready, without starting a session:

```sh
sudo togi doctor
```

It makes the checks `sudo togi run` makes before tuning ([Preflight](spec/runtime.md#preflight)) and prints one row per check: `ok`, `FAIL`, `warn` or `skipped`. The last line is `ready`, or `not ready:` with the failed checks; fix those first. A `warn` on `watchdog` means no hardware watchdog is active in this boot, which the tuning boot provides. A `warn` on `bios_context` means the BIOS context changed since the session started, so the next run starts a new session ([section 7](#7-after-a-breaking-update)). `doctor` appends nothing to the journal and changes no offset; [Commands](spec/runtime.md#commands) describes what it does without `sudo`.

## 3. A quick in-session test

Start sessions from the [tuning boot](#4-overnight-the-tuning-boot), especially after a breaking update: it has hardware-watchdog reset protection. An in-session run is an optional way to watch a few trials on the desktop, not the recommended way to start a session.

```sh
sudo togi run
```

It shows the session as the `togi watch` dashboard, redrawn whenever the journal changes: what togi is doing and why, the running trial and what each outcome would lead to, the stages and a table of offsets per CCD. `togi watch` in another terminal shows the same screen and takes keys: `?` explains it. Watch a few trials, then press Ctrl-C: togi restores offsets and exits 0, and the next run resumes ([Exit codes](spec/runtime.md#exit-codes)). `sudo togi run --no-tui` prints one line per event instead.

```sh
togi status
togi events --kind trial
```

If the machine freezes during an in-session trial, it needs a manual reset unless an active hardware watchdog provides reset protection. `sudo togi run` warns when none is active and continues; it does not arm one ([Preflight](spec/runtime.md#preflight)). The warning stays in `togi events --kind session.warning`. After a reset or crash reboot, the next run attributes the crash from the journal and continues ([Crashes](spec/tuner.md#crashes)).

## Daily observation

After a session exists, `sudo togi day` observes a normal boot until Ctrl-C or a stop signal. It never applies Curve Optimizer offsets or starts test workloads. It reads the BIOS context and every core's offsets at start, re-reads offsets every ten minutes, and records checkpointed active hours excluding suspend. An orderly shutdown, including a short power-button shutdown handled by systemd, writes a final checkpoint followed by a clean-stop marker. A later start records a previous observed boot without that marker as an unclean end, with its last checkpoint, offsets, reset reason and the evidence it could read: machine checks of both boots, and graphics resets, ring timeouts, oopses and hung tasks in the crashed boot's kernel log tail, with whether logging went on after the last error. Unavailable evidence stays unknown, never zero.

`togi status` shows the Day summary; `togi events --kind day` shows the raw records. Hours are checkpointed lower bounds, not a stability guarantee. An interval whose two readbacks differ cannot identify when offsets changed, so it remains in boot hours but earns no hours for either profile.

### Answering what happened

An unclean end alone does not say whether the CPU caused it: graphics undervolts and power cuts crash a machine too. So each unclean end becomes a question, and `togi status` and `togi watch` show it, with what togi saw, until you answer. The four answers:

1. `frozen`: it froze.
2. `graphics`: graphics died but sound went on.
3. `power-off`: I turned it off while it was working normally.
4. `unsure`: not sure.

```sh
togi day answer <boot> frozen
```

`<boot>` is the boot id `status` prints. Answering needs no root shell, but it needs the day service running and your user allowed on its socket: name your user in `services.togi.day.users` (below). Run the same command again to change an answer; the last one counts. The answer is journaled against the crashed boot and its offset profile. It does not change any profile or tuning decision, and until you answer, togi's own reading of the evidence is shown but equally not acted on.

### Recording a crash togi did not observe

A freeze from before the day service existed has no unclean end to answer. `sudo togi day record` journals it, so Ruleset 13 can treat the offsets it ran as a whole-profile veto; the [journal spec](spec/journal.md#day-observation) has the rules, and the command prints what the record will do before it appends. It writes the journal itself, so stop the day service first (`systemctl stop togi-day.service`) and start it afterward.

For a boot the observer saw end uncleanly, name it with `--boot <boot>`; the offsets are the ones the observer read back, unless you pass `--offset` for the ones you remember entering:

```sh
sudo togi day record --boot <boot> --running game
```

For a crash with no recorded end, give when it happened with `--unobserved-at` and the offsets with `--offset`, plus when you entered them in firmware setup with `--entered-at`. Each `--offset` is `all=N`, `ccdI=N` or `coreI=N`; a more specific one overrides a less specific one, and every core needs a value. To record the 2026-10-19 freezes:

```sh
# the freeze at -25 and -30 by CCD (check which CCD took which value)
sudo togi day record --unobserved-at "<crash time>" --offset ccd0=-25 --offset ccd1=-30 \
  --entered-at "<time you entered the offsets>" --running build

# the freeze at the confirmed profile, which differs per core: one --offset coreI=N per core, N from your profile
sudo togi day record --unobserved-at "<crash time>" --offset core0=N --offset core1=N --offset core2=N \
  --entered-at "<time you entered the offsets>" --running desktop  # ...and so on for every core

# the freeze at a uniform -20
sudo togi day record --unobserved-at "<crash time>" --offset all=-20 \
  --entered-at "<time you entered the offsets>" --running build
```

Times are RFC 3339 or local (`2026-10-19 14:30`). `--running` is `idle`, `desktop`, `build`, `game`, `other` or `unknown` (the default). At a terminal, `togi day record` asks for whatever the options leave out, with defaults: the newest unclean end without a record, and its read-back offsets. The prompts take the same `all=N` form as the options and journal the same record; `--yes` skips the final confirmation.

`togi day notify` sends a desktop notification for each pending question using the installed `notify-send` (from libnotify; togi adds no notification library). On KDE Plasma (`XDG_CURRENT_DESKTOP` containing `KDE`) the notification has the four answers as buttons and waits up to two minutes for one. On other desktops it is a plain notification naming the command, because togi does not query what a notification server supports; answer with `togi day answer`. Dismissing a notification answers nothing, and the question stays in `status` and `watch`. The module's `togi-day-notify` user service runs it for each user in `services.togi.day.users`; elsewhere run `togi day notify` in your graphical session yourself.

Daily observation requires an existing compatible session journal. It refuses to create a session or transition one written by another ruleset. Start a session through the tuning boot first. Stop daily observation before `togi run` or `togi reset` in a normal boot: they share the host and writer locks. `togi status`, `togi events` and `togi watch` remain lock-free while observation runs.

To run it automatically in normal boots, enable the NixOS module option after creating a session:

```nix
services.togi = {
  enable = true;
  day.enable = true;
  day.users = [ "alice" ]; # may answer day questions without root; replace with your user
};
```

The `togi-day.service` unit is absent from the tuning specialisation. Enabling it keeps journald persistent with a five-second sync interval. Before a manual `run` or `reset` in a normal boot, use `systemctl stop togi-day.service`; start it again with `systemctl start togi-day.service` afterward. Inspect service diagnostics with `journalctl -u togi-day.service`. Enabling it without a session reports why it cannot start instead of creating a tuning session.

The optional `services.togi.day.watchdog = true;` asks PID 1 to arm and feed a 60-second hardware watchdog in normal boots, so a frozen machine resets itself and the next day service start records why. It is **off by default**; observation itself needs no watchdog and writes no offsets. It loads `sp5100_tco`, the AMD chipset watchdog driver, so it fits machines whose watchdog is that device. Add it next to `day.enable`:

```nix
services.togi = {
  day.enable = true;
  day.watchdog = true;
};
```

A hardware watchdog may keep counting during s2idle, which has not been measured, so the option keeps the pre-sleep pause in `togi-day-watchdog-sleep.service`. SP5100 TCO loses timer MMIO decoding after deep sleep on the measured platform: it can still report `active` while `timeleft` reads `4294967295`, providing no reset protection. After every systemd-managed resume the unit unloads and reloads `sp5100_tco` before re-arming PID 1's watchdog. Module failures are logged and do not skip the re-arm request. The observer rechecks at sampling ticks and checkpoints, recording changes immediately; `togi status` reports its last check, not merely its startup check. Check `/sys/class/watchdog/watchdog0/timeleft` several seconds apart before and after a deep sleep longer than 90 seconds: readings must be at most `timeout` and count down between PID 1's feeds, and status must say `armed`. A present but unreadable or malformed counter, or a value greater than `timeout`, is not armed; drivers without a `timeleft` attribute retain their state-only check. A sleep started by writing `/sys/power/state` directly bypasses this unit, and `CONFIG_WATCHDOG_NOWAYOUT` or the driver's `nowayout` parameter can prevent release and reload. [Day watchdog](spec/runtime.md#day-watchdog) owns the rules and limits. A reset whose crashed boot ended at a sleep entry remains sleep-related, not evidence of a CPU hang.

## 4. Overnight: the tuning boot

Reboot and pick "NixOS - togi" in the GRUB menu once. That entry boots to a console with no desktop, and tty1 shows `togi watch --tuning-boot`, a dashboard of the session. That dashboard ignores `q`, `Q`, Ctrl-C and Ctrl-D, so a stray keypress cannot close it, and its help and footer name no quit key; `systemctl stop togi-watch.service` still ends it. Its log is on tty3 (Alt+F3), with the kernel's messages; tty2 has a login prompt. `togi.service` tunes unattended. GRUB remembers the entry, so every crash reboot returns to it and the service resumes. Shutting down or rebooting on purpose, with the power button, `poweroff` or `reboot`, leaves the tuning boot: togi stops cleanly and the next boot selects your newest normal generation. To keep tuning across your own reboots instead, set `services.togi.tuning.leaveOnShutdown = false;`. Automatic service restart-limit retry reboots are different and keep the tuning entry even with this option on ([Restart-limit retry boots and leave reasons](spec/runtime.md#restart-limit-retry-boots-and-leave-reasons)).

For a GPU-rendered terminal instead of the Linux console, enable kmscon and rebuild before selecting the tuning boot:

```nix
services.togi.tuning.display = "kmscon";
services.togi.tuning.kmscon.fontSize = 27; # Default: 160x45 cells at 2560x1440.
```

The module ships DejaVu Sans Mono, so no host font setup is needed. With kmscon, use Ctrl+Alt+F2 for the login prompt, Ctrl+Alt+F3 for logs, and Ctrl+Alt+F1 to return. Stop `togi-kmscon.service` rather than `togi-watch.service` to close that display. Failed starts or repeated crashes automatically hand tty1 back to the console watcher; tuning is independent of both displays ([Tuning boot](spec/runtime.md#tuning-boot)).

kmscon clones every connected output and fits the dashboard to the smallest grid. To keep a monitor dark, for example an OLED, disable its connector with `video=<connector>:d`. Find connector names under `/sys/class/drm/` (for example, `card0-HDMI-A-1` means `HDMI-A-1`). To limit this to the tuning boot, use the following in a NixOS module taking `{ config, lib, ... }`; replace the connector name. Priority 40 overrides the module's forced console routing, so retain the normal system's other parameters and the tuning boot's console parameters:

```nix
specialisation.togi.configuration.boot.kernelParams = lib.mkOverride 40 (
  lib.filter (p: builtins.match "console=tty[0-9]+(,.*)?" p == null)
    config.boot.kernelParams
  ++ [ "console=tty3" "printk.always_kmsg_dump=1" "video=HDMI-A-1:d" ]
);
```

The tuning boot loads `sp5100_tco` in the initrd and lets systemd arm and feed the hardware watchdog. togi starts tuning only with an active hardware watchdog; otherwise it stops with a preflight dead end ([Preflight](spec/runtime.md#preflight)). `togi events --kind preflight.check` shows the final `watchdog` result. A normal `sudo togi run` only warns.

The tuning boot also saves kernel messages on an orderly reboot or shutdown, as well as panic ([Tuning boot](spec/runtime.md#tuning-boot)). After an unclean togi boot, `togi events --json --kind crash.detected` includes `pstore.path` and the last lines of the saved kernel messages when it finds that boot's matching EFI pstore archive ([Trial outcome](spec/workloads.md#trial-outcome)). The archive stays under `/var/lib/systemd/pstore/`.

After search, checking's R7 steps run each CCD's full load, then partial loads that idle the top voltage requesters found so far, and the all-core load last ([Together trial sequence](spec/tuner.md#together-trial-sequence)). A night can spend longer on R7 than a fixed five-part schedule. `status` and the dashboard show top requesters and each core's self-sufficiency for each R7 workload.

Multi-core R7 failures follow [voltage-targeted backoff](spec/tuner.md#r7-voltage-targeted-backoff), not a hunt of the loaded cores; [R7 request order and attribution](spec/tuner.md#r7-request-order-and-attribution) says which core moves. An unattributed one whose idle cores are off CO 0 is [located](spec/tuner.md#escalation-to-a-located-hunt) by a [hunt](spec/tuner.md#hunt) of those cores only after its load has been backed off twice since its last pass, or when no loaded core can step back. R1–R6 and single-core failures keep their hunts.

No failure at CO 0 stops togi on its own trial, unless every core was at 0 in it already: togi first reruns the failing trial with every core at CO 0 (`trial.intent` ending `rerun with every core at offset 0`). [Dead ends](spec/tuner.md#dead-ends) says what each result means, and [section 6](#6-dead-ends) covers the dead end.

## 5. In the morning

Shut down or reboot. The next boot is your normal system; with `leaveOnShutdown` off, pick your normal entry in the GRUB menu. Then:

```sh
togi status
```

`status` lists the profile to enter in BIOS in its `BIOS` column, one offset per core, with each core's failure point and combinations and its phase. A `*` marks an offset that is not confirmed yet: a passed full cycle confirms it, except that a held failure's stepped-back offset stays marked until its hold ends, and until phase 1's first passed cycle there is no confirmed profile. `status` also names the phase and, in phase 2, how many rounds remain at most before one full cycle, then the clean cycles once phase 2 has concluded, valid trials per workload, missing full-cycle coverage and the Tctl peak ([Commands](spec/runtime.md#commands)). Those offsets can differ from the solo limits search found, because phase 2 moves cores toward them. Picking "NixOS - togi" again continues the session.

Choose how long to keep testing with `sudo togi run --cycles N`, replacing N with a positive count; [Checking](spec/tuner.md#checking) says when it stops. Without the flag, checking continues indefinitely. To stop at the first confirmed profile instead, right after phase 1's passed cycle and before phase 2 moves any core, use `sudo togi run --first-result` (not together with `--cycles`); run again without it to continue with phase 2.

A clean cycle is workload breadth, not proof of stability. The default cycle's R3 and R4 trials on every core ([schedule](spec/workloads.md#durations-and-checking-schedule)) add about 2.1 hours per cycle on 16 cores in simulation (a failure-free simulated cycle takes 15.6 hours instead of 13.5). A rare failure can still pass them: under the simulator's limit model, three R4 trials miss a failure one count past a core's limit about 0.01% of the time, against 4.6% for one trial; on a machine fitted to real evidence where one trial misses it 70.6% of the time, three still miss it 35.1% of the time. More cycles provide more testing, but neither a count nor a Tctl peak guarantees safety in untested real use.

Before entering the profile, understand the coverage behind “confirmed”: checking exercises the synthetic loads in the [regime table](spec/workloads.md#regimes), with full-cycle coverage defined by [Checking](spec/tuner.md#checking). Passing evidence can come from deeper offsets, so confirmation does not mean every trial ran at the exact profile you enter.

That coverage does not include light work on many cores at once or both threads of many cores at once. The idle trial is not proof of stability through real desktop idle, sleep and wake, or changing background activity. Trial offsets also do not exercise the early boot phases that a BIOS-applied offset runs through before togi starts. A confirmed profile can still crash in ordinary use.

The table shows the current `OFFSET` and, in `BIOS`, the profile to enter, and shows each core's `CCD` and `SLOT` (0-7 within that CCD). Enter the `BIOS` column of every row in the BIOS per-core Curve Optimizer controls by CCD and slot, not by Linux core number alone. BIOS labels vary by board; confirm every row before saving. Stop if the labels cannot be reconciled with the recorded CCD and slot.

After saving the profile, let it run ordinary use for a while before putting heavy or write-intensive work, such as package builds and system rebuilds, on it. Keep backups: a crash during writes risks the filesystem and package store. No fixed observation period makes the profile proven stable.

### If the profile crashes in daily use

1. Set every Curve Optimizer offset back to **0 in BIOS** before resuming normal work. Do not keep trying the failed profile.
2. Check the filesystem with the checks appropriate to that filesystem, using rescue media or an unmounted filesystem where required. Check the Nix package store's contents with `sudo nix-store --verify --check-contents`, and address any reported damage before returning to heavy or write-intensive work.
3. Do not guess a uniform step-back, even one shallower than the profile's shallowest core. It can leave the weakest core almost unchanged from the offset that crashed; it is not a tested replacement profile.
4. Record what was running, when the crash happened and every per-core offset actually entered in BIOS, then [report the crash](https://github.com/shgew/togi/issues). Keep the togi session and available system logs as evidence; remove personal and machine-identifying details from anything you publish. A readback after changing BIOS settings cannot tell you the crashed boot's offsets.

togi does not currently record daily-use crashes as feedback against the BIOS profile. Running it again or seeing the same confirmed profile in `status` does not clear that failure. Stay at BIOS CO 0 rather than treating the displayed profile as safe to re-enter.

## 6. Dead ends

togi stops by itself at a [dead end](spec/tuner.md#dead-ends) when it cannot make progress. In the tuning boot it records the dead end, persists its leave reason in GRUB, clears the saved entry, and shows the explanation on tty1 ([Dead-end actions](spec/runtime.md#dead-end-actions)).

If `togi.service` instead fails without a dead end or lock contention, systemd restarts it; repeated failures reach the start limit, which returns to the tuning boot twice and then to the normal system ([Restart-limit retry boots and leave reasons](spec/runtime.md#restart-limit-retry-boots-and-leave-reasons)). Lock contention exits 3 without an automatic restart: let the current owner finish, then start the service again.

After an abnormal service exit, inspect `journalctl -u togi.service` for the emergency cleanup and each core's zero readback before the restart ([Tuning boot](spec/runtime.md#tuning-boot)). To repeat that bounded zero-only cleanup after a crashed manual run, with no other togi process running, use `sudo togi emergency-stop`; a failure is reported instead of claiming that offsets were restored.

`togi status` shows the dead end, and the end of `togi events` shows it after the trial, failure, crash or SMU events it cites as evidence. Fix the cause, then run `sudo togi run` or pick the tuning boot again. A core whose failure at 0 ended the session stops later runs until `sudo togi reset --core <N>`; in multi-core R7 that is the top requester the dead end names. A BIOS-context change instead starts a seeded session ([section 7](#7-after-a-breaking-update)). [runtime.md](spec/runtime.md#dead-end-actions) and [tuner.md](spec/tuner.md#dead-ends) describe the conditions.

Leave reasons survive in GRUB as one pending `togi_leave_reason` record. To inspect one from the normal system before starting togi again, use `sudo grub-editenv <grubenv> list` (`<grubenv>` is described in [section 8](#8-when-the-tuning-boot-does-not-reach-togi)). Normal in-session runs and `status`/`events` do not import it. View it after the next tuning-boot run with `togi events --kind boot.leave_reason` ([Restart-limit retry boots and leave reasons](spec/runtime.md#restart-limit-retry-boots-and-leave-reasons)).

## 7. After a breaking update

**The Go to Rust switch is the exception to everything below.** The Rust build neither archives nor carries from a Go session, so the advice that the next run archives the old session does not apply to it: move the Go state directory aside first, as [Go to Rust cutover](#go-to-rust-cutover) says.

For the schema-4 vocabulary update, use `checking.cycle`, `durations.short_trial_s` and `togi run --cycles N`; rename the old settings and flag as listed in [ADR 0037](adr/0037-cycles-and-trials.md). The old names are refused. The next successful run archives the schema-3 session and carries its eligible evidence; no manual reset is needed.

An update whose changelog line starts with **BREAKING** changes the tuning rules or the journal format, so it cannot continue a session written by an earlier build. If the line also renames configuration keys or `togi run` flags, rename them first in `services.togi.settings` (or `/etc/togi/config.json`) and in anything that runs `togi run`: togi refuses the old names, and `togi run` exits 2 before it archives anything. The session needs nothing by hand: rebuild, then reboot and pick "NixOS - togi". Starting from the tuning boot is especially important after a breaking update: an in-session run without an active hardware watchdog can freeze until a manual reset. The first run archives the old session to `/var/lib/togi/archive/` and starts a new one seeded from it ([Transitions](spec/journal.md#transitions), [Session start](spec/tuner.md#session-start)).

Which evidence a transition carries, and how far back it reads, is in [Transitions](spec/journal.md#transitions) and [Fact eligibility](spec/journal.md#fact-eligibility). No manual reset is needed.

`togi status` shows the carry on its `carried:` line, and each core's first `core.phase` in `togi events --kind core.phase` names where its start came from. `togi events --kind session.carried` shows the whole event, with the session and `seq` behind every carried value.

If the BIOS context changed (BIOS version, microcode, board, CPU or boost limit), the next run archives the old session even without a ruleset update and starts a seeded session ([Session start](spec/tuner.md#session-start)). The `carried:` line explains the difference.

A configured `candidate_solo_limits` or `start_offsets` value for a core and a carried failure point combine by the precedence in [Session start](spec/tuner.md#session-start). A carried failure at 0 can stop the new session at a dead end for that core, as the old one did, until you fix the cause and run `sudo togi reset --core N` ([Dead ends](spec/tuner.md#dead-ends)).

To start over with nothing carried, run `sudo togi reset --all` instead of `togi run`. A session written by a newer ruleset or journal schema than the installed build's is still refused: install that build again, or archive the session with `sudo togi reset --all`.

## Go to Rust cutover

This step is for the one-time switch from the Go build to the Rust build, and it is the exception to [section 7](#7-after-a-breaking-update): the Rust build does not archive, carry from or read a Go session. A Go journal in `/var/lib/togi` is not a session written by an earlier build that a transition can seed from; it is another build's file. Every Rust `togi run`, `status`, `events` and `watch` whose state directory has a first journal line that is not a complete header of this build exits 1 with `<state dir>/events.jsonl: first line is not a complete header of this build; either another build's journal or a header torn by a crash; see docs/howto.md#go-to-rust-cutover`, reads no further, takes no lock and changes no file in the state directory ([Commands](spec/runtime.md#commands)). The message does not say the journal is Go's, because it cannot know: a Go journal and a Rust header torn by a crash before its first newline look the same. Rebooting into the tuning boot does not help, and `togi reset --all` is not the way out. Find which case you have with the check below, then do the matching procedure.

Which one is it? You cannot be certain from the file alone, so treat what you see as clues. Run `sudo head -c 300 /var/lib/togi/events.jsonl; echo` and look at the shape: a Go journal's first line is a flat object (no `"data"` and a `"build"` beside `"seq"`), a Rust header is `{"seq":1,...,"kind":"session.start",...,"data":{...` with the build nested under `"data"`. Look also at `/var/lib/togi/archive/` and at what the machine ran before: a state directory that Go's build wrote has Go's history. Neither line count nor a `state.json` decides it: a Go first line can lack its newline, a crash can leave a Rust header partly written before a later newline, and both builds leave a `state.json`. If the line is plainly Go's, or you are not sure, take the cutover below, which preserves the whole directory and starts a fresh session. Use [the torn first header procedure](#a-torn-first-header-of-the-rust-build) only when the line is visibly a cut-off Rust `session.start`.

Cut over, as root, before the first Rust boot:

1. Stop the Go build's tuning completely: `sudo systemctl stop togi.service togi-watch.service`, and stop any `togi run` or `togi watch` you started by hand in a terminal (Ctrl-C). Check that `pgrep -a -x togi` lists nothing: it also matches the watch service, so stop whatever it lists. Do not boot "NixOS - togi" again until step 5. A Go `togi run` still going keeps writing to the directory you are about to move.
2. Check that the backup name is free, counting a dangling symlink as taken: `sudo sh -c 'test ! -e /var/lib/togi-go && test ! -L /var/lib/togi-go && echo free'`. If it prints nothing, something already sits there, perhaps a backup of an earlier attempt. Never overwrite it: choose another name for the backup and use that name everywhere below, or stop and look at what is there.
3. Move the Go state aside without overwriting: `sudo mv -nT /var/lib/togi /var/lib/togi-go`. `-n` refuses to replace an existing destination, but `mv` can exit 0 when it skips, so its status proves nothing. Confirm that the source is gone and the destination exists: `sudo sh -c 'test -d /var/lib/togi-go && test ! -e /var/lib/togi && test ! -L /var/lib/togi && echo moved'`. If it does not print `moved`, stop: nothing was overwritten, but the cutover has not happened. Nothing in the directory is deleted, edited or read by the Rust build, and the Go build's session, archives and `state.json` stay there as they were.
4. Rebuild with the Rust package: update the `togi` input to the cutover merge commit or later, where `default` is the Rust build ([section 1](#1-install-the-module)), and leave `services.togi.package` at its default.
5. Reboot and pick "NixOS - togi". The Rust build finds no `/var/lib/togi`, creates it and starts a fresh session with nothing carried over from Go ([Session start](spec/tuner.md#session-start)). Nothing converts the Go journal, so the Go session's progress is not in the new one.

The only state directory is `/var/lib/togi`, used by one build. The moved Go directory is a plain directory under `/var/lib`, never a second state directory: no `--state-dir` and no `-rs` suffix. There is no rollback to the Go build; the directory stays at the backup path chosen above for reference.

### A torn first header of the Rust build

A crash while the very first journal line was being written can leave `events.jsonl` holding part of the Rust `session.start`. The Rust build cannot tell that from another build's journal, so it refuses it and repairs nothing automatically. A first line that never completed recorded no session, so no tuning progress should be in the file; the check in the previous section is why you believe it is this case, and if you are unsure, use the cutover steps instead. Do not edit the file, and never type a header by hand: a made-up `session.start` would carry a build stamp, boot and `seq` that nothing wrote.

1. Stop every writer and reader: `sudo systemctl stop togi.service togi-watch.service` and any `togi run` or `togi watch` started by hand. Check that `pgrep -a -x togi` lists nothing. Do not boot "NixOS - togi" again until step 4.
2. Keep the file as it is, moved out of the way without overwriting: pick a free name such as `/var/lib/togi/events.jsonl.torn-20261012`, check it with `sudo sh -c 'test ! -e NAME && test ! -L NAME && echo free'`, then `sudo mv -n /var/lib/togi/events.jsonl NAME` and confirm that `events.jsonl` is gone and `NAME` exists, as in the cutover steps (`mv -n` can exit 0 when it skips). Leave `archive/`, `trials/`, `lock`, `state.json` and everything else in place. The new session then follows the ordinary Rust archive and carry rules ([Transitions](spec/journal.md#transitions)) for whatever `archive/` holds; whether anything carries is not guaranteed and depends on its markers and schema.
3. Check with `sudo togi status`: it now fails with a missing-journal error naming `events.jsonl` (no `no session yet` message; that appears only for a journal that exists). That error only shows the file is gone; it proves nothing about what the file was. Keep the moved file as evidence.
4. Reboot and pick "NixOS - togi", or run `sudo togi run`. A new session starts. The kept file is evidence for a bug report and nothing reads it.

## 8. When the tuning boot does not reach togi

The recovery in [section 6](#6-dead-ends) runs in `togi.service` and `togi-restart-limit.service`, so it cannot help when the tuning boot fails before userspace: GRUB keeps choosing "NixOS - togi" on every boot. When the tuning boot does reach a console, tty1 shows `togi watch` and tty3 togi's log.

togi uses the GRUB environment of the first `boot.loader.grub.mirroredBoots` entry and the module refuses more than one mirror: `<grubenv>` below stands for `<path>/grub/grubenv`, where `<path>` is that entry's `path`. On an ordinary install it is `/boot/grub/grubenv`.

1. At the GRUB menu, pick one of your normal generations. Once it is up, run `sudo grub-editenv <grubenv> list`; if `saved_entry` still names the tuning entry, run `sudo grub-editenv <grubenv> unset saved_entry`.
2. If that does not get you a working system, boot rescue media that has `grub-editenv` (on a NixOS installer, `nix-shell -p grub2`) and mount the partition holding `<path>`. With `<boot>` standing for the installed system's `<path>` under the mount point (the mount point itself when `<path>` is its own partition, `<mount point><path>` when it is part of the root file system), run:

   ```sh
   grub-editenv <boot>/grub/grubenv unset saved_entry
   grub-editenv <boot>/grub/grubenv list
   ```

   The second command must not show `saved_entry`. Reboot.
3. If you entered offsets in BIOS, set Curve Optimizer back to 0 there.

## 9. Removing togi

Do this from the normal system:

1. Set `services.togi.tuning.enable = false`, or remove the module and the `togi` input, and rebuild.
2. Run `sudo grub-editenv <grubenv> list`: `saved_entry` must not name the tuning entry. If it does, run `sudo grub-editenv <grubenv> unset saved_entry`.
3. If you entered togi's offsets in BIOS and want them gone, set Curve Optimizer back to 0.

Removing togi leaves `/var/lib/togi` in place. Keeping it lets a later install continue or carry the session; archiving or deleting it is a separate choice.
