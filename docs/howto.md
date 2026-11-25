# How to tune a machine with togi

For a Zen 5 (Granite Ridge) desktop running NixOS with GRUB. Every command below runs on that machine.

## 1. Install the module

Add the `main` branch as a flake input. Your lock file pins the revision you use:

```nix
# flake.nix
{
  inputs.togi = {
    url = "github:shgew/togi/main";
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

Keep your existing `nixpkgs` input and host name. `specialArgs` is what gives the host's configuration its `inputs` argument; a flake that already passes its inputs another way keeps doing that. See the [pre-1.0 update policy](../AGENTS.md#keeping-docs-current).

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

When you choose to update, run `nix flake update togi` and rebuild. For an existing Ruleset 13 journal, see [the Ruleset 14 transition](#7-after-a-breaking-update).

The module installs `inputs.togi.packages.${pkgs.stdenv.hostPlatform.system}.default`, the `togi` built with crane. Moving a host from the earlier Go build of togi takes the steps in [Go to Rust cutover](#go-to-rust-cutover). From a checkout, `nix build .#default` builds the package, `just togi ARGS` runs the binary from the tree and `just check-one package` checks its version and completion scripts.

mprime and y-cruncher come from the host's `nixpkgs`. Updating one of them, for example with `nix flake update nixpkgs`, can cost that backend's passes: [Evidence](spec/tuner.md#evidence) says when a pass still counts. Update the backends when the extra trials are worth it, not as routine.

### Host lock

The NixOS module creates one root-only [host lock](spec/runtime.md#host-lock), `/run/lock/togi.lock`, before the tuning and day services start. `run`, `reset` and the root-only [hardware tests](development.md#rust-test-execution) share it. A busy lock exits 3. Never delete the lock to unblock it: another process can still hold the old inode, allowing two owners.

Use `sudo` to reset on Linux. On macOS, `reset` of a copied journal can run without root using its [separate private lock](spec/runtime.md#host-lock); hardware tuning remains Linux-only.

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

It makes the checks `sudo togi run` makes before tuning ([Preflight](spec/runtime.md#preflight)) and prints one row per check: `ok`, `FAIL`, `warn` or `skipped`. The last line is `ready`, or `not ready:` with the failed checks; fix those first. A `FAIL` on `watchdog` means no hardware watchdog demonstrates an armed, counting timer in this boot; manual runs refuse too. A missing, unreadable or static counter fails; a PID 1 refresh needs a later decrease. A `warn` on `bios_context` means the recorded BIOS context changed: the next run records the new context in the same session and follows [firmware invalidation](spec/tuner.md#firmware), rather than archiving it; tuning there needs a new stock attestation from `sudo togi night` in a normal boot. `doctor` appends nothing to the journal and changes no offset; [Commands](spec/runtime.md#commands) describes what it does without `sudo`.

## 3. A quick in-session test

Start sessions from the [tuning boot](#4-overnight-the-tuning-boot), especially after a breaking update: it has hardware-watchdog reset protection. An in-session run is an optional way to watch a few trials on the desktop, not the recommended way to start a session. Either way, first give the stock attestation in a normal boot with `sudo togi night`, or with `n` on `sudo togi watch`, which asks the same confirmation; without a current one `sudo togi run` ends before any trial with `stock_unattested` and exit 12 ([Night and day](spec/runtime.md#night-and-day)).

```sh
sudo togi run
```

`run` prints one line per journal event to stderr. To see the dashboard, run `togi watch` in another terminal; it follows the journal independently, shows the running trial and offsets, and `?` explains the screen. Press Ctrl-C in the **run** terminal to stop; a clean stop verifies stock and exits 0, and failed cleanup is reported ([Exit codes](spec/runtime.md#exit-codes)). Quitting `watch` does not stop the run.

```sh
togi status
```

An in-session run requires an armed, counting hardware watchdog, just like the tuning boot; `sudo togi run` checks once at startup and refuses if it is unarmed. Sampling countdown progress can take up to four seconds per candidate device; manual startup does not retry for readiness or arm a watchdog. It checks again before each nonzero profile application and workload start; loss detected there stops new work and attempts to restore and verify stock. A stop signal interrupts sampling and takes ordinary cleanup ([Preflight](spec/runtime.md#preflight)). `togi events --kind preflight.check` shows a failed `watchdog` result. After a reset or crash reboot, the next run uses the journal to recover and record the available [crash evidence](spec/tuner.md#crashes).

If a previous run left nonzero offsets on the same boot, `run` attempts to restore and verify CO 0 even if the watchdog is unarmed. It still refuses to tune; a failed restoration reports that stock was not verified ([Same-boot resume](spec/runtime.md#same-boot-resume)).

## Daily observation

After a session exists, leave firmware Curve Optimizer at CO 0 and arm PID 1's hardware watchdog.
`sudo togi day` refuses a nonzero all-core baseline or an unarmed/non-counting watchdog, then applies
the exact permitted committed D and observes normal use until Ctrl-C or a stop signal. It never
starts test workloads or makes a second tuning decision
([day transitions](spec/runtime.md#ruleset-13-policy-boundary)). It reads BIOS context and every core
at start and checkpoints active hours excluding suspend every ten minutes. An orderly shutdown,
including a short power-button shutdown handled by systemd, zeroes and verifies every core before
its final checkpoint and clean-stop marker. A later start records an earlier observed boot without
that marker as unclean, with its checkpoint, offsets, reset reason and available kernel evidence;
unavailable evidence stays unknown.

Unlike `run`, a `sudo togi day` started by hand that refuses a nonzero baseline does not restore
stock: it exits and leaves those offsets applied ([#1632](https://github.com/shgew/togi/issues/1632)).
Reboot, or run `sudo togi emergency-stop` with no other togi process running; only a successful
exit means every core was zeroed and read back at 0, and a reported failure leaves stock unverified.
The installed day service attempts the same zero-only recovery through its post-stop step after
such a refusal.

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

Every pause zeroes every core and verifies readback, keeping the learned observations and saved D. An untimed pause survives a service restart and sleep, but ends at the next cold boot. A timed pause keeps its journaled deadline across all three. Five minutes before expiry, the desktop notifier offers to extend the current deadline by one hour; dismissing it changes nothing. Ending a pause, early or at its deadline, reapplies the still-permitted committed D through the full transaction and announces only verified success. This may restore nonzero offsets, never promote or bypass a safety retreat or firmware reset.

On the main `togi watch` view, press `p` for pause/resume or `f` for freeze/unfreeze; help (`?`) describes the keys. Freeze is not pause: it retains D and prevents climbing, never safety retreats or a firmware reset. Paused hours do not earn a hold, and a cold boot that starts paused applies stock and cannot promote later in that boot, even after resume or expiry. [Day transitions](spec/runtime.md#day-pause-and-freeze) own the exact lifecycle.


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

`togi day notify` sends a desktop notification for each pending question using the installed `notify-send` (from libnotify; togi adds no notification library). On KDE Plasma (`XDG_CURRENT_DESKTOP` containing `KDE`) the notification has the four answers as buttons and waits up to two minutes for one. On other desktops it is a plain notification naming the command, because togi does not query what a notification server supports; answer with `togi day answer`. Dismissing a notification answers nothing, and the question stays in `status` and `watch`. The module's `togi-day-notify` user service runs it for each user in `services.togi.day.users`; elsewhere run `togi day notify` in your graphical session yourself.

### Recording a crash togi did not observe

When the day service starts, it records an unclean end for each earlier boot in which its last start was not followed by a clean stop. A crash in daily use in a boot where the day service never started, or where its last start had already ended in a clean stop, is not recorded and is not tuning evidence; there is no manual entry to add it. Existing manual records remain readable and still count as [tuning evidence](spec/tuner.md#manual-crash-records).

### Running the day service

Daily observation requires an existing compatible session journal. It refuses to create a session or transition one written by another ruleset, exits 16 and, unlike other failures, is not restarted by the module's unit. Start a session with `sudo togi night` and the tuning boot first. Stop daily observation before `togi run` or `togi reset` in a normal boot: they share the host and writer locks. `togi status`, `togi events` and `togi watch` remain lock-free while observation runs.

To run it automatically in normal boots, enable the NixOS module option after creating a session:

```nix
services.togi = {
  enable = true;
  day.enable = true;
  day.users = [ "alice" ]; # may answer and control pause/freeze without root; replace with your user
};
```

The `togi-day.service` unit is absent from the tuning specialisation. Enabling it keeps journald persistent with a five-second sync interval. Before a manual `run` or `reset` in a normal boot, use `systemctl stop togi-day.service`; stop zeroes and verifies before recording a clean marker. Start it again with `systemctl start togi-day.service` afterward. A same-boot restart, including `nixos-rebuild switch`, reapplies the still-permitted committed D without promoting. Systemd-managed sleep restores the watchdog before the resume hook requests the same profile transaction with all-core readbacks. An abnormal exit runs zero-only emergency recovery before the delayed restart, except exit 16 (an incompatible journal, such as the previous ruleset's after an update): the unit stays failed because a restart would refuse again. After the journal has been transitioned, start it with `systemctl restart togi-day.service` or let the next boot do so. Inspect diagnostics with `journalctl -u togi-day.service`. Without a session, verified zero baseline or armed/counting watchdog, the service reports why it cannot start.

The first eligible live nonzero step supplies [hardware acceptance](spec/tuner-ruleset14.md#9-safety-and-activation). Keep the journal and record the exact revision, configuration and observations. A [retrospective](reviewing.md#retros) is optional. Do not bypass qualification, holds or safety blockers to obtain a step.

Enabling `services.togi.day.enable` loads `sp5100_tco` and asks PID 1 to arm and feed a 60-second hardware watchdog in normal boots. The watchdog can reset the machine if feeding stops; it does not catch every desktop freeze or establish why a boot ended. The watchdog and sleep hooks are part of day enablement, not a separate option. Remove `services.togi.day.watchdog` from existing configurations. Day application refuses unless the watchdog is armed and counting.

A hardware watchdog may keep counting during s2idle, which has not been measured, so every day setup includes the pre-sleep pause in `togi-day-watchdog-sleep.service`. SP5100 TCO loses timer MMIO decoding after deep sleep on the measured platform: it can still report `active` while `timeleft` reads `4294967295`, providing no reset protection. After every systemd-managed resume the unit unloads and reloads `sp5100_tco` before re-arming PID 1's watchdog. Module failures are logged and do not skip the re-arm request. The observer rechecks at sampling ticks and checkpoints, recording changes immediately; `togi status` reports its last check, not merely its startup check. Check `/sys/class/watchdog/watchdog0/timeleft` several seconds apart before and after a deep sleep longer than 90 seconds: readings must be at most `timeout` and count down between PID 1's feeds, and status must say `armed`. A missing, unreadable, malformed or static counter, or a value greater than `timeout`, is not armed. An upward refresh by PID 1 needs a subsequent decrease; the refresh alone does not prove counting. A sleep started by writing `/sys/power/state` directly bypasses this unit, and `CONFIG_WATCHDOG_NOWAYOUT` or the driver's `nowayout` parameter can prevent release and reload. [Day watchdog](spec/runtime.md#day-watchdog) owns the rules and limits. A reset whose crashed boot ended at a sleep entry remains sleep-related, not evidence of a CPU hang.

### Deliberately letting the watchdog fire

This is a **destructive reset test**, not a routine health check or a stability test. It deliberately panics the kernel while the day service is running. Unsaved data will be lost; `sync` reduces pending writes but cannot make an abrupt reset safe. Save work, stop unrelated workloads, keep backups, and have local access to reset or power-cycle the machine if the watchdog fails. Run only in a normal boot, with BIOS Curve Optimizer at **0 on every core**, an existing compatible session, and `services.togi.day.enable` set as above. Do not test with an offset profile you want to qualify: this creates real, permanent crash evidence in that session.

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

Enable `services.togi.tuning.enable = true;` with GRUB and rebuild. If you
are switching from systemd-boot, first boot the normal system and restore
the previous boot selection with `sudo togi night-end` using the version
that started the night. The GRUB-only version does not change old EFI
selections or read their saved state.

Switch from the day with `sudo togi night`. It shows the plan, then its existing reboot confirmation
also asks you to confirm that the machine is stable at CO 0 with BIOS at
stock. Only `y` or `yes` goes on: togi journals that attestation once per
firmware context, selects the current generation's tuning entry, remembers
the boot selection it replaces and reboots. A change in the recorded BIOS
context, which includes BIOS version and microcode but no separate AGESA
version, requires an attestation for that context unless this session already
has one. Any other answer changes nothing.
In a normal boot, `sudo togi watch` offers the same switch on `n`, with the
same stock-stability confirmation and no second prompt.

The tuning entry boots to a console with no desktop. tty1 shows `togi watch --tuning-boot`, a dashboard that ignores `q`, `Q`, Ctrl-C and Ctrl-D; `systemctl stop togi-watch.service` still ends it. Its log and the kernel's messages are on tty3 (Alt+F3); tty2 has a login prompt. `togi.service` tunes unattended. A night begun with `togi night` has no time limit, and crash reboots stay in tuning.

End the night with `F10` on the tuning dashboard and answer `y` to `Reboot into the day cycle?`, which stops the run cleanly first, or use the power button, `poweroff` or `reboot`. The tuner also ends it at a dead end or its restart limit. To keep tuning across your own reboots instead, set `services.togi.tuning.leaveOnShutdown = false;`. Automatic restart-limit retry reboots keep the tuning entry even with the option on ([retry policy](spec/runtime.md#restart-limit-retry-boots-and-leave-reasons)).

Ending restores the previous GRUB selection exactly. If the day had no saved entry, GRUB uses its default; togi never chooses a normal generation. If `togi restart-limit` or the shutdown hook's `togi night-end` itself fails, the module's fallback only unsets `saved_entry`, so the next boot is GRUB's default rather than an earlier saved day entry ([retry policy](spec/runtime.md#restart-limit-retry-boots-and-leave-reasons)).

You can also pick the tuning specialisation in the GRUB menu, which remembers that choice automatically. A menu-picked tuning boot runs no stock confirmation of its own: without a current attestation from an earlier `togi night` in a normal boot, it ends before any trial with `stock_unattested` (exit 12); a reset, a ruleset transition or a firmware change needs a new answer first ([Night and day](spec/runtime.md#night-and-day)). The module installs the GRUB tools and writes `/etc/togi/night.json`.

The tuning display uses the Linux console's 16-colour profile and built-in font, which covers the dashboard's block and box glyphs even when the normal system uses another font. It needs no GPU terminal, font setup or display option. Alt+F2 opens the login prompt, Alt+F3 shows logs, and Alt+F1 returns to the dashboard. Restarting the watcher does not restart tuning.

The tuning boot loads `sp5100_tco` in the initrd and lets systemd arm and feed the hardware watchdog. Every run requires an armed, counting hardware watchdog; otherwise it stops with a preflight dead end ([Preflight](spec/runtime.md#preflight)). `togi events --kind preflight.check` shows the final `watchdog` result. The tuning boot retries readiness until 30 seconds have passed; a final check can take up to four more seconds per candidate device. A manual `sudo togi run` checks once without retrying for readiness; countdown sampling still takes time.

After an unclean tuning boot, `togi events --kind crash.detected,mce` shows the interrupted action, available reset reason and machine-check evidence ([Trial outcome](spec/workloads.md#trial-outcome)). togi does not read saved kernel dumps for crash classification.

See [what a night does](how-togi-tunes.md#what-a-night-does) for the queues and coverage, and [stock attestation](spec/tuner-ruleset14.md#operator-attested-stock-stability) for the rules on all-zero profiles.

## 5. In the morning

Shut down or reboot. The next boot follows your restored normal selection; with `leaveOnShutdown` off, use F10 to end the night or pick your normal entry in the boot menu. Then:

```sh
togi status
```

`status` shows each CCD's applied day offset `D`, hard ceiling `C`, tagged edge `U` and counts still to go.
The table shows exact-profile qualification and outstanding coverage.
Below it, Day policy shows the applied profile, hold, latest step and what blocks it, freeze, firmware, events and vetoes.
It also shows the tuning phase, any stop or finish reason and the remaining campaign allowance.
The Night section shows elapsed observed time and each queue's share used.
See [Commands](spec/runtime.md#commands) for display details and [How togi tunes](how-togi-tunes.md) for the rules.
Another tuning run continues the session.

For the evidence behind those labels:

```sh
togi status --evidence
sudo togi doctor --day
```

The evidence view shows exact-profile qualification evidence, applied-step history and comparable same-class night measurements.
Historical observations across contexts are shown separately, alongside stock totals and unknown intervals.
See [what the screens show](how-togi-tunes.md#what-the-screens-show) for how to read the experimental evidence. No view gives a safety verdict or an ETA.

The first observed day boot after a night has a Morning report in `status`; `togi day notify` also
sends it to the desktop. It summarizes recorded trial ends and observed task time, and explains that
the day service applies the permitted committed profile, with promotion only at an eligible cold
boot. It is reconstructed from the existing journal and retained across a day-service restart in
that boot, not repeated on the next day boot. Notification deduplication is per notifier process
lifetime: restarting the notifier may show it again. A notification is not proof of current hardware
offsets or stability.

`doctor --day` checks the day prerequisites rather than stress-workload availability. Its baseline row requires a current, complete, verified CO-0 readback recorded in this boot, using the day service's own stock-verification rule. The day service's start records that proof. An earlier-boot receipt, a receipt superseded by a write or failed/partial/nonzero readback, applied D alone or a live zero diagnostic reading cannot replace it. Missing proof refuses rather than being silently filled in. Like ordinary `doctor`, it writes no journal event and changes no offset; root hardware checks still require the host lock, so stop a competing hardware process first. See [day diagnostics](spec/runtime.md#day-diagnostics) for the exact checks.

A night has no end time. Stop a foreground `sudo togi run` with Ctrl-C in its terminal, or end the tuning boot with F10 in its dashboard or a reboot (the power button included). A clean stop restores every core to CO 0. A crash reboot into the tuning boot continues the same night; the next run after you end it opens a new one. A direct run
or menu-started tuning boot without the current stock attestation stops at
stock with `stock_unattested` (exit 12): give it through the normal-boot
`togi night` confirmation first ([Night and day](spec/runtime.md#night-and-day)).
An empty admissible nonzero queue stops with `no_nonzero_work`
(exit 12), not a stock trial. Excluded backends and runtime safety stops
retain their [named stopping conditions](spec/tuner.md#night-query-results).

Keep BIOS Curve Optimizer at **0**; never copy a proposed day profile into firmware. Keep backups: [night coverage](how-togi-tunes.md#what-a-night-does) and a [clean hold](spec/tuner-ruleset14.md#4-a-clean-hold-earns-one-step) do not establish stability in daily use.

### If the profile crashes in daily use

1. Reboot and answer the question about the unclean end (`togi day answer`, [Answering what happened](#answering-what-happened)). A CPU-suspect event vetoes that whole profile and every later day profile at or deeper than it on every CCD. It schedules a step back at the next cold boot and one reproduction night; [daily-event escalation](spec/tuner.md#whole-profile-veto-and-escalation) determines which steps retreat. A clean reproduction does not clear the veto; only evidence that reclassifies the event does.
2. Check the filesystem with the checks appropriate to that filesystem, using rescue media or an unmounted filesystem where required. Check the Nix package store's contents with `sudo nix-store --verify --check-contents`, and address any reported damage before returning to heavy or write-intensive work.
3. If the day service never started in that boot, or its last start there had already ended in a clean stop when the machine froze, togi has no record of the crash and does not count it ([Recording a crash togi did not observe](#recording-a-crash-togi-did-not-observe)); include the offsets that were applied when you report it.
4. [Report the crash](https://github.com/shgew/togi/issues) with what was running and when; keep the togi session and available system logs as evidence, and remove personal and machine-identifying details from anything you publish.

## 6. Dead ends

togi stops by itself at a [dead end](spec/tuner.md#dead-ends) when it cannot make progress. In the tuning boot it records the dead end, persists its leave reason in the boot loader's state, restores the day's selection, and shows the explanation on tty1 ([Dead-end actions](spec/runtime.md#dead-end-actions)).

If `togi.service` instead fails without a dead end or lock contention, systemd restarts it; repeated failures reach the start limit, which returns to the tuning boot twice and then to the normal system ([Restart-limit retry boots and leave reasons](spec/runtime.md#restart-limit-retry-boots-and-leave-reasons)). Lock contention exits 3 without an automatic restart: let the current owner finish, then start the service again.

After an abnormal service exit, inspect `journalctl -u togi.service` for the emergency cleanup and each core's zero readback before the restart ([Tuning boot](spec/runtime.md#tuning-boot)). To repeat that bounded zero-only cleanup after a crashed manual run, with no other togi process running, use `sudo togi emergency-stop`; a failure is reported instead of claiming that offsets were restored.

`togi status` shows the dead end, and the end of `togi events` shows it after
the evidence it cites. Fix the named cause before starting again. For
`stock_unattested`, return to the normal boot and confirm through
`sudo togi night`; do not bypass the answer with a direct run. For
`no_nonzero_work`, continue daily observation at stock rather than looking
for an all-zero workload to run. A change in the recorded BIOS context continues the
session under [firmware invalidation](spec/tuner.md#firmware), requiring a
new-context attestation rather than seeding a new session.
[runtime.md](spec/runtime.md#dead-end-actions) and
[tuner.md](spec/tuner.md#dead-ends) describe the stop conditions.

Leave reasons survive outside the tuning journal as one pending `togi_leave_reason` record in GRUB's environment block. Inspect it from the normal system with `sudo grub-editenv <grubenv> list` ([find the environment file](#8-when-the-tuning-boot-does-not-reach-togi)). Normal in-session runs and `status`/`events` do not import it. View it after the next tuning-boot run with `togi events --kind boot.leave_reason` ([retry state](spec/runtime.md#restart-limit-retry-boots-and-leave-reasons)).

## 7. After a breaking update

**The Go to Rust switch is the exception to everything below.** The Rust build neither archives nor imports from a Go session: move the Go state directory aside first, as [Go to Rust cutover](#go-to-rust-cutover) says.

Update configuration before the tuning boot; invalid configuration exits 2 before archiving. Rebuild, then in a normal boot run `sudo togi night` and answer its stock-stability confirmation again: the successor is a new session, and archived consent is not imported ([Night and day](spec/runtime.md#night-and-day)). Reboot into the tuning boot with its active watchdog. Going straight to the tuning boot, or running `sudo togi run`, ends before any trial with `stock_unattested` (exit 12). The Ruleset 14 cutover archives Ruleset 13 unchanged and starts the durably identified successor at verified stock; no manual reset is needed. With `services.togi.day.enable`, `togi-day.service` refuses the old journal (exit 16) and stays failed instead of restarting every minute; once the journal has been transitioned, run `sudo systemctl restart togi-day.service` or reboot into a normal generation ([day service](#3-a-quick-in-session-test)).

It imports original-identity raw daily observations, compatible `exposure.observed`, and CPU-suspect events, stops and vetoes needed to reconstruct the campaign. Inspect commitments with `togi events --kind successor.imported` and `togi events --kind campaign.start`. Missing/unreconstructible campaign history blocks nonzero exploration; old S, qualification conclusions and nonzero D are never permission.

The two-event machine allowance includes provisional charges and is never renewed by restart, reset, firmware change, new session or reboot. Later non-CPU reclassification is displayed separately without refund. `sudo togi reset --all` stops raw tuning carry at a permanent boundary, but campaign-ledger records cross it. There is no per-core reset. A reset starts a new session, so answer `sudo togi night`'s stock-stability confirmation again before tuning; the old answer is not carried over.

A firmware change remains in-session: it returns to stock and requires new-context exact qualification and 50 clean stock hours before guarded restoration of the previous full profile. It never reopens a stopped unit.

For rollback, an older build refuses to resume a newer-ruleset journal before append. Same-schema inspection remains possible with warnings/skipped unknown kinds, but strict reset may refuse unknown additive kinds; reinstall the writing build to archive it safely. [Reset and compatibility](spec/runtime.md#reset-and-compatibility) describes the exact operation-specific behavior.

## Go to Rust cutover

This step is for the one-time switch from the Go build to the Rust build, and it is the exception to [section 7](#7-after-a-breaking-update): the Rust build does not archive, import from or read a Go session. A Go journal in `/var/lib/togi` is another build's file, not an eligible Rust observation source. Every Rust `togi run`, `status`, `events` and `watch` whose state directory has a first journal line that is not a complete header of this build exits 1 with `<state dir>/events.jsonl: first line is not a complete header of this build; either another build's journal or a header torn by a crash; see docs/howto.md#go-to-rust-cutover`, reads no further, takes no lock and changes no file in the state directory ([Commands](spec/runtime.md#commands)). The message does not say the journal is Go's, because it cannot know: a Go journal and a Rust header torn by a crash before its first newline look the same. Rebooting into the tuning boot does not help, and `togi reset --all` is not the way out. Find which case you have with the check below, then do the matching procedure.

Which one is it? You cannot be certain from the file alone, so treat what you see as clues. Run `sudo head -c 300 /var/lib/togi/events.jsonl; echo` and look at the shape: a Go journal's first line is a flat object (no `"data"` and a `"build"` beside `"seq"`), a Rust header is `{"seq":1,...,"kind":"session.start",...,"data":{...` with the build nested under `"data"`. Look also at `/var/lib/togi/archive/` and at what the machine ran before: a state directory that Go's build wrote has Go's history. Neither line count nor a `state.json` decides it: a Go first line can lack its newline, a crash can leave a Rust header partly written before a later newline, and the Go build and earlier Rust builds both left a `state.json` (the current build neither writes nor reads one). If the line is plainly Go's, or you are not sure, take the cutover below, which preserves the whole directory and starts a fresh session. Use [the torn first header procedure](#a-torn-first-header-of-the-rust-build) only when the line is visibly a cut-off Rust `session.start`.

Cut over, as root, before the first Rust boot:

1. Stop the Go build's tuning completely: `sudo systemctl stop togi.service togi-watch.service`, and stop any `togi run` or `togi watch` you started by hand in a terminal (Ctrl-C). Check that `pgrep -a -x togi` lists nothing: it also matches the watch service, so stop whatever it lists. Do not boot "NixOS - togi" again until step 5. A Go `togi run` still going keeps writing to the directory you are about to move.
2. Check that the backup name is free, counting a dangling symlink as taken: `sudo sh -c 'test ! -e /var/lib/togi-go && test ! -L /var/lib/togi-go && echo free'`. If it prints nothing, something already sits there, perhaps a backup of an earlier attempt. Never overwrite it: choose another name for the backup and use that name everywhere below, or stop and look at what is there.
3. Move the Go state aside without overwriting: `sudo mv -nT /var/lib/togi /var/lib/togi-go`. `-n` refuses to replace an existing destination, but `mv` can exit 0 when it skips, so its status proves nothing. Confirm that the source is gone and the destination exists: `sudo sh -c 'test -d /var/lib/togi-go && test ! -e /var/lib/togi && test ! -L /var/lib/togi && echo moved'`. If it does not print `moved`, stop: nothing was overwritten, but the cutover has not happened. Nothing in the directory is deleted, edited or read by the Rust build, and the Go build's session, archives and `state.json` stay there as they were.
4. Rebuild with the Rust package: update the `togi` input to the cutover merge commit or later, where `default` is the Rust build ([section 1](#1-install-the-module)), and leave `services.togi.package` at its default.
5. Reboot into a normal generation, not "NixOS - togi", and run `sudo togi night`: the new session has no stock attestation, and a tuning boot without one ends before any trial with `stock_unattested` (exit 12). After you answer its stock-stability confirmation the machine reboots into "NixOS - togi". The Rust build finds no Go journal, starts a fresh session without importing Go observations ([Transitions](spec/journal.md#transitions)) and nothing converts the Go journal, so the Go session's progress is not in the new one. See [Night and day](spec/runtime.md#night-and-day).

The only state directory is `/var/lib/togi`, used by one build. The moved Go directory is a plain directory under `/var/lib`, never a second state directory: no `--state-dir` and no `-rs` suffix. There is no rollback to the Go build; the directory stays at the backup path chosen above for reference.

### A torn first header of the Rust build

A crash while the very first journal line was being written can leave `events.jsonl` holding part of the Rust `session.start`. The Rust build cannot tell that from another build's journal, so it refuses it and repairs nothing automatically. A first line that never completed recorded no session, so no tuning progress should be in the file; the check in the previous section is why you believe it is this case, and if you are unsure, use the cutover steps instead. Do not edit the file, and never type a header by hand: a made-up `session.start` would carry a build stamp, boot and `seq` that nothing wrote.

1. Stop every writer and reader: `sudo systemctl stop togi.service togi-watch.service` and any `togi run` or `togi watch` started by hand. Check that `pgrep -a -x togi` lists nothing. Do not boot "NixOS - togi" again until step 4.
2. Keep the file as it is, moved out of the way without overwriting: pick a free name such as `/var/lib/togi/events.jsonl.torn-20261012`, check it with `sudo sh -c 'test ! -e NAME && test ! -L NAME && echo free'`, then `sudo mv -n /var/lib/togi/events.jsonl NAME` and confirm that `events.jsonl` is gone and `NAME` exists, as in the cutover steps (`mv -n` can exit 0 when it skips). Leave `archive/`, `trials/`, `lock`, `state.json` and everything else in place. The new session follows [Transitions](spec/journal.md#transitions) for any pending raw-day import: only eligible observations identified by its durable marker may be imported, never old tuning decisions.
3. Check with `sudo togi status`: it now fails with a missing-journal error naming `events.jsonl` (no `no session yet` message; that appears only for a journal that exists). That error only shows the file is gone; it proves nothing about what the file was. Keep the moved file as evidence.
4. Reboot into a normal generation and run `sudo togi night`; after you answer its stock-stability confirmation it reboots into "NixOS - togi" and a new session starts. Picking that entry in the menu first, or running `sudo togi run`, gives the new session no attestation and ends before any trial with `stock_unattested` (exit 12) ([Night and day](spec/runtime.md#night-and-day)). The kept file is evidence for a bug report and nothing reads it.

## 8. When the tuning boot does not reach togi

The recovery in [section 6](#6-dead-ends) runs in `togi.service` and `togi-restart-limit.service`, so it cannot help when the tuning boot fails before userspace: the persistent boot default still selects tuning. When the tuning boot does reach a console, tty1 shows `togi watch` and tty3 togi's log.

The module requires exactly one `boot.loader.grub.mirroredBoots` entry. `<grubenv>` below stands for `<path>/grub/grubenv`, where `<path>` is that entry's `path`. On an ordinary install it is `/boot/grub/grubenv`.

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

1. Run `sudo togi night-end` before disabling the module to give up the tuning selection. Verify `sudo grub-editenv <grubenv> list` no longer names tuning in `saved_entry`; otherwise unset it.
2. Set `services.togi.tuning.enable = false`, or remove the module and the `togi` input, and rebuild.
3. If you entered togi's offsets in BIOS and want them gone, set Curve Optimizer back to 0.

Removing togi leaves `/var/lib/togi` in place. A later install can resume a compatible session or perform the [fresh transition](spec/journal.md#transitions); keeping the directory does not promise tuning-state carry. Archiving or deleting it is a separate choice.
