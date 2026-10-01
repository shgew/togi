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

## 3. A quick in-session test

```sh
sudo togi run
```

It shows the session as a dashboard redrawn every second: search, a hunt or refinement when active, the running start and one tile per core. Watch a few trials, then press Ctrl-C. It restores each core to its baseline or its current offset when shallower, records `shutdown` and exits 0; the next run resumes. `sudo togi run --no-tui` prints one line per event instead.

```sh
togi status
togi events --kind trial
```

If the machine crashes during a trial, it comes back to the normal desktop; the next `sudo togi run` attributes the crash from the journal and continues.

## 4. Overnight: the tuning boot

Reboot and pick "NixOS - togi" in the GRUB menu once. That entry boots to a console with no desktop, and tty1 shows `togi watch`, a dashboard of the session. Its log is on tty3 (Alt+F3), with the kernel's messages; tty2 has a login prompt. `togi.service` tunes unattended. GRUB remembers the entry, so every crash reboot returns to it and the service resumes. Shutting down or rebooting on purpose, with the power button, `poweroff` or `reboot`, leaves the tuning boot: togi stops cleanly and the next boot selects your newest normal generation. To keep tuning across your own reboots instead, set `services.togi.tuning.leaveOnShutdown = false;`.

The tuning boot loads `sp5100_tco` in the initrd and lets systemd arm and feed the hardware watchdog. Before writing any Curve Optimizer offsets or starting a workload, togi waits up to 30 seconds for an active hardware watchdog, checking once per second. If it never arms, togi stops with a preflight dead end and clears the saved entry; it does not start tuning without reset protection. `togi events --kind preflight.check` shows the final `watchdog` result. A normal `sudo togi run` does not require or wait for the watchdog.

## 5. In the morning

Shut down or reboot. The next boot is your normal system; with `leaveOnShutdown` off, pick your normal entry in the GRUB menu. Then:

```sh
togi status
togi cert
```

`cert` lists the profile to enter in BIOS, one offset per core, with each core's failed and joint marks, its done state and the tier-clock evidence. Do not enter it in BIOS before it reaches Silver: Bronze can rest on a single two-minute R4 start, and under the simulator's edge model one such start misses a failure one count past a core's edge about 4.6% of the time. After refinement those offsets can differ from the checked edges search found. A clean qualifying rotation earns Bronze only when every core is done and refinement can reach no more total depth. Picking "NixOS - togi" again continues the session.

The table labels those values `OFFSET` and shows each core's `CCD` and `SLOT` (0-7 within that CCD). Match every row to the BIOS per-core Curve Optimizer controls by CCD and slot, not by Linux core number alone. BIOS labels vary by board; confirm every row before saving. Stop if the labels cannot be reconciled with the recorded CCD and slot.

## 6. Dead ends

togi stops by itself at a dead end: failure at offset 0, an untrusted SMU, repeated trials without evidence, a missing backend, repeated stray crashes, an escaped backend thread, a thermal-trip reset or failed preflight. In the tuning boot it records the dead end, clears GRUB's saved entry, and shows the explanation on tty1. After a boot-loop dead end it reboots into the normal system. If `togi.service` instead fails without a dead end or lock contention, systemd starts it again after a minute; once it has failed three times within 30 minutes, `togi-restart-limit.service` clears the entry and reboots into the normal system, with `leaveOnShutdown` on or off. Lock contention exits 3 without an automatic restart or recovery action: let the current owner finish, then start the service again.

`togi status` shows the dead end, and the end of `togi events` shows it after the trial, failure, crash or SMU events it cites as evidence. Fix the cause, then run `sudo togi run` or pick the tuning boot again. A core that failed at 0 stops later runs until `sudo togi reset --core <N>`. A BIOS-context change instead automatically archives the old session and starts a seeded one carrying candidate edges but not failed marks. [runtime.md](spec/runtime.md#dead-end-actions) and [tuner.md](spec/tuner.md) describe the conditions.

## 7. After a breaking update

An update whose changelog line starts with **BREAKING** changes the tuning rules or the journal format, so it cannot continue a session written by an earlier build. Nothing needs doing by hand: rebuild, then run `sudo togi run` or pick "NixOS - togi". The first run archives the old session to `/var/lib/togi/archive/` and starts a new one that carries what the old one found:
- each core's deepest offset passing an isolated trial becomes a candidate edge, and the new core checks it in search with the full R1 and R2 start count;
- each core's shallowest attributed failure, including a single culprit found by a hunt, becomes a carried failed mark; joint marks do not carry, and reset or defect exclusions still apply;
- a candidate edge at or deeper than its carried mark is clamped one count shallower.

When the old session was started by hand after an earlier breaking update, with `reset --all`, the carry also reads the sessions archived before it, as long as they ran under the same BIOS and each under a different ruleset from the one after it.

`togi status` shows the carry on its `carried:` line, and each core's first `core.phase` in `togi events --kind core.phase` names where its start came from. `togi events --kind session.carried` shows the whole event, with the session and `seq` behind every carried value.

If the BIOS context changed (BIOS version, microcode, board, CPU or boost limit), the next run archives the old session even without a ruleset update and starts a seeded session. Only candidate edges carry: old failed marks do not apply under the changed BIOS. The `carried:` line explains the difference.

A configured `candidate_edges` or `start_offsets` value for a core wins over what is carried, but a carried failed mark still clamps it. A core with a carried mark at 0 failed at CO 0: the new session stops at a dead end for it, as the old one did, until you fix the cause and run `sudo togi reset --core N`.

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
