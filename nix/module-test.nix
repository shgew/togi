# Evaluates the x86_64-linux NixOS configuration on every host, VM-free: GRUB and its mount
# dependencies, GRUB mirror constraints, package overrides, the tuning boot's 30s watchdog and day sleep hooks.
{
  pkgs,
  hostPkgs,
  package,
}:
let
  inherit (pkgs) lib;
  module = import ./module.nix { packages.${pkgs.stdenv.hostPlatform.system}.default = package; };
  evaluate =
    mirrors: selectedPackage: hardwareTestGroup:
    evaluateDay mirrors selectedPackage hardwareTestGroup { };
  evaluateDay =
    mirrors: selectedPackage: hardwareTestGroup: day:
    evaluateLoader mirrors selectedPackage hardwareTestGroup day { };
  evaluateLoader =
    mirrors: selectedPackage: hardwareTestGroup: day: loader:
    (import (pkgs.path + "/nixos/lib/eval-config.nix") {
      inherit pkgs;
      system = pkgs.stdenv.hostPlatform.system;
      modules = [
        # Override only this module's JSON producer, without rebuilding the Linux package set.
        (
          {
            config,
            lib,
            pkgs,
            ...
          }:
          module {
            inherit config lib;
            pkgs = pkgs // {
              formats = hostPkgs.formats;
            };
          }
        )
        {
          system.stateVersion = "26.05";
          boot.loader = lib.recursiveUpdate {
            grub = {
              enable = true;
              mirroredBoots = mirrors;
            };
          } loader;
          fileSystems = {
            "/" = {
              device = "/dev/vda1";
              fsType = "ext4";
            };
            "/boot" = {
              device = "/dev/vda2";
              fsType = "ext4";
              options = [ "noauto" ];
            };
          };
          services.togi = {
            inherit day;
            enable = true;
            package = lib.mkIf (selectedPackage != null) selectedPackage;
            inherit hardwareTestGroup;
            tuning.enable = true;
            settings = {
              dead_ends.inconclusive_in_a_row = 4;
            };
          };
          hardware.cpu.amd.ryzen-smu.enable = false;
          users.groups = {
            svc = { };
          }
          // lib.optionalAttrs (hardwareTestGroup != null) {
            ${hardwareTestGroup} = { };
          };
          users.users = {
            alice.isNormalUser = true;
            bob.isNormalUser = true;
            svc = {
              isSystemUser = true;
              group = "svc";
            };
          };
        }
      ];
    }).config;
  mirror = path: {
    inherit path;
    devices = [ "nodev" ];
  };
  zero = evaluate [ ] null null;
  one = evaluate [ (mirror "/boot") ] null null;
  two = evaluate [
    (mirror "/boot")
    (mirror "/boot2")
  ] null null;
  systemdLoader = {
    grub.enable = false;
    systemd-boot.enable = true;
  };
  systemd = evaluateLoader [ ] null null { } systemdLoader;
  bothLoaders = evaluateLoader [ (mirror "/boot") ] null null { } {
    systemd-boot.enable = true;
  };
  neitherLoader = evaluateLoader [ ] null null { } {
    grub.enable = false;
  };
  overridden = evaluate [ (mirror "/boot") ] pkgs.hello null;
  delegated = evaluate [ (mirror "/boot") ] null "togi-hardware";
  overriddenTuning = overridden.specialisation.togi.configuration;
  day = evaluateDay [ (mirror "/boot") ] null null { enable = true; };
  dayTuning = day.specialisation.togi.configuration;
  dayUsers = evaluateDay [ (mirror "/boot") ] null null {
    enable = true;
    users = [ "alice" ];
  };
  dayUsersTuning = dayUsers.specialisation.togi.configuration;
  dayTwoUsers = evaluateDay [ (mirror "/boot") ] null null {
    enable = true;
    users = [
      "alice"
      "bob"
    ];
  };
  daySystemUser = evaluateDay [ (mirror "/boot") ] null null {
    enable = true;
    users = [ "svc" ];
  };
  lockupPanics =
    config:
    lib.filter (p: lib.hasInfix "lockup" p || lib.hasInfix "nmi_watchdog" p) config.boot.kernelParams
    ++ lib.filter (k: lib.hasInfix "lockup" k || lib.hasInfix "nmi_watchdog" k) (
      builtins.attrNames config.boot.kernel.sysctl
    );
  rejectsTuning =
    config:
    lib.any (
      a: !a.assertion && lib.hasPrefix "services.togi.tuning.enable" a.message
    ) config.assertions;
  tuning = one.specialisation.togi.configuration;
  consoleWatch = tuning.systemd.services.togi-watch;
  restartLimitScript = tuning.systemd.services.togi-restart-limit.script;
  leaveTuningBoot = tuning.systemd.services.togi-leave-tuning-boot;
  togiService = tuning.systemd.services.togi;
  # ExecStopPost is the module's postStop script: the shipped package's zero-only emergency path, gated on an
  # abnormal result.
  stopHook = togiService.postStop;
  resumeScript = builtins.unsafeDiscardStringContext day.systemd.services.togi-day-resume.serviceConfig.ExecStopPost.text;
  generatedConfig = one.environment.etc."togi/config.json";
  nightSetup = one.environment.etc."togi/night.json";
  expectedNight = hostPkgs.writeText "togi-module-night.json" (
    builtins.toJSON {
      loader = "grub";
      grubenv = "/boot/grub/grubenv";
      entry = "NixOS - togi";
    }
  );
  expectedSettings = hostPkgs.writeText "togi-module-settings.json" (
    builtins.toJSON one.services.togi.settings
  );
in
assert rejectsTuning zero;
assert lib.all (a: a.assertion) one.assertions;
assert rejectsTuning two;
assert rejectsTuning bothLoaders;
assert rejectsTuning neitherLoader;
assert rejectsTuning systemd;
assert generatedConfig.target == "togi/config.json";
assert one.services.togi.settings.backend_user == "togi-trial";
assert builtins.elem "noauto" tuning.fileSystems."/boot".options;
assert tuning.systemd.services.togi.unitConfig.RequiresMountsFor == "/boot/grub/grubenv";
assert lib.hasInfix "RequiresMountsFor=/boot/grub/grubenv" tuning.systemd.units."togi.service".text;
assert !(builtins.elem "systemd-pstore.service" tuning.systemd.services.togi.after);
assert !(builtins.elem "printk.always_kmsg_dump=1" tuning.boot.kernelParams);
assert !(builtins.elem "printk.always_kmsg_dump=1" one.boot.kernelParams);
assert tuning.systemd.settings.Manager.RuntimeWatchdogSec == "30s";
assert
  tuning.systemd.services.togi-restart-limit.unitConfig.RequiresMountsFor == "/boot/grub/grubenv";
assert leaveTuningBoot.unitConfig.RequiresMountsFor == "/boot/grub/grubenv";
# The day observer is off by default, a normal-boot service only, and keeps the journal on disk.
assert !one.services.togi.day.enable;
assert !(one.systemd.services ? togi-day);
assert !(one.systemd.units ? "togi-day.service");
assert !(one.services.journald.settings.Journal ? SyncIntervalSec);
assert !(tuning.systemd.services ? togi-day);
assert day.services.togi.day.enable;
assert !dayTuning.services.togi.day.enable;
assert
  day.systemd.services.togi-day.serviceConfig.ExecStart
  == "${lib.getExe package} day --state-dir /var/lib/togi";
assert day.systemd.services.togi-day.serviceConfig.StateDirectory == "togi";
assert day.systemd.services.togi-day.serviceConfig.Restart == "on-failure";
assert day.systemd.services.togi-day.serviceConfig.RestartPreventExitStatus == "16";
assert day.systemd.services.togi-day.wantedBy == [ "multi-user.target" ];
assert builtins.elem "systemd-modules-load.service" day.systemd.services.togi-day.after;
assert day.systemd.units ? "togi-day.service";
assert !(dayTuning.systemd.services ? togi-day);
assert !(dayTuning.systemd.units ? "togi-day.service");
assert lib.hasInfix "ExecStopPost=" day.systemd.units."togi-day.service".text;
assert lib.hasInfix ''[ "''${SERVICE_RESULT:-}" != success ] || exit 0''
  day.systemd.services.togi-day.postStop;
assert lib.hasInfix
  (builtins.unsafeDiscardStringContext "exec ${lib.getExe package} emergency-stop")
  day.systemd.services.togi-day.postStop;
assert day.systemd.services.togi-day-resume.wantedBy == [ "sleep.target" ];
assert
  day.systemd.services.togi-day-resume.before == [
    "sleep.target"
    "togi-day-watchdog-sleep.service"
  ];
assert day.systemd.services.togi-day-resume.unitConfig.StopWhenUnneeded;
assert day.systemd.services.togi-day-resume.serviceConfig.Type == "oneshot";
assert day.systemd.services.togi-day-resume.serviceConfig.RemainAfterExit;
assert lib.hasInfix "ExecStopPost=" day.systemd.units."togi-day-resume.service".text;
assert !(one.systemd.services ? togi-day-resume);
assert !(dayTuning.systemd.services ? togi-day-resume);
assert dayTuning.systemd.services ? togi;
assert day.services.journald.settings.Journal.Storage == "persistent";
assert day.services.journald.settings.Journal.SyncIntervalSec == "5s";
assert dayTuning.services.journald.settings.Journal.SyncIntervalSec == "1s";
# Answering without root: day.users joins the togi group, owns /run/togi 0750 and gets a notifier only in its
# own graphical sessions. Nothing of it exists without the day observer or in the tuning boot.
assert one.services.togi.day.users == [ ];
assert !(one.users.groups ? togi);
assert !(builtins.elem "d /run/togi 0750 root togi -" one.systemd.tmpfiles.rules);
assert builtins.elem "d /run/togi 0750 root togi -" day.systemd.tmpfiles.rules;
assert builtins.elem "d /run/togi 0750 root togi -" dayUsers.systemd.tmpfiles.rules;
assert !(builtins.elem "d /run/togi 0750 root togi -" dayUsersTuning.systemd.tmpfiles.rules);
assert day.users.groups ? togi;
assert !(dayUsersTuning.users.groups ? togi);
assert (day.systemd.services.togi-day.serviceConfig.Group or "root") == "root";
assert !(day.systemd.services.togi-day.serviceConfig ? RuntimeDirectory);
assert dayUsers.users.users.alice.extraGroups == [ "togi" ];
assert dayUsers.users.users.bob.extraGroups == [ ];
assert dayTwoUsers.users.users.bob.extraGroups == [ "togi" ];
assert day.users.users.alice.extraGroups == [ ];
assert dayUsersTuning.users.users.alice.extraGroups == [ ];
assert lib.all (a: a.assertion) dayTwoUsers.assertions;
# Account type is NixOS's to validate: a system user listed here joins the group and gets the notifier too.
assert daySystemUser.users.users.svc.extraGroups == [ "togi" ];
assert lib.all (a: a.assertion) daySystemUser.assertions;
assert daySystemUser.systemd.user.services ? togi-day-notify;
assert dayUsers.systemd.user.services.togi-day-notify.bindsTo == [ "graphical-session.target" ];
assert !(day.systemd.user.services ? togi-day-notify);
assert !(dayUsersTuning.systemd.user.services ? togi-day-notify);
assert dayUsers.systemd.user.units ? "togi-day-notify.service";
assert dayUsers.systemd.user.services.togi-day-notify.wantedBy == [ "graphical-session.target" ];
assert dayUsers.systemd.user.services.togi-day-notify.partOf == [ "graphical-session.target" ];
assert dayUsers.systemd.user.services.togi-day-notify.after == [ "graphical-session-pre.target" ];
assert
  dayUsers.systemd.user.services.togi-day-notify.serviceConfig.ExecStart
  == "${lib.getExe package} day notify";
assert dayUsers.systemd.user.services.togi-day-notify.serviceConfig.Restart == "on-failure";
assert dayUsers.systemd.user.services.togi-day-notify.startLimitIntervalSec == 0;
assert builtins.elem pkgs.libnotify dayUsers.systemd.user.services.togi-day-notify.path;
assert
  dayUsers.systemd.user.services.togi-day-notify.serviceConfig.ExecCondition
  != dayTwoUsers.systemd.user.services.togi-day-notify.serviceConfig.ExecCondition;
# Day enablement alone installs the normal boot's 60s watchdog without lockup panics.
assert day.systemd.settings.Manager.RuntimeWatchdogSec == "60s";
assert builtins.elem "sp5100_tco" day.boot.initrd.kernelModules;
assert !(one.systemd.settings.Manager ? RuntimeWatchdogSec);
assert !(builtins.elem "sp5100_tco" one.boot.initrd.kernelModules);
assert lockupPanics day == [ ];
assert dayTuning.systemd.settings.Manager.RuntimeWatchdogSec == "30s";
# Around sleep the watchdog is stopped and restarted through the manager's RuntimeWatchdogUSec property: 0 disarms
# it, infinity restores the configured 60s. Without day enablement, and in the tuning boot, there is no such unit.
assert day.systemd.services.togi-day-watchdog-sleep.wantedBy == [ "sleep.target" ];
assert day.systemd.services.togi-day-watchdog-sleep.before == [ "sleep.target" ];
assert day.systemd.services.togi-day-watchdog-sleep.unitConfig.StopWhenUnneeded;
assert day.systemd.services.togi-day-watchdog-sleep.serviceConfig.Type == "oneshot";
assert day.systemd.services.togi-day-watchdog-sleep.serviceConfig.RemainAfterExit;
assert lib.hasSuffix "RuntimeWatchdogUSec t 0" (
  builtins.unsafeDiscardStringContext day.systemd.services.togi-day-watchdog-sleep.serviceConfig.ExecStart
);
assert
  map builtins.unsafeDiscardStringContext day.systemd.services.togi-day-watchdog-sleep.serviceConfig.ExecStopPost
  == [
    "-${builtins.unsafeDiscardStringContext "${pkgs.coreutils}/bin/timeout"} --verbose --kill-after=2s 5s ${builtins.unsafeDiscardStringContext "${pkgs.kmod}/bin/modprobe"} -r sp5100_tco"
    "-${builtins.unsafeDiscardStringContext "${pkgs.coreutils}/bin/timeout"} --verbose --kill-after=2s 5s ${builtins.unsafeDiscardStringContext "${pkgs.kmod}/bin/modprobe"} sp5100_tco"
    "${builtins.unsafeDiscardStringContext "${day.systemd.package}/bin/busctl"} set-property org.freedesktop.systemd1 /org/freedesktop/systemd1 org.freedesktop.systemd1.Manager RuntimeWatchdogUSec t 18446744073709551615"
  ];
assert day.systemd.services.togi-day-watchdog-sleep.serviceConfig.TimeoutStopSec == "30s";
assert lib.hasInfix "TimeoutStopSec=30s" day.systemd.units."togi-day-watchdog-sleep.service".text;
assert lib.hasInfix "--verbose --kill-after=2s 5s"
  day.systemd.units."togi-day-watchdog-sleep.service".text;
assert !(day.systemd.services.togi-day-watchdog-sleep.serviceConfig ? ExecStop);
assert !(one.systemd.services ? togi-day-watchdog-sleep);
assert !(dayTuning.systemd.services ? togi-day-watchdog-sleep);
assert lib.hasInfix ''[ "''${MONITOR_SERVICE_RESULT:-}" = start-limit-hit ] || exit 0''
  restartLimitScript;
assert lib.hasInfix
  (builtins.unsafeDiscardStringContext "${lib.getExe package} restart-limit --tuning-boot /boot/grub/grubenv")
  restartLimitScript;
assert lib.hasInfix "rm -f /run/togi-retry-tuning-boot" restartLimitScript;
assert lib.hasInfix "togi_leave_reason=" restartLimitScript;
assert lib.hasInfix ''\"id\":\"fallback-$INVOCATION_ID\",\"count\":0'' restartLimitScript;
assert lib.hasInfix "restart-limit command failed; returning to the normal system"
  restartLimitScript;
assert lib.hasInfix "grub-editenv /boot/grub/grubenv unset saved_entry" restartLimitScript;
assert lib.hasInfix "systemctl reboot" restartLimitScript;
assert !lib.hasInfix "--force" restartLimitScript;
assert !lib.hasInfix "next_entry" restartLimitScript;
assert builtins.elem pkgs.coreutils tuning.systemd.services.togi-restart-limit.path;
assert lib.hasInfix "[ ! -e /run/togi-retry-tuning-boot ] || exit 0" leaveTuningBoot.preStop;
# The power button restores the day's selection exactly; the plain unset is only the fallback when that command fails.
assert lib.hasInfix (builtins.unsafeDiscardStringContext "${lib.getExe package} night-end || {")
  leaveTuningBoot.preStop;
assert lib.hasInfix
  (builtins.unsafeDiscardStringContext "${pkgs.grub2}/bin/grub-editenv /boot/grub/grubenv unset saved_entry")
  leaveTuningBoot.preStop;
assert builtins.elem pkgs.grub2 consoleWatch.path;
assert builtins.elem pkgs.grub2 one.environment.systemPackages;
assert nightSetup.target == "togi/night.json";
assert builtins.elem "f /run/lock/togi.lock :0600 :root :root - -" one.systemd.tmpfiles.rules;
assert builtins.elem "f /run/lock/togi.lock :0660 :root :togi-hardware - -"
  delegated.systemd.tmpfiles.rules;
assert
  lib.sort lib.lessThan (
    map lib.toInt (
      lib.splitString " " tuning.systemd.services.togi.serviceConfig.RestartPreventExitStatus
    )
  ) == [ 3 ] ++ lib.range 10 18;
assert one.services.togi.package == package;
assert builtins.elem pkgs.hello overridden.environment.systemPackages;
assert builtins.elem pkgs.hello overriddenTuning.environment.systemPackages;
assert
  overriddenTuning.systemd.services.togi.serviceConfig.ExecStart
  == "${lib.getExe pkgs.hello} run --tuning-boot /boot/grub/grubenv";
assert lib.hasInfix
  (builtins.unsafeDiscardStringContext "${lib.getExe pkgs.hello} restart-limit --tuning-boot /boot/grub/grubenv")
  overriddenTuning.systemd.services.togi-restart-limit.script;
assert
  overriddenTuning.systemd.services.togi-watch.serviceConfig.ExecStart
  == "${lib.getExe pkgs.hello} watch --tuning-boot";
assert
  tuning.systemd.services.togi-watch.serviceConfig.ExecStart
  == "${lib.getExe package} watch --tuning-boot";
assert !(one.systemd.services ? togi-watch);
assert consoleWatch.wantedBy == [ "multi-user.target" ];
assert consoleWatch.environment.TERM == "linux";
assert consoleWatch.serviceConfig.StandardInput == "tty";
assert consoleWatch.serviceConfig.StandardOutput == "tty";
assert consoleWatch.serviceConfig.TTYPath == "/dev/tty1";
assert consoleWatch.serviceConfig.Restart == "always";
assert consoleWatch.serviceConfig.RestartSec == 1;
assert consoleWatch.serviceConfig.Nice == -10;
assert tuning.console.font == null;
assert !tuning.hardware.graphics.enable;
assert !(builtins.elem "togi.service" consoleWatch.requires);
assert !(builtins.elem "togi.service" consoleWatch.bindsTo);
assert togiService.serviceConfig.Restart == "on-failure";
assert togiService.serviceConfig.RestartSec == 60;
assert togiService.startLimitIntervalSec == 1800;
assert togiService.startLimitBurst == 3;
assert lib.hasInfix "ExecStopPost=" tuning.systemd.units."togi.service".text;
assert lib.hasInfix ''[ "''${SERVICE_RESULT:-}" != success ] || exit 0'' stopHook;
assert !lib.hasInfix "EXIT_STATUS" stopHook;
assert !lib.hasInfix "exit-code" stopHook;
assert lib.hasInfix
  (builtins.unsafeDiscardStringContext "exec ${lib.getExe package} emergency-stop")
  stopHook;
assert lib.hasSuffix "emergency-stop\n" stopHook;
assert builtins.elem tuning.systemd.package togiService.path;
assert builtins.elem pkgs.grub2 togiService.path;
assert !lib.hasInfix "emergency-stop" togiService.serviceConfig.ExecStart;
assert lib.hasInfix
  (builtins.unsafeDiscardStringContext "exec ${lib.getExe pkgs.hello} emergency-stop")
  overriddenTuning.systemd.services.togi.postStop;
assert lib.hasInfix "is-active togi-day.service || exit 0" resumeScript;
assert lib.hasInfix (builtins.unsafeDiscardStringContext "exec ${lib.getExe package} day reapply")
  resumeScript;
hostPkgs.runCommand "togi-module-check" { nativeBuildInputs = [ hostPkgs.jq ]; } ''
  jq --exit-status --slurpfile expected ${expectedSettings} \
    '. == $expected[0]' ${generatedConfig.source}
  jq --exit-status --slurpfile expected ${expectedNight} \
    '. == $expected[0]' ${nightSetup.source}
  touch "$out"
''
