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

The module also creates the unprivileged `togi-trial` system user and group and sets `backend_user = "togi-trial"` in the configuration. Togi itself still runs as root; mprime and y-cruncher use that account without supplementary groups. They can update their generated inputs and results in their own trial instance directory, but cannot modify togi's surrounding state, journal or control files. Rebuild after upgrading to this behavior so the account exists before the next run. A hand-written configuration must set `backend_user` to an existing account with non-root UID and primary GID: missing, nonexistent or root credentials refuse preflight, never run a backend as root.

To move to a newer release, change the tag in `url`, run `nix flake update togi` and rebuild. An update whose changelog line starts with **BREAKING** starts a seeded session on the next run ([section 7](#7-after-a-breaking-update)).

mprime and y-cruncher come from the host's `nixpkgs`. Do not update them during a session, for example with `nix flake update nixpkgs`: togi does not yet tell a new backend build's passes from the old one's, so passes earned by the old binaries would keep counting for the new ones.

### Host lock and delegated hardware tests

`run`, `reset` and hardware tests share one host lock across all state directories. A busy lock exits 3 before hardware, journal or boot-entry changes. Do not delete `/run/lock/togi.lock` to unblock it: replacing an inode while another process holds it would allow two owners. In a tuning boot, contention is not automatically restarted and never drives restart-limit GRUB clearing or reboot.

The module provisions a root-owned mode-0600 lock during boot, before user logins, in NixOS's root-owned mode-0755 `/run/lock`. It leaves an existing lock's inode, owner and mode alone. On an upgrade from the public-readable lock, a root acquisition restricts legacy permissions in place, even if an old read-only holder is still running; it still exits 3 until that holder closes the lock. Changing permissions cannot revoke descriptors opened earlier. Stop all tuning and hardware-test processes and close their lock descriptors, or rebuild and reboot, to complete the security migration. A foreign-owned, symlink, special or multiply linked lock is refused, not deleted or repaired blindly; on Linux, rebooting into the rebuilt normal system recreates `/run` and provisions the trusted lock before users can create one.

Delegated hardware tests need an explicitly authorized dedicated group, as well as their existing SMU/SMN permissions and delegated cpuset controller. Do not give this access to the unprivileged backend account. For example:

```nix
users.groups.togi-hardware = { };
users.users.operator.extraGroups = [ "togi-hardware" ];
services.togi.hardwareTestGroup = "togi-hardware";
```

Rebuild, then reboot to provision the root-owned mode-0660 lock for that group; log in again for group membership to take effect. The module's creation-only rule deliberately does not change an existing lock when this option changes, including when set back to `null`. A fresh boot applies the new authorization. Run `just hardware` sequentially as before; ordinary users without this group cannot open the lock, even if they can read togi's state.

For a manual Linux install, protect the lock's parent from unprivileged writes, or provision it during trusted early boot before users can precreate its name in a sticky shared directory. After quiescing all lock users, this descriptor-based procedure provisions or migrates a single root-owned regular inode without following a lock symlink. The example requires an existing `togi-hardware` group; replace the final argument with `-` for root-only mode 0600. An existing foreign or multiply linked inode is refused without changing it, and a held lock makes the procedure fail rather than replacing it.

Directory validation requires both the lock's containing directory and its ancestors to be root- or caller-owned, regardless of their permission bits: a foreign owner can chmod even a mode-0555 directory and replace its children, splitting the lock domain. The execution namespace's `/` is trusted independently of its owner's numeric UID mapping, unless it is itself the containing directory. Group- or world-writable directories still need the sticky bit, and a foreign-owned ancestor is refused even with that bit. This permits private locks under authorized sandbox ancestry without changing production lock ownership or delegated access.

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

On Darwin, only copied-journal `reset` uses `/tmp/togi.lock`: the first operator may create a private mode-0600 lock and reuse it without `sudo`. A different operator cannot rely on the old public-readable file; use the owning operator or explicitly provision root-owned group access after quiescing all old descriptors. Hardware tuning remains Linux-only.

## 2. Rebuild, then boot fresh

Rebuild (`nixos-rebuild boot`, or your usual way), then reboot. In BIOS, every Curve Optimizer offset must be 0. Do not run another tool that writes Curve Optimizer offsets in the boot you tune from: togi reads the offsets it finds at the start of a session as the baseline.

Then check that the machine is ready, without starting a session:

```sh
sudo togi doctor
```

It makes the checks `sudo togi run` makes before tuning and prints one row per check: `ok`, `FAIL`, `warn` or `skipped`. The last line is `ready`, or `not ready:` with the failed checks, which `run` would turn into a preflight dead end; fix those first. A failed check gives `not ready` with or without `sudo`. A `warn` on `watchdog` means no hardware watchdog is active in this boot, which the tuning boot provides. A `warn` on `bios_context` means the BIOS context changed since the session started, so the next run archives the session and starts a new one ([section 7](#7-after-a-breaking-update)). `doctor` appends nothing to the journal and changes no offset. Without `sudo` it makes only the checks that need no root and marks the rest `skipped`; when none of its checks failed, it ends with `not fully checked: run sudo togi doctor`.

## 3. A quick in-session test

Start sessions from the [tuning boot](#4-overnight-the-tuning-boot), especially after a breaking update: it has hardware-watchdog reset protection. An in-session run is an optional way to watch a few trials on the desktop, not the recommended way to start a session.

```sh
sudo togi run
```

It shows the session as the `togi watch` dashboard, redrawn whenever the journal changes: what togi is doing and why, the running trial and what each outcome would lead to, the stages and a table of offsets per CCD. `togi watch` in another terminal shows the same screen and takes keys: `?` explains it. Watch a few trials, then press Ctrl-C. It restores each core to its baseline or its current offset when shallower, records `shutdown` and exits 0; the next run resumes. `sudo togi run --no-tui` prints one line per event instead.

```sh
togi status
togi events --kind trial
```

If the machine freezes during an in-session trial, it needs a manual reset unless an active hardware watchdog provides reset protection. `sudo togi run` records a `session.warning` when no hardware watchdog is active and continues; it does not arm one. The warning appears in the dashboard's history and stays in `togi events --kind session.warning`. After a reset or crash reboot, the next run attributes the crash from the journal and continues.

## 4. Overnight: the tuning boot

Reboot and pick "NixOS - togi" in the GRUB menu once. That entry boots to a console with no desktop, and tty1 shows `togi watch`, a dashboard of the session. Its log is on tty3 (Alt+F3), with the kernel's messages; tty2 has a login prompt. `togi.service` tunes unattended. GRUB remembers the entry, so every crash reboot returns to it and the service resumes. Shutting down or rebooting on purpose, with the power button, `poweroff` or `reboot`, leaves the tuning boot: togi stops cleanly and the next boot selects your newest normal generation. To keep tuning across your own reboots instead, set `services.togi.tuning.leaveOnShutdown = false;`. Automatic service restart-limit retry reboots are different: their boot-local `/run/togi-retry-tuning-boot` marker makes the shutdown hook preserve the tuning entry even with this option on. The recovery command creates the marker only after its GRUB operations succeed and removes it if reboot fails; the shell fallback removes it as well.

The tuning boot loads `sp5100_tco` in the initrd and lets systemd arm and feed the hardware watchdog. Before writing any Curve Optimizer offsets or starting a workload, togi waits up to 30 seconds for an active hardware watchdog, checking once per second. If it never arms, togi stops with a preflight dead end and clears the saved entry; it does not start tuning without reset protection. `togi events --kind preflight.check` shows the final `watchdog` result. A normal `sudo togi run` checks once and warns when none is active, but does not require or wait for the watchdog.

The tuning boot also enables kernel-message dumps on orderly reboot or shutdown, as well as panic. When the next `run` detects an unclean togi boot and finds its EFI pstore archive, `togi events --json --kind crash.detected` includes `pstore.path` and the last lines of the saved kernel messages. The ordinary event line and dashboard name the archive location. The archive stays under `/var/lib/systemd/pstore/`; missing records, such as a hard freeze without a dump, leave the field absent. Reading this optional diagnostic cannot stop recovery.

After search, an R7 step runs each CCD's full load, then partial loads that idle the top voltage requesters found so far, down to at least two loaded cores. The all-core load runs last. These partials count toward the cycle, so a night can spend longer on R7 than a fixed five-part schedule. `status` and the dashboard show top requesters and each core's self-sufficiency for each R7 workload.

Multi-core R7 does not hunt its loaded cores: every failure requires a voltage-targeted backoff, unless the core it would move already sits shallower than in that failure, as it often does for carried failures. A named computation error counts against its core; an unattributed failure moves one core in the affected CCD's top-request group. If that group is already at CO 0, the next movable group in the same CCD's request order moves instead. In an all-core failure, a CCD whose loaded cores were all at CO 0 is left out while the other CCD has a movable loaded core, unless a stalled core confined the failure to that CCD; only an affected CCD with every loaded core at 0 is a dead end. A named core at 0 dead-ends only if it was a top requester in that trial; otherwise the failure routes to its CCD's top group, or to the loaded CCD's when its own CCD was idle. Backoff raises the selected core toward a measured passing voltage, rounding up at 3.6 mV per count; without qualifying passes, or when only its offset stands in for its request, it moves one count. After stepping past a top group at CO 0 with measured requests, the selected core moves enough counts to rise above that group's request, unless reaching CO 0 stops it first. R1–R6 and single-core failures keep their hunts and first-failure rules.

Before an unattributed multi-core R7 failure moves a loaded core, a located hunt reruns the same load with every idle core at 0, the loaded cores unchanged (`hunt.group` stage `locate`). If that fails too, the hunt ends `loaded` and the backoff above follows; if it passes, the hunt narrows the idle cores and backs off the one or combination it finds, leaving the loaded cores where they were.

## 5. In the morning

Shut down or reboot. The next boot is your normal system; with `leaveOnShutdown` off, pick your normal entry in the GRUB menu. Then:

```sh
togi status
```

`status` lists the profile to enter in BIOS, one offset per core, with each core's failure point and combinations and its phase. It reports clean cycles since the last deepening, their count and the latest cycle number, valid trials per workload, missing full-cycle coverage, and the Tctl peak from passes together since the last profile change with its source trial-end event. After deepening those offsets can differ from the checked solo limits search found. Picking "NixOS - togi" again continues the session.

Choose how long to keep testing with `sudo togi run --cycles N`, replacing N with a positive count. It stops after N clean cycles valid for the current profile, only when every core is at its limit and deepening can reach no more total depth. Without the flag, checking continues indefinitely. An earlier clean cycle can still count after deepening returns to an equal or shallower profile, if every core was at its limit when it ended, it followed the latest reset, and no failure since that reset contradicted its profile.

A clean cycle is workload breadth, not proof of stability. A single two-minute R4 trial can miss a rare failure; under the simulator's limit model it misses a failure one count past a core's limit about 4.6% of the time. More cycles provide more testing, but neither a count nor a Tctl peak guarantees safety in untested real use.

The table labels those values `OFFSET` and shows each core's `CCD` and `SLOT` (0-7 within that CCD). Match every row to the BIOS per-core Curve Optimizer controls by CCD and slot, not by Linux core number alone. BIOS labels vary by board; confirm every row before saving. Stop if the labels cannot be reconciled with the recorded CCD and slot.

## 6. Dead ends

togi stops by itself at a dead end: failure at offset 0, an untrusted SMU, repeated trials without evidence, a missing backend, repeated stray crashes, an escaped backend thread, a thermal-trip reset or failed preflight. In the tuning boot it records the dead end, persists its leave reason in GRUB, clears the saved entry, and shows the explanation on tty1. After a boot-loop dead end it reboots into the normal system. Incompatible or unknown-kind journal refusals also persist their reason and clear the entry, but never append to the refused journal. If saving a reason fails, togi still attempts the clear and reports the reason error separately; a successful clear still permits a required boot-loop reboot. A failed clear never requests reboot.

If `togi.service` instead fails without a dead end or lock contention, systemd starts it again after a minute. Three failed starts within 30 minutes reach the service start limit. The first two consecutive limit hits reboot back into the tuning boot; the third clears the saved entry and reboots into the normal system, with `leaveOnShutdown` on or off. The cross-boot count resets on a tuning-boot run's first successful durable journal append, not merely on process startup or successful preflight; even a durably recorded preflight refusal resets it. This count does not limit crash reboots during tuning. Lock contention exits 3 without an automatic restart or recovery action: let the current owner finish, then start the service again.

If the `restart-limit` command itself fails, the shell hook falls back to the normal system. It best-effort removes the retry marker and saves `restart-limit command failed; returning to the normal system` with count 0 (unknown) and a `fallback-<service invocation id>` identity, then clears the saved entry and requests a normal, non-forced reboot only if the clear succeeds. Failure to remove the marker or save the fallback reason does not prevent the clear. Recovery never sets GRUB's `next_entry`.

`togi status` shows the dead end, and the end of `togi events` shows it after the trial, failure, crash or SMU events it cites as evidence. Fix the cause, then run `sudo togi run` or pick the tuning boot again. A core whose failure at 0 ended the session stops later runs until `sudo togi reset --core <N>`; in multi-core R7 that is the top requester the dead end names. A BIOS-context change instead automatically archives the old session and starts a seeded one carrying candidate solo limits but not failure points. [runtime.md](spec/runtime.md#dead-end-actions) and [tuner.md](spec/tuner.md) describe the conditions.

Leave reasons survive in GRUB as one pending `togi_leave_reason` record. To inspect one from the normal system before starting togi again, use `sudo grub-editenv <grubenv> list` (`<grubenv>` is described in [section 8](#8-when-the-tuning-boot-does-not-reach-togi)). Normal in-session runs and `status`/`events` do not import it. On the next tuning-boot run that can append durably to its journal, togi records `boot.leave_reason` with `reason_id`, `restart_limit_count` and `reason`, then clears the GRUB record. View it with `togi events --kind boot.leave_reason`. A crash or clear failure after the event is durable does not journal the record twice: the next run matches its unique ID and only retries clearing. This event does not change the journal schema.

## 7. After a breaking update

For the schema-4 vocabulary update, use `[checking] cycle`, `durations.short_trial_s` and `togi run --cycles N`; rename the old settings and flag as listed in [ADR 0037](adr/0037-cycles-and-trials.md). The old names are refused. The next successful run archives the schema-3 session and carries its eligible evidence; no manual reset is needed.

An update whose changelog line starts with **BREAKING** changes the tuning rules or the journal format, so it cannot continue a session written by an earlier build. If the line also renames configuration keys or `togi run` flags, rename them first in `services.togi.settings` (or `/etc/togi/config.toml`) and in anything that runs `togi run`: togi refuses the old names, and `togi run` exits 2 before it archives anything. The session needs nothing by hand: rebuild, then reboot and pick "NixOS - togi". Starting from the tuning boot is especially important after a breaking update: a transition starts checking each core at its carried solo limit, the deepest offset that core passed alone in the earlier session. An in-session run without an active hardware watchdog can freeze until a manual reset. The first run archives the old session to `/var/lib/togi/archive/` and starts a new one that carries what the old one found:
- each core's deepest offset passing a trial run alone becomes a candidate solo limit, and the new core checks it in search; eligible carried passes count toward the required trials of its frozen R1 and R2 classes;
- each core's shallowest attributed failure, including a single culprit found by a hunt, becomes a carried failure point; combinations do not carry, and reset or defect exclusions still apply;
- a candidate solo limit at or deeper than its carried failure point is clamped one count shallower.

The ruleset-9 transition also counts ruleset-8 record-only R7 outcomes as ordinary evidence. Same-BIOS carried R7 failures are evaluated before the first trial and can move a core immediately; a carried failure whose core already sits shallower than in that failure moves nothing unless the core later returns there. Their requests and CCD clocks are retained or reconstructed from archived samples. No manual reset is needed for this transition.

When the archived session was not itself seeded by a carry, the carry also reads older sessions under the same BIOS, going back while each differs in ruleset or evidence epoch from the one after it and stopping after the first that was seeded. `reset --all` permanently excludes sessions at or before its boundary, including their copied evidence; later transitions never reach behind it.

`togi status` shows the carry on its `carried:` line, and each core's first `core.phase` in `togi events --kind core.phase` names where its start came from. `togi events --kind session.carried` shows the whole event, with the session and `seq` behind every carried value.

If the BIOS context changed (BIOS version, microcode, board, CPU or boost limit), the next run archives the old session even without a ruleset update and starts a seeded session. Only candidate solo limits carry: old failure points do not apply under the changed BIOS. The `carried:` line explains the difference.

A configured `candidate_solo_limits` or `start_offsets` value for a core wins over what is carried, but a carried failure point still clamps it. A core with a carried failure point at 0 failed at CO 0: the new session stops at a dead end for it, as the old one did, until you fix the cause and run `sudo togi reset --core N`.

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

## 10. Removing togi

Do this from the normal system:

1. Set `services.togi.tuning.enable = false`, or remove the module and the `togi` input, and rebuild.
2. Run `sudo grub-editenv <grubenv> list`: `saved_entry` must not name the tuning entry. If it does, run `sudo grub-editenv <grubenv> unset saved_entry`.
3. If you entered togi's offsets in BIOS and want them gone, set Curve Optimizer back to 0.

Removing togi leaves `/var/lib/togi` in place. Keeping it lets a later install continue or carry the session; archiving or deleting it is a separate choice.
