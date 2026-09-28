# How to tune a machine with togi

For a Zen 5 (Granite Ridge) desktop running NixOS with GRUB. Every command below runs on that machine.

## 1. Install the module

Add togi to your flake inputs and import its module in the host's configuration:

```nix
# flake.nix
inputs.togi = {
  url = "github:shgew/togi";
  inputs.nixpkgs.follows = "nixpkgs";
};
```

```nix
# the host's NixOS configuration
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

## 2. Rebuild, then boot fresh

Rebuild (`nixos-rebuild boot`, or your usual way), then reboot. In BIOS, every Curve Optimizer offset must be 0. Do not run another tool that writes Curve Optimizer offsets in the boot you tune from: togi reads the offsets it finds at the start of a session as the baseline.

## 3. A quick in-session test

```sh
sudo togi run
```

It shows the session as a dashboard that redraws every second: the stage, the running trial and one tile per core. Watch a few trials, then press Ctrl-C. It writes each core back to its baseline, or to its current offset where that is shallower, records `shutdown` and exits 0; the next `sudo togi run` resumes where it stopped. `sudo togi run --no-tui` prints one line per event instead: the preflight checks, the BIOS context, the baseline, then each search trial's intent, start and end.

```sh
togi status
togi events --kind trial
```

If the machine crashes during a trial, it comes back to the normal desktop; the next `sudo togi run` attributes the crash from the journal and continues.

## 4. Overnight: the tuning boot

Reboot and pick "NixOS - togi" in the GRUB menu once. That entry boots to a console with no desktop, and tty1 shows `togi watch`, a dashboard of the session. Its log is on tty3 (Alt+F3), with the kernel's messages; tty2 has a login prompt. `togi.service` tunes unattended. GRUB remembers the entry, so every crash reboot returns to it and the service resumes. Shutting down or rebooting on purpose, with the power button, `poweroff` or `reboot`, leaves the tuning boot: togi stops cleanly and the next boot selects your newest normal generation. To keep tuning across your own reboots instead, set `services.togi.tuning.leaveOnShutdown = false;`.

## 5. In the morning

Shut down or reboot. The next boot is your normal system; with `leaveOnShutdown` off, pick your normal entry in the GRUB menu. Then:

```sh
togi status
togi cert
```

`cert` lists each core's edge, the deepest offset tested stable, to enter in BIOS once you judge the tier sufficient. Picking "NixOS - togi" again continues the same session.

## 6. Dead ends

togi stops by itself only at a dead end: a core fails at offset 0, the SMU misbehaves, trials prove nothing several times in a row, the machine keeps crashing before any trial, a backend escapes its cores, or a preflight check fails. In the tuning boot it records the dead end, clears GRUB's saved entry so the next boot selects your newest normal generation, and stops with the explanation on tty1's dashboard. After a boot loop it reboots into the normal system at once.

`togi status` shows the dead end, and `togi events --kind deadend,boot.saved_entry` shows what happened. Fix the cause, then run `sudo togi run` or pick "NixOS - togi" again to resume. A core that failed at offset 0 stops every later run until `sudo togi reset --core <N>`, and a changed BIOS context until `sudo togi reset --all` starts a new session. [runtime.md](spec/runtime.md#dead-end-actions) and [tuner.md](spec/tuner.md) describe each condition.

## 7. Starting again after a breaking update

An update whose changelog line starts with **BREAKING** refuses to continue a session written by an earlier build. In particular, ruleset-2 sessions must be archived before resuming under ruleset 3, which starts confirmation with R2 mprime AVX-512. Before archiving, note each core's failed mark from the FAILED column of `togi status`, and for cores without one the offset each was last confirmed at, from `togi events --kind core.phase`. OFFSET in `status` can be shallower than that after a suspect backoff. Archive the session with `sudo togi reset --all`; the archived journal stays in `/var/lib/togi/archive/`.

To skip searching for edges you already know, give each core a candidate edge: its failed mark plus one, or the offset it was last confirmed at where it has no failed mark. A core with failed mark 0 gets none: it failed at CO 0, so fix that cause first. Such a core starts the new session in confirmation at that offset. Confirmation still runs every trial, so a value that no longer holds costs a failure and moves the core one count shallower:

```nix
services.togi.settings.candidate_edges = {
  "0" = -36;
  "8" = -50;
};
```

Rebuild, then run `sudo togi run` or pick "NixOS - togi". Each value is read only when the new session records that core's first phase; once every core in `togi status` shows a phase, remove them so a later `reset --all` starts from the baseline. If a configured candidate edge remains at or deeper than a failed mark in the session being archived, `reset --all` warns but still archives it; use the value it suggests (the failed mark plus one), or remove the edge. If the failed mark is 0, remove the edge and fix the cause first. `start_offsets` is the gentler alternative: search starts from that offset and still steps deeper until it fails.

## 8. Moving from shycler

togi was called shycler up to 0.3.1. The journal format and the tuning rules did not change, so a shycler session continues under togi, but every installed name did. Do this from the normal system, not the tuning boot:

1. Make sure no tuning boot is pending: `sudo grub-editenv /boot/grub/grubenv list` must not show `saved_entry` naming the shycler entry. If it does, run `sudo grub-editenv /boot/grub/grubenv unset saved_entry`. With mirrored boot directories, use the GRUB environment of the first one. Stop any `shycler run` still going.
2. Point the flake input at `github:shgew/togi`, import `inputs.togi.nixosModules.default`, and rename `services.shycler` to `services.togi`; the options under it keep their names.
3. Move the state: `sudo mv /var/lib/shycler /var/lib/togi`.
4. Rebuild. `/etc/togi/config.toml` replaces `/etc/shycler/config.toml`, and `togi status` shows the session where shycler left it.

Pick "NixOS - togi" to continue tuning.
