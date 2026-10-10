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

Other settings go in `services.togi.settings`, rendered to `/etc/togi/config.toml`; [runtime.md](spec/runtime.md#configuration) lists the keys.

The module also creates the unprivileged `togi-trial` system user and group and sets `backend_user = "togi-trial"` in the configuration. Togi itself still runs as root; mprime and y-cruncher use that account, which can write only their own trial instance directory within togi's state ([Backends](spec/workloads.md#backends)). Rebuild after upgrading to this behavior so the account exists before the next run. A hand-written configuration must set `backend_user` to an existing unprivileged account; [Privileges](spec/runtime.md#privileges) and [Preflight](spec/runtime.md#preflight) say what is refused.

To move to a newer release, change the tag in `url`, run `nix flake update togi` and rebuild. An update whose changelog line starts with **BREAKING** starts a seeded session on the next run ([section 7](#7-after-a-breaking-update)).

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

For a manual Linux install, protect the lock's parent from unprivileged writes, or provision it during trusted early boot before users can precreate its name in a sticky shared directory. After quiescing all lock users, this descriptor-based procedure provisions or migrates a single root-owned regular inode without following a lock symlink. The example requires an existing `togi-hardware` group; replace the final argument with `-` for root-only mode 0600. An existing foreign or multiply linked inode is refused without changing it, and a held lock makes the procedure fail rather than replacing it.

The procedure creates the lock only under a parent that [Host lock](spec/runtime.md#host-lock) accepts.

```sh
nix-shell -p python3 --run 'sudo "$(command -v python3)" - /run/lock/togi.lock togi-hardware' <<'PY'
import fcntl
import grp
import os
import stat
import sys

path, group = sys.argv[1:]
parent = os.stat(os.path.dirname(path))
if parent.st_uid != 0 or parent.st_mode & 0o022:
    raise SystemExit("lock parent must be root-owned and not writable by other users")
gid = 0 if group == "-" else grp.getgrnam(group).gr_gid
fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
try:
    info = os.fstat(fd)
    if not stat.S_ISREG(info.st_mode) or info.st_uid != 0 or info.st_nlink != 1:
        raise SystemExit("refusing an unsafe or foreign lock inode")
    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    os.fchmod(fd, 0o600)
    os.fchown(fd, 0, gid)
    if group != "-":
        os.fchmod(fd, 0o660)
finally:
    os.close(fd)
PY
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

## 4. Overnight: the tuning boot

Reboot and pick "NixOS - togi" in the GRUB menu once. That entry boots to a console with no desktop, and tty1 shows `togi watch`, a dashboard of the session. Its log is on tty3 (Alt+F3), with the kernel's messages; tty2 has a login prompt. `togi.service` tunes unattended. GRUB remembers the entry, so every crash reboot returns to it and the service resumes. Shutting down or rebooting on purpose, with the power button, `poweroff` or `reboot`, leaves the tuning boot: togi stops cleanly and the next boot selects your newest normal generation. To keep tuning across your own reboots instead, set `services.togi.tuning.leaveOnShutdown = false;`. Automatic service restart-limit retry reboots are different and keep the tuning entry even with this option on ([Restart-limit retry boots and leave reasons](spec/runtime.md#restart-limit-retry-boots-and-leave-reasons)).

The tuning boot loads `sp5100_tco` in the initrd and lets systemd arm and feed the hardware watchdog. togi starts tuning only with an active hardware watchdog; otherwise it stops with a preflight dead end ([Preflight](spec/runtime.md#preflight)). `togi events --kind preflight.check` shows the final `watchdog` result. A normal `sudo togi run` only warns.

The tuning boot also saves kernel messages on an orderly reboot or shutdown, as well as panic ([Tuning boot](spec/runtime.md#tuning-boot)). After an unclean togi boot, `togi events --json --kind crash.detected` includes `pstore.path` and the last lines of the saved kernel messages when it finds that boot's matching EFI pstore archive ([Trial outcome](spec/workloads.md#trial-outcome)). The archive stays under `/var/lib/systemd/pstore/`.

After search, checking's R7 steps run each CCD's full load, then partial loads that idle the top voltage requesters found so far, and the all-core load last ([Together trial sequence](spec/tuner.md#together-trial-sequence)). A night can spend longer on R7 than a fixed five-part schedule. `status` and the dashboard show top requesters and each core's self-sufficiency for each R7 workload.

Multi-core R7 failures follow [voltage-targeted backoff](spec/tuner.md#r7-voltage-targeted-backoff), not a hunt of the loaded cores; [R7 request order and attribution](spec/tuner.md#r7-request-order-and-attribution) says which core moves. An unattributed one whose idle cores are off CO 0 is first located by a [hunt](spec/tuner.md#hunt) of those cores. R1–R6 and single-core failures keep their hunts.

No failure at CO 0 stops togi on its own trial, unless every core was at 0 in it already: togi first reruns the failing trial with every core at CO 0 (`trial.intent` ending `rerun with every core at CO 0`). [Dead ends](spec/tuner.md#dead-ends) says what each result means, and [section 6](#6-dead-ends) covers the dead end.

## 5. In the morning

Shut down or reboot. The next boot is your normal system; with `leaveOnShutdown` off, pick your normal entry in the GRUB menu. Then:

```sh
togi status
```

`status` lists the profile to enter in BIOS, one offset per core, with each core's failure point and combinations and its phase. It also reports clean cycles, valid trials per workload, missing full-cycle coverage and the Tctl peak ([Commands](spec/runtime.md#commands)). After deepening those offsets can differ from the checked solo limits search found. Picking "NixOS - togi" again continues the session.

Choose how long to keep testing with `sudo togi run --cycles N`, replacing N with a positive count; [Checking](spec/tuner.md#checking) says when it stops. Without the flag, checking continues indefinitely.

A clean cycle is workload breadth, not proof of stability. The default cycle's R3 and R4 trials on every core ([schedule](spec/workloads.md#durations-and-checking-schedule)) add about 2.1 hours per cycle on 16 cores in simulation (a failure-free simulated cycle takes 15.6 hours instead of 13.5). A rare failure can still pass them: under the simulator's limit model, three R4 trials miss a failure one count past a core's limit about 0.01% of the time, against 4.6% for one trial; on a machine fitted to real evidence where one trial misses it 70.6% of the time, three still miss it 35.1% of the time. More cycles provide more testing, but neither a count nor a Tctl peak guarantees safety in untested real use.

The table labels those values `OFFSET` and shows each core's `CCD` and `SLOT` (0-7 within that CCD). Match every row to the BIOS per-core Curve Optimizer controls by CCD and slot, not by Linux core number alone. BIOS labels vary by board; confirm every row before saving. Stop if the labels cannot be reconciled with the recorded CCD and slot.

## 6. Dead ends

togi stops by itself at a [dead end](spec/tuner.md#dead-ends) when it cannot make progress. In the tuning boot it records the dead end, persists its leave reason in GRUB, clears the saved entry, and shows the explanation on tty1 ([Dead-end actions](spec/runtime.md#dead-end-actions)).

If `togi.service` instead fails without a dead end or lock contention, systemd restarts it; repeated failures reach the start limit, which returns to the tuning boot twice and then to the normal system ([Restart-limit retry boots and leave reasons](spec/runtime.md#restart-limit-retry-boots-and-leave-reasons)). Lock contention exits 3 without an automatic restart: let the current owner finish, then start the service again.

`togi status` shows the dead end, and the end of `togi events` shows it after the trial, failure, crash or SMU events it cites as evidence. Fix the cause, then run `sudo togi run` or pick the tuning boot again. A core whose failure at 0 ended the session stops later runs until `sudo togi reset --core <N>`; in multi-core R7 that is the top requester the dead end names. A BIOS-context change instead starts a seeded session ([section 7](#7-after-a-breaking-update)). [runtime.md](spec/runtime.md#dead-end-actions) and [tuner.md](spec/tuner.md#dead-ends) describe the conditions.

Leave reasons survive in GRUB as one pending `togi_leave_reason` record. To inspect one from the normal system before starting togi again, use `sudo grub-editenv <grubenv> list` (`<grubenv>` is described in [section 8](#8-when-the-tuning-boot-does-not-reach-togi)). Normal in-session runs and `status`/`events` do not import it. View it after the next tuning-boot run with `togi events --kind boot.leave_reason` ([Restart-limit retry boots and leave reasons](spec/runtime.md#restart-limit-retry-boots-and-leave-reasons)).

## 7. After a breaking update

For the schema-4 vocabulary update, use `[checking] cycle`, `durations.short_trial_s` and `togi run --cycles N`; rename the old settings and flag as listed in [ADR 0037](adr/0037-cycles-and-trials.md). The old names are refused. The next successful run archives the schema-3 session and carries its eligible evidence; no manual reset is needed.

An update whose changelog line starts with **BREAKING** changes the tuning rules or the journal format, so it cannot continue a session written by an earlier build. If the line also renames configuration keys or `togi run` flags, rename them first in `services.togi.settings` (or `/etc/togi/config.toml`) and in anything that runs `togi run`: togi refuses the old names, and `togi run` exits 2 before it archives anything. The session needs nothing by hand: rebuild, then reboot and pick "NixOS - togi". Starting from the tuning boot is especially important after a breaking update: an in-session run without an active hardware watchdog can freeze until a manual reset. The first run archives the old session to `/var/lib/togi/archive/` and starts a new one seeded from it ([Transitions](spec/journal.md#transitions), [Session start](spec/tuner.md#session-start)).

Which evidence a transition carries, and how far back it reads, is in [Transitions](spec/journal.md#transitions) and [Fact eligibility](spec/journal.md#fact-eligibility). No manual reset is needed.

`togi status` shows the carry on its `carried:` line, and each core's first `core.phase` in `togi events --kind core.phase` names where its start came from. `togi events --kind session.carried` shows the whole event, with the session and `seq` behind every carried value.

If the BIOS context changed (BIOS version, microcode, board, CPU or boost limit), the next run archives the old session even without a ruleset update and starts a seeded session ([Session start](spec/tuner.md#session-start)). The `carried:` line explains the difference.

A configured `candidate_solo_limits` or `start_offsets` value for a core and a carried failure point combine by the precedence in [Session start](spec/tuner.md#session-start). A carried failure at 0 can stop the new session at a dead end for that core, as the old one did, until you fix the cause and run `sudo togi reset --core N` ([Dead ends](spec/tuner.md#dead-ends)).

To start over with nothing carried, run `sudo togi reset --all` instead of `togi run`. A session written by a newer ruleset or journal schema than the installed build's is still refused: install that build again, or archive the session with `sudo togi reset --all`.

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
