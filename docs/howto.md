# How to tune a machine with shycler

For a Zen 5 (Granite Ridge) desktop running NixOS with GRUB. Every command below runs on that machine.

## 1. Install the module

Add shycler to your flake inputs and import its module in the host's configuration:

```nix
# flake.nix
inputs.shycler = {
  url = "git+https://code.marleb.org/shgew/shycler.git";
  inputs.nixpkgs.follows = "nixpkgs";
};
```

```nix
# the host's NixOS configuration
{ inputs, ... }:
{
  imports = [ inputs.shycler.nixosModules.default ];
  services.shycler = {
    enable = true;
    tuning.enable = true;
    backends.mprime.enable = true;
    backends.ycruncher.enable = true;
  };
}
```

mprime and y-cruncher are unfree: allow them with `nixpkgs.config.allowUnfree = true`, or with an `allowUnfreePredicate` naming `mprime` and `y-cruncher`.

The module loads `ryzen_smu` through `hardware.cpu.amd.ryzen-smu.enable`. A host that already loads its own `ryzen_smu` build sets `hardware.cpu.amd.ryzen-smu.enable = false`.

Other settings go in `services.shycler.settings`, rendered to `/etc/shycler/config.toml`; [runtime.md](spec/runtime.md#configuration) lists the keys.

## 2. Rebuild, then boot fresh

Rebuild (`nixos-rebuild boot`, or your usual way), then reboot. In BIOS, every Curve Optimizer offset must be 0. Do not run another tool that writes Curve Optimizer offsets in the boot you tune from: shycler reads the offsets it finds at the start of a session as the baseline.

## 3. A quick in-session test

```sh
sudo shycler run
```

It prints one line per event: the preflight checks, the BIOS context, the baseline, then each search trial's intent, start and end. Watch a few trials, then press Ctrl-C. It writes each core back to its baseline, or to its current offset where that is shallower, records `shutdown` and exits 0; the next `sudo shycler run` resumes where it stopped.

```sh
shycler status
shycler events --kind trial
```

If the machine crashes during a trial, it comes back to the normal desktop; the next `sudo shycler run` attributes the crash from the journal and continues.

## 4. Overnight: the tuning boot

Reboot and pick "NixOS - shycler" in the GRUB menu once. That entry boots to a console with no desktop, and tty1 shows `shycler watch`, a dashboard of the session. Its log is on tty3 (Alt+F3), with the kernel's messages; tty2 has a login prompt. `shycler.service` tunes unattended. GRUB remembers the entry, so every crash reboot returns to it and the service resumes. Shutting down or rebooting on purpose, with the power button, `poweroff` or `reboot`, leaves the tuning boot: shycler stops cleanly and the next boot selects your newest normal generation. To keep tuning across your own reboots instead, set `services.shycler.tuning.leaveOnShutdown = false;`.

## 5. In the morning

Shut down or reboot. The next boot is your normal system; with `leaveOnShutdown` off, pick your normal entry in the GRUB menu. Then:

```sh
shycler status
shycler cert
```

`cert` lists each core's edge, the deepest offset tested stable, to enter in BIOS once you judge the tier sufficient. Picking "NixOS - shycler" again continues the same session.

## 6. Dead ends

shycler stops by itself only at a dead end: a core fails at offset 0, the SMU misbehaves, trials prove nothing several times in a row, the machine keeps crashing before any trial, a backend escapes its cores, or a preflight check fails. In the tuning boot it records the dead end, clears GRUB's saved entry so the next boot selects your newest normal generation, and stops with the explanation on tty1's dashboard. After a boot loop it reboots into the normal system at once.

`shycler status` shows the dead end, and `shycler events --kind deadend,boot.saved_entry` shows what happened. Fix the cause, then run `sudo shycler run` or pick "NixOS - shycler" again to resume. A core that failed at offset 0 stops every later run until `sudo shycler reset --core <N>`, and a changed BIOS context until `sudo shycler reset --all` starts a new session. [runtime.md](spec/runtime.md#dead-end-actions) and [tuner.md](spec/tuner.md) describe each condition.

## 7. Starting again after a breaking update

An update whose changelog line starts with **BREAKING** refuses to continue a session written by an earlier build. Before archiving it, note each core's failed mark, or its edge where it has none, from `shycler status`. Archive the session with `sudo shycler reset --all`; the archived journal stays in `/var/lib/shycler/archive/`.

To skip searching for edges you already know, give each core a candidate edge: its failed mark plus one, or its edge where it has no failed mark. Such a core starts the new session in confirmation at that offset. Confirmation still runs every trial, so a value that no longer holds costs a failure and moves the core one count shallower:

```nix
services.shycler.settings.candidate_edges = {
  "0" = -36;
  "8" = -50;
};
```

Rebuild, then run `sudo shycler run` or pick "NixOS - shycler". The values are read only when the new session records each core's first phase; remove them once it has started. `start_offsets` is the gentler alternative: search starts from that offset and still steps deeper until it fails.
