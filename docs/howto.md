# How to tune a machine with togi

For a Zen 5 (Granite Ridge) desktop running NixOS with GRUB or systemd-boot. Every command below runs on that machine.

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

To move to a newer release, change the tag in `url`, run `nix flake update togi` and rebuild. Ruleset 14 archives Ruleset 13 unchanged, starts from verified stock and reconstructs the durable campaign ([section 7](#7-after-a-breaking-update)).

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

Rebuild (`nixos-rebuild boot`, or your usual way), then reboot. In BIOS, set every Curve Optimizer offset to 0. [Session start](spec/tuner.md#session-start) requires a verified all-zero baseline; nonzero or incomplete readback is refused, not clamped or merely noted. Do not run another tool that writes Curve Optimizer offsets in the boot you tune from.

How togi treats the BIOS values:

- It reads every core's offset from the SMU once, at the first `run` of a session, and requires an all-zero baseline: a nonzero or incomplete readback is refused, not clamped ([Session start](spec/tuner.md#session-start)). Keep BIOS Curve Optimizer at 0; a BIOS profile that crashes before togi runs surfaces as a boot-loop dead end ([ADR 0082](adr/0082-bios-stays-at-zero.md)).
- Every write sets the core's absolute offset; the SMU does not add it to the BIOS value. Firmware restores BIOS values on reboot; togi's trial writes replace those offsets.
- The BIOS context that ties a session to a BIOS (BIOS version, board, CPU model, microcode and boost limit) holds no Curve Optimizer offsets. Changing a Curve Optimizer offset in BIOS does not start a new session, and togi keeps the baseline it captured.
- A clean stop restores every core to CO 0 through the verified write/readback path ([Exit restoration](spec/runtime.md#exit-codes)). It does not capture the baseline again.

Then check that the machine is ready, without starting a session:

```sh
sudo togi doctor
```

It makes the checks `sudo togi run` makes before tuning ([Preflight](spec/runtime.md#preflight)) and prints one row per check: `ok`, `FAIL`, `warn` or `skipped`. The last line is `ready`, or `not ready:` with the failed checks; fix those first. A `warn` on `watchdog` means no hardware watchdog is active in this boot, which the tuning boot provides. A `warn` on `bios_context` means the recorded BIOS context changed: the next run records the new context in the same session and follows [firmware invalidation](spec/tuner.md#firmware), rather than archiving it. `doctor` appends nothing to the journal and changes no offset; [Commands](spec/runtime.md#commands) describes what it does without `sudo`.

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

After a session exists, leave firmware Curve Optimizer at CO 0 and arm PID 1's hardware watchdog. `sudo togi day` refuses a nonzero all-core baseline or an unarmed/non-counting watchdog, then applies **stock (CO 0) only** and observes normal use until Ctrl-C or a stop signal. Nonzero day offsets are not enabled; the applying capability enforces this without changing tuner decisions ([day transitions](spec/runtime.md#ruleset-13-policy-boundary)). It never starts test workloads. It reads BIOS context and every core at start and checkpoints active hours excluding suspend every ten minutes. An orderly shutdown, including a short power-button shutdown handled by systemd, zeroes and verifies every core before its final checkpoint and clean-stop marker. A later start records an earlier observed boot without that marker as unclean, with its checkpoint, offsets, reset reason and available kernel evidence; unavailable evidence stays unknown.

`togi status` shows the Day summary; `togi events --kind day` shows the raw records. Hours are checkpointed lower bounds, not a stability guarantee. An interval whose two readbacks differ cannot identify when offsets changed, so it remains in boot hours but earns no hours for either profile.

### Pausing for important work

Keep the day service running and name your user in `services.togi.day.users`. These commands use its credential-checked socket without root:

```sh
togi day pause                 # until the next cold boot
togi day pause 3h              # until three hours from now, including across reboot
togi day pause --until 18:30   # until the next local 18:30
togi day resume                # end any pause early
togi day freeze                # retain D, block promotions only
togi day unfreeze              # lift that promotion freeze
```

Every pause zeroes every core and verifies readback, keeping the learned observations. An untimed pause survives a service restart and sleep, but ends at the next cold boot. A timed pause keeps its journaled deadline across all three. Five minutes before expiry, the desktop notifier offers to extend the current deadline by one hour; dismissing it changes nothing. Ending a pause, early or at its deadline, uses the full transaction and announces verified success. **The current applier still forces stock (CO 0)**: no command here restores a historical nonzero profile.

On the main `togi watch` view, press `p` for pause/resume or `f` for freeze/unfreeze; help (`?`) describes the keys. Freeze is not pause: it retains D and prevents climbing, never safety retreats or a firmware reset. Paused hours do not earn a hold, and a cold boot that starts paused cannot promote later in that boot. [Day transitions](spec/runtime.md#day-pause-and-freeze) own the exact lifecycle and stock-only restriction.


### Answering what happened

An unclean end alone does not say whether the CPU caused it: graphics undervolts and power cuts crash a machine too. So each unclean end becomes a question, and `togi status` and `togi watch` show it, with what togi saw, until you answer. The four answers:

1. `frozen`: it froze.
2. `graphics`: graphics died but sound went on.
3. `power-off`: I turned it off while it was working normally.
4. `unsure`: not sure.

```sh
togi day answer <boot> frozen
```

`<boot>` is the boot id `status` prints. Answering needs no root shell, but it needs the day service running and your user allowed on its socket: name your user in `services.togi.day.users` (below). Run the same command again to change an answer; the last one counts. The answer is journaled against the crashed boot and its offset profile. Ruleset 13 consumes the answers and recorded evidence under [Daily events](spec/tuner.md#daily-events), including provisional CPU-suspect classification while an end remains unanswered. A later answer can reclassify that event; it does not enable nonzero hardware application.

### Recording a crash togi did not observe

A freeze from before the day service existed has no unclean end to answer. `sudo togi day record` journals it, so Ruleset 13 can treat the offsets it ran as a whole-profile veto; the [journal spec](spec/journal.md#day-observation) has the rules, and the command prints what the record will do before it appends. Leave the day service running: the command sends your confirmed record to its credential-checked socket, and the live writer validates and appends it. The service refuses a record drafted from a session other than the one it observes, such as one a `--state-dir` naming another directory selects. Only when no service runs does the command take the writer lock itself; a refused or failed socket request never falls back to a second writer.

For a boot the observer saw end uncleanly, name it with `--boot <boot>`; the offsets are the ones the observer read back, unless you pass `--offset` for the ones you remember entering:

```sh
sudo togi day record --boot <boot> --running game
```

For a crash with no recorded end, give when it happened with `--unobserved-at` and the offsets with `--offset`, plus when you entered them in firmware setup with `--entered-at`. Each `--offset` is `all=N`, `ccdI=N` or `coreI=N`; a more specific one overrides a less specific one, and every core needs a value. To record the 2026-10-19 freezes:

```sh
# the freeze at -25 and -30 by CCD (check which CCD took which value)
sudo togi day record --unobserved-at "<crash time>" --offset ccd0=-25 --offset ccd1=-30 \
  --entered-at "<time you entered the offsets>" --running build

# the freeze at a profile that differs per core: one --offset coreI=N per core, N from your profile
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
  day.users = [ "alice" ]; # may answer and control pause/freeze without root; replace with your user
};
```

The `togi-day.service` unit is absent from the tuning specialisation. Enabling it keeps journald persistent with a five-second sync interval. Before a manual `run` or `reset` in a normal boot, use `systemctl stop togi-day.service`; stop zeroes and verifies before recording a clean marker. Start it again with `systemctl start togi-day.service` afterward. A same-boot restart, including `nixos-rebuild switch`, reapplies stock without promoting. Systemd-managed sleep restores the watchdog before the resume hook requests another stock transaction with all-core readbacks. An abnormal exit runs zero-only emergency recovery before the delayed restart. Inspect diagnostics with `journalctl -u togi-day.service`. Without a session, verified zero baseline or armed/counting watchdog, the service reports why it cannot start.

The optional `services.togi.day.watchdog = true;` asks PID 1 to arm and feed a 60-second hardware watchdog in normal boots, so a frozen machine resets itself and the next day service start records why. It is **off by default**; stock-only day application still requires an armed, counting watchdog supplied by this option or another system configuration. It loads `sp5100_tco`, the AMD chipset watchdog driver, so it fits machines whose watchdog is that device. Add it next to `day.enable`:

```nix
services.togi = {
  day.enable = true;
  day.watchdog = true;
};
```

A hardware watchdog may keep counting during s2idle, which has not been measured, so the option keeps the pre-sleep pause in `togi-day-watchdog-sleep.service`. SP5100 TCO loses timer MMIO decoding after deep sleep on the measured platform: it can still report `active` while `timeleft` reads `4294967295`, providing no reset protection. After every systemd-managed resume the unit unloads and reloads `sp5100_tco` before re-arming PID 1's watchdog. Module failures are logged and do not skip the re-arm request. The observer rechecks at sampling ticks and checkpoints, recording changes immediately; `togi status` reports its last check, not merely its startup check. Check `/sys/class/watchdog/watchdog0/timeleft` several seconds apart before and after a deep sleep longer than 90 seconds: readings must be at most `timeout` and count down between PID 1's feeds, and status must say `armed`. A present but unreadable or malformed counter, or a value greater than `timeout`, is not armed; drivers without a `timeleft` attribute retain their state-only check. A sleep started by writing `/sys/power/state` directly bypasses this unit, and `CONFIG_WATCHDOG_NOWAYOUT` or the driver's `nowayout` parameter can prevent release and reload. [Day watchdog](spec/runtime.md#day-watchdog) owns the rules and limits. A reset whose crashed boot ended at a sleep entry remains sleep-related, not evidence of a CPU hang.

### Deliberately letting the watchdog fire

This is a **destructive reset test**, not a routine health check or a stability test. It deliberately panics the kernel while the day service is running. Unsaved data will be lost; `sync` reduces pending writes but cannot make an abrupt reset safe. Save work, stop unrelated workloads, keep backups, and have local access to reset or power-cycle the machine if the watchdog fails. Run only in a normal boot, with BIOS Curve Optimizer at **0 on every core**, an existing compatible session, and the day/watchdog options above enabled. Do not test with an offset profile you want to qualify: this creates real, permanent crash evidence in that session.

1. **Check before crashing.** Leave `togi-day.service` running; stopping it would write a clean-stop marker. Record these outputs somewhere you can read after reboot:

   ```sh
   togi --version
   systemctl is-active togi-day.service
   cat /proc/sys/kernel/random/boot_id
   togi status
   togi events --kind day
   cat /sys/class/watchdog/watchdog0/{identity,state,timeout}
   for i in 1 2 3; do cat /sys/class/watchdog/watchdog0/timeleft; sleep 3; done
   ```

   Require an active day service, a `day.boot_start` for this boot with every readback at 0, and `status` reporting PID 1's watchdog armed. For the module's SP5100 device, require identity `SP5100 TCO timer`, state `active`, timeout `60`, and changing `timeleft` values no greater than 60 (never `4294967295`). If `status` identifies another watchdog number, use that device's sysfs directory. Do not proceed on an unavailable or non-counting watchdog.

2. **Exclude another reboot mechanism.** This procedure requires Magic SysRq support (`/proc/sysrq-trigger`), no loaded crash kernel, and no panic debugger or platform-specific panic handler that reboots, powers off or feeds the hardware watchdog. Check your kernel configuration if uncertain:

   ```sh
   sudo test -w /proc/sysrq-trigger
   cat /sys/kernel/kexec_crash_loaded
   sysctl kernel.panic
   sudo sysctl -w kernel.panic=0
   ```

   Require `kexec_crash_loaded` to be `0`; if that file is absent, establish that kexec crash dumping is disabled in the kernel rather than assuming it. Save the original `kernel.panic` value. Setting it to 0 disables the kernel's own timed panic reboot for this boot, so a fast panic reboot cannot masquerade as watchdog expiry. Do not add a persistent sysctl setting. If you cancel the test, restore the saved value with `sudo sysctl -w kernel.panic=<saved-value>`.

3. **Flush, then trigger once.** The last command below intentionally freezes normal execution and will not return. Do not paste it until all prerequisites pass:

   ```sh
   sudo journalctl --sync
   sync
   sudo sh -c 'echo c > /proc/sysrq-trigger'
   ```

   [Magic SysRq `c`](https://docs.kernel.org/admin-guide/sysrq.html) triggers a kernel panic; writing `/proc/sysrq-trigger` as an administrator does not require enabling the keyboard SysRq mask. The [kernel panic path](https://github.com/torvalds/linux/blob/v6.12/kernel/panic.c) stops other CPUs and, with no crash kernel and `kernel.panic=0`, waits instead of scheduling a reboot. PID 1 can no longer feed the hardware watchdog. [systemd's runtime watchdog](https://www.freedesktop.org/software/systemd/man/latest/systemd-system.conf.html#RuntimeWatchdogSec=) is fed by PID 1, not by `togi-day.service`; merely stopping the day service would not test expiry.

   Expect a hardware reset within roughly one configured timeout of the last feed, not exactly 60 seconds after your command. Observe the elapsed time externally. If no reset occurs after 90 seconds, recover locally with reset or power-cycle and retain the failed result; that manual reset does not prove the watchdog fired. Do not repeat blindly.

4. **Inspect the next normal boot.**

   ```sh
   systemctl is-active togi-day.service
   togi status
   togi events --kind day
   journalctl -b -1 -k --no-pager
   journalctl -b -u togi-day.service --no-pager
   ```

   The boot ID must change. The next day service start records `day.unclean_end` for the old boot, with its last checkpoint (if any), last read-back offsets and available reset/log evidence; it does not invent exposure after that checkpoint or a clean stop. It opens the usual four-answer question in `status`/`watch` and the desktop notifier. A proven immediate-successor watchdog reason is retained as such; unavailable reset reporting remains unknown. The evidence classification is not guaranteed to say only “watchdog”: a retained kernel fault or graphics tail can take precedence, and a watchdog reset ending at sleep entry is sleep-related ([classification](spec/runtime.md#day-observation)). Missing panic log lines are not proof that no panic occurred.

   Answer with `togi day answer <old-boot> power-off` for this deliberately interrupted, otherwise working machine, not `frozen` as though it had spontaneously hung. This records the operator's intentional interruption; it does not erase the evidence or prove CPU stability. At stock the daily policy treats the event as a platform observation, not evidence against a nonzero profile. An unanswered question remains pending; the [answering procedure](#answering-what-happened) also explains changing an answer. Check that the watchdog is armed and counting again after reboot. Normal boot reapplies the configured panic sysctl policy; verify it with `sysctl kernel.panic`.

## 4. Overnight: the tuning boot

Enable `services.togi.tuning.enable = true;` with exactly one supported boot loader, GRUB or systemd-boot, and rebuild. Switch from the day with `sudo togi night`. It shows what the night will do, as `togi status` plans it, then asks `Reboot into the night now? [y/N]`. Only `y` or `yes` goes on: it says good night, persistently selects the current generation's tuning entry, remembers the boot selection it replaces and reboots. Any other answer changes nothing. In a normal boot, `sudo togi watch` offers the same switch on `n`, with a confirmation.

The tuning entry boots to a console with no desktop. tty1 shows `togi watch --tuning-boot`, a dashboard that ignores `q`, `Q`, Ctrl-C and Ctrl-D; `systemctl stop togi-watch.service` still ends it. Its log and the kernel's messages are on tty3 (Alt+F3); tty2 has a login prompt. `togi.service` tunes unattended. A night begun with `togi night` has no time limit, and crash reboots stay in tuning with either loader.

End the night with `F10` on the tuning dashboard and answer `y` to `Reboot into the day cycle?`, which stops the run cleanly first, or use the power button, `poweroff` or `reboot`. The tuner also ends it at a dead end or its restart limit. To keep tuning across your own reboots instead, set `services.togi.tuning.leaveOnShutdown = false;`. Automatic restart-limit retry reboots keep the tuning entry even with the option on ([retry policy](spec/runtime.md#restart-limit-retry-boots-and-leave-reasons)).

Ending restores the previous boot policy, not a normal generation chosen by togi. On systemd-boot that includes an absent EFI default, an explicit entry, a glob or `@saved`, together with its last-booted state. A newer default you select during the night wins over the remembered one. Stale saved IDs are restored verbatim and systemd-boot applies its normal fallback.

You can also pick the tuning specialisation in the boot menu. GRUB remembers that choice automatically. systemd-boot remembers it only with effective `@saved`; with an explicit default or pattern, crash reboots follow that policy instead. Prefer `togi night` for unattended crash recovery with any policy. The module installs the loader tools and writes `/etc/togi/night.json`; see [Night and day](spec/runtime.md#night-and-day) for the exact restoration rules.

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

A night works through its queues (reproduction, qualification, mapping, controls, requalification) by their shares of elapsed observed time, then the filler ([Night order](spec/tuner.md#night-order-and-reserved-budgets)). R7 loads run each CCD's full load, then partial loads derived from measured requests, and the all-core load ([coverage matrix](spec/workloads.md#coverage-matrix)). `status` and the dashboard show each queue's share used.

Mapping descends one CCD at a time with only that CCD tuned, so a failure is charged to that CCD ([Night edge U](spec/tuner.md#night-edge-u)). Failures at a nonzero whole profile stay profile-level evidence until isolated testing locates them.

A conclusive failure at CO 0 stops togi on its own: it records a `platform unstable at stock` dead end naming the class, rather than tuning around it. A failure at a nonzero profile that names a CO 0 core first gets one rerun of the same class with every CCD at CO 0. [Dead ends](spec/tuner.md#dead-ends) and [CO-0 controls](spec/tuner.md#co-0-controls) say what each result means, and [section 6](#6-dead-ends) covers the dead end.

## 5. In the morning

Shut down or reboot. The next boot follows your restored normal selection; with `leaveOnShutdown` off, use F10 to end the night or pick your normal entry in the boot menu. Then:

```sh
togi status
```

`status` shows, for each CCD, its applied day offset `D`, its cap `S` with the slope (fallback or measured) and what binds it, its edge `U`, the counts still to go, and the trial classes covered and uncovered for the cap. Below the table it shows the Day policy (applied profile, hold, latest step and what blocks it, freeze, firmware, events and vetoes) and the Night (elapsed observed time and each queue's share used; [Commands](spec/runtime.md#commands)); [How togi tunes](how-togi-tunes.md) explains each. Another tuning run continues the session.

For the evidence behind those labels:

```sh
togi status --evidence
sudo togi doctor --day
```

The evidence view puts the selected exact profile beside stock observations, exposes rate/slope witnesses, hours and coverage gaps, and names the governing [cap rule](spec/tuner.md#cap-s-margin-from-measured-slope). “No observations”, “non-mapping exposure only” and “mapping exposure inadequate” are different gaps: daily hours and qualification hours do not become isolated mapping exposure. CO 0 is an untuned fallback, not a claim that stock meets the hazard goal. No view gives a safety verdict, an ETA or a promise that more nights will produce a nonzero profile.

The first observed day boot after a night has a Morning report in `status`; `togi day notify` also sends it to the desktop. It summarizes recorded trial ends and observed task time, and says that automatic nonzero application is disabled. It is reconstructed from the existing journal and retained across a day-service restart in that boot, not repeated on the next day boot. Notification deduplication is per notifier process lifetime: restarting the notifier may show it again. A notification is not proof of current hardware offsets.

`doctor --day` checks the day prerequisites rather than stress-workload availability. Its baseline row asks whether this boot has a complete, verified CO-0 baseline witness, separately from the journal's applied D or a current zero readback. Missing proof is not silently filled in. Like ordinary `doctor`, it writes no journal event and changes no offset; root hardware checks still require the host lock, so stop a competing hardware process first. See [day diagnostics](spec/runtime.md#day-diagnostics) for the exact checks.

A night has no end time: `sudo togi run` tunes until you end it with Ctrl-C, F10 in the dashboard or a reboot (the power button included), and restores every core to CO 0 when it stops. A crash reboot into the tuning boot continues the same night; the next run after you end it opens a new one. The other stopping conditions are the tuner's [DeadEnd and Unavailable results](spec/tuner.md#night-query-results), including blocked stock controls with no other admissible work, and runtime safety dead ends.

A night's mapping and qualification are evidence, not proof of stability. Qualification runs the [coverage matrix](spec/workloads.md#coverage-matrix) on the exact profile under test; classes that do not bound the cap are listed as uncovered, never as safe.

That coverage does not include light work on many cores at once or both threads of many cores at once beyond the matrix's own classes. The idle trial is not proof of stability through real desktop idle, sleep and wake, or changing background activity. Trial offsets also do not exercise the early boot stages before togi applies a profile. A qualified profile can still crash in ordinary use.

Keep BIOS Curve Optimizer at **0**; never copy a proposed day profile into firmware. [Ruleset 14](spec/tuner-ruleset14.md#4-a-clean-hold-earns-one-step) requires 50 clean exact-profile hours even at initial stock, exact next-profile qualification and an eligible cold boot for each bounded step toward C. Status/watch show D/C/U, hold and campaign allowance, not “remaining to S”; S is diagnostic only. Evidence reads `experimental: N clean hours; at most 1 crash per X h (95%, fixed-rate model)`. Keep backups: neither a completed hold nor a finished climb proves stability.

### If the profile crashes in daily use

1. Reboot and answer the question about the unclean end (`togi day answer`, [Answering what happened](#answering-what-happened)). A CPU-suspect event vetoes that whole profile and every later day profile at or deeper than it on every CCD. It schedules a step back at the next cold boot and one reproduction night; [daily-event escalation](spec/tuner.md#whole-profile-veto-and-escalation) determines which steps retreat. A clean reproduction does not clear the veto; only evidence that reclassifies the event does.
2. Check the filesystem with the checks appropriate to that filesystem, using rescue media or an unmounted filesystem where required. Check the Nix package store's contents with `sudo nix-store --verify --check-contents`, and address any reported damage before returning to heavy or write-intensive work.
3. If togi was not running when the machine froze, [record the crash](#recording-a-crash-togi-did-not-observe) with the offsets that were applied.
4. [Report the crash](https://github.com/shgew/togi/issues) with what was running and when; keep the togi session and available system logs as evidence, and remove personal and machine-identifying details from anything you publish.

## 6. Dead ends

togi stops by itself at a [dead end](spec/tuner.md#dead-ends) when it cannot make progress. In the tuning boot it records the dead end, persists its leave reason in the boot loader's state, restores the day's selection, and shows the explanation on tty1 ([Dead-end actions](spec/runtime.md#dead-end-actions)).

If `togi.service` instead fails without a dead end or lock contention, systemd restarts it; repeated failures reach the start limit, which returns to the tuning boot twice and then to the normal system ([Restart-limit retry boots and leave reasons](spec/runtime.md#restart-limit-retry-boots-and-leave-reasons)). Lock contention exits 3 without an automatic restart: let the current owner finish, then start the service again.

After an abnormal service exit, inspect `journalctl -u togi.service` for the emergency cleanup and each core's zero readback before the restart ([Tuning boot](spec/runtime.md#tuning-boot)). To repeat that bounded zero-only cleanup after a crashed manual run, with no other togi process running, use `sudo togi emergency-stop`; a failure is reported instead of claiming that offsets were restored.

`togi status` shows the dead end, and the end of `togi events` shows it after the trial, failure, crash or SMU events it cites as evidence. Fix the cause, then run `sudo togi run` or pick the tuning boot again. Follow [stock-control failure handling](spec/tuner.md#co-0-controls), not a per-core blacklist or reset command. A BIOS/AGESA/microcode change continues the session under [firmware invalidation](spec/tuner.md#firmware); it does not seed a new one. [runtime.md](spec/runtime.md#dead-end-actions) and [tuner.md](spec/tuner.md#dead-ends) describe the stop conditions.

Leave reasons survive outside the tuning journal as one pending `togi_leave_reason` record: GRUB's environment block or systemd-boot's persistent `/var/lib/togi-boot/backend.json`. With GRUB, inspect it from the normal system with `sudo grub-editenv <grubenv> list` (`<grubenv>` is described in [section 8](#8-when-the-tuning-boot-does-not-reach-togi)); with systemd-boot, inspect that JSON file. Normal in-session runs and `status`/`events` do not import it. View it after the next tuning-boot run with `togi events --kind boot.leave_reason` ([retry state](spec/runtime.md#restart-limit-retry-boots-and-leave-reasons)).

## 7. After a breaking update

**The Go to Rust switch is the exception to everything below.** The Rust build neither archives nor imports from a Go session: move the Go state directory aside first, as [Go to Rust cutover](#go-to-rust-cutover) says.

Update configuration before the tuning boot; invalid configuration exits 2 before archiving. Rebuild and reboot into the tuning boot with its active watchdog. The Ruleset 14 cutover archives Ruleset 13 unchanged and starts the durably identified successor at verified stock; no manual reset is needed.

It imports original-identity raw daily observations, compatible `exposure.observed`, and CPU-suspect events, stops and vetoes needed to reconstruct the campaign. Inspect commitments with `togi events --kind successor.imported` and `togi events --kind campaign.start`. Missing/unreconstructible campaign history blocks nonzero exploration; old S, qualification conclusions and nonzero D are never permission.

The two-event machine allowance includes provisional charges and is never renewed by restart, reset, firmware change, new session or reboot. Later non-CPU reclassification is displayed separately without refund. `sudo togi reset --all` stops raw tuning carry at a permanent boundary, but campaign-ledger records cross it. There is no per-core reset.

A firmware change remains in-session: it returns to stock and requires new-context exact qualification and 50 clean stock hours before guarded restoration of the previous full profile. It never reopens a stopped unit.

For rollback, an older build refuses to resume a newer-ruleset journal before append. Same-schema inspection remains possible with warnings/skipped unknown kinds, but strict reset may refuse unknown additive kinds; reinstall the writing build to archive it safely. [Reset and compatibility](spec/runtime.md#reset-and-compatibility) describes the exact operation-specific behavior. `SCHEMA` remains 1; build-fingerprint changes rebuild snapshots.

## Go to Rust cutover

This step is for the one-time switch from the Go build to the Rust build, and it is the exception to [section 7](#7-after-a-breaking-update): the Rust build does not archive, import from or read a Go session. A Go journal in `/var/lib/togi` is another build's file, not an eligible Rust observation source. Every Rust `togi run`, `status`, `events` and `watch` whose state directory has a first journal line that is not a complete header of this build exits 1 with `<state dir>/events.jsonl: first line is not a complete header of this build; either another build's journal or a header torn by a crash; see docs/howto.md#go-to-rust-cutover`, reads no further, takes no lock and changes no file in the state directory ([Commands](spec/runtime.md#commands)). The message does not say the journal is Go's, because it cannot know: a Go journal and a Rust header torn by a crash before its first newline look the same. Rebooting into the tuning boot does not help, and `togi reset --all` is not the way out. Find which case you have with the check below, then do the matching procedure.

Which one is it? You cannot be certain from the file alone, so treat what you see as clues. Run `sudo head -c 300 /var/lib/togi/events.jsonl; echo` and look at the shape: a Go journal's first line is a flat object (no `"data"` and a `"build"` beside `"seq"`), a Rust header is `{"seq":1,...,"kind":"session.start",...,"data":{...` with the build nested under `"data"`. Look also at `/var/lib/togi/archive/` and at what the machine ran before: a state directory that Go's build wrote has Go's history. Neither line count nor a `state.json` decides it: a Go first line can lack its newline, a crash can leave a Rust header partly written before a later newline, and both builds leave a `state.json`. If the line is plainly Go's, or you are not sure, take the cutover below, which preserves the whole directory and starts a fresh session. Use [the torn first header procedure](#a-torn-first-header-of-the-rust-build) only when the line is visibly a cut-off Rust `session.start`.

Cut over, as root, before the first Rust boot:

1. Stop the Go build's tuning completely: `sudo systemctl stop togi.service togi-watch.service`, and stop any `togi run` or `togi watch` you started by hand in a terminal (Ctrl-C). Check that `pgrep -a -x togi` lists nothing: it also matches the watch service, so stop whatever it lists. Do not boot "NixOS - togi" again until step 5. A Go `togi run` still going keeps writing to the directory you are about to move.
2. Check that the backup name is free, counting a dangling symlink as taken: `sudo sh -c 'test ! -e /var/lib/togi-go && test ! -L /var/lib/togi-go && echo free'`. If it prints nothing, something already sits there, perhaps a backup of an earlier attempt. Never overwrite it: choose another name for the backup and use that name everywhere below, or stop and look at what is there.
3. Move the Go state aside without overwriting: `sudo mv -nT /var/lib/togi /var/lib/togi-go`. `-n` refuses to replace an existing destination, but `mv` can exit 0 when it skips, so its status proves nothing. Confirm that the source is gone and the destination exists: `sudo sh -c 'test -d /var/lib/togi-go && test ! -e /var/lib/togi && test ! -L /var/lib/togi && echo moved'`. If it does not print `moved`, stop: nothing was overwritten, but the cutover has not happened. Nothing in the directory is deleted, edited or read by the Rust build, and the Go build's session, archives and `state.json` stay there as they were.
4. Rebuild with the Rust package: update the `togi` input to the cutover merge commit or later, where `default` is the Rust build ([section 1](#1-install-the-module)), and leave `services.togi.package` at its default.
5. Reboot and pick "NixOS - togi". The Rust build finds no `/var/lib/togi`, creates it and starts a fresh session without importing Go observations ([Transitions](spec/journal.md#transitions)). Nothing converts the Go journal, so the Go session's progress is not in the new one.

The only state directory is `/var/lib/togi`, used by one build. The moved Go directory is a plain directory under `/var/lib`, never a second state directory: no `--state-dir` and no `-rs` suffix. There is no rollback to the Go build; the directory stays at the backup path chosen above for reference.

### A torn first header of the Rust build

A crash while the very first journal line was being written can leave `events.jsonl` holding part of the Rust `session.start`. The Rust build cannot tell that from another build's journal, so it refuses it and repairs nothing automatically. A first line that never completed recorded no session, so no tuning progress should be in the file; the check in the previous section is why you believe it is this case, and if you are unsure, use the cutover steps instead. Do not edit the file, and never type a header by hand: a made-up `session.start` would carry a build stamp, boot and `seq` that nothing wrote.

1. Stop every writer and reader: `sudo systemctl stop togi.service togi-watch.service` and any `togi run` or `togi watch` started by hand. Check that `pgrep -a -x togi` lists nothing. Do not boot "NixOS - togi" again until step 4.
2. Keep the file as it is, moved out of the way without overwriting: pick a free name such as `/var/lib/togi/events.jsonl.torn-20261012`, check it with `sudo sh -c 'test ! -e NAME && test ! -L NAME && echo free'`, then `sudo mv -n /var/lib/togi/events.jsonl NAME` and confirm that `events.jsonl` is gone and `NAME` exists, as in the cutover steps (`mv -n` can exit 0 when it skips). Leave `archive/`, `trials/`, `lock`, `state.json` and everything else in place. The new session follows [Transitions](spec/journal.md#transitions) for any pending raw-day import: only eligible observations identified by its durable marker may be imported, never old tuning decisions.
3. Check with `sudo togi status`: it now fails with a missing-journal error naming `events.jsonl` (no `no session yet` message; that appears only for a journal that exists). That error only shows the file is gone; it proves nothing about what the file was. Keep the moved file as evidence.
4. Reboot and pick "NixOS - togi", or run `sudo togi run`. A new session starts. The kept file is evidence for a bug report and nothing reads it.

## 8. When the tuning boot does not reach togi

The recovery in [section 6](#6-dead-ends) runs in `togi.service` and `togi-restart-limit.service`, so it cannot help when the tuning boot fails before userspace: the persistent boot default still selects tuning. When the tuning boot does reach a console, tty1 shows `togi watch` and tty3 togi's log.

With systemd-boot, pick a normal generation in its menu, then run `sudo togi night-end` to restore the remembered selection. If that fails, use `sudo bootctl set-default ""` to remove the persistent EFI override; if `loader.conf` uses `@saved`, also choose the normal entry with the menu's default-selection key so its saved choice no longer points to tuning. Inspect `bootctl status` before rebooting. Rescue media booted in EFI mode can change these same firmware variables; do not replace the default with a fixed generation as routine recovery.

With GRUB:

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

1. Run `sudo togi night-end` before disabling the module to give up the tuning selection. With GRUB, verify `sudo grub-editenv <grubenv> list` no longer names tuning in `saved_entry`; otherwise unset it. With systemd-boot, inspect `bootctl status` and remove an unwanted override with `sudo bootctl set-default ""`.
2. Set `services.togi.tuning.enable = false`, or remove the module and the `togi` input, and rebuild.
3. If you entered togi's offsets in BIOS and want them gone, set Curve Optimizer back to 0.

Removing togi leaves `/var/lib/togi` in place. A later install can resume a compatible session or perform the [fresh transition](spec/journal.md#transitions); keeping the directory does not promise tuning-state carry. Archiving or deleting it is a separate choice.
