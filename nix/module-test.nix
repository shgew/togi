# Evaluates the x86_64-linux NixOS configuration on every host, VM-free: loader selection and mount
# dependencies, GRUB mirror constraints, package overrides and the tuning specialisation's 30s watchdog.
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
    evaluateDisplay mirrors selectedPackage hardwareTestGroup null 27;
  evaluateDisplay =
    mirrors: selectedPackage: hardwareTestGroup: display: fontSize:
    evaluateDay mirrors selectedPackage hardwareTestGroup display fontSize { };
  evaluateDay =
    mirrors: selectedPackage: hardwareTestGroup: display: fontSize: day:
    evaluateLoader mirrors selectedPackage hardwareTestGroup display fontSize day { };
  evaluateLoader =
    mirrors: selectedPackage: hardwareTestGroup: display: fontSize: day: loader:
    (import (pkgs.path + "/nixos/lib/eval-config.nix") {
      # Keep generated configuration files native while evaluating Linux boot settings on every host.
      pkgs = pkgs // {
        formats = hostPkgs.formats;
      };
      system = pkgs.stdenv.hostPlatform.system;
      modules = [
        module
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
              fsType = if loader.systemd-boot.enable or false then "vfat" else "ext4";
              options = [ "noauto" ];
            };
          };
          services.togi = {
            inherit day;
            enable = true;
            package = lib.mkIf (selectedPackage != null) selectedPackage;
            inherit hardwareTestGroup;
            tuning.enable = true;
            tuning.display = lib.mkIf (display != null) display;
            tuning.kmscon.fontSize = fontSize;
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
  systemd = evaluateLoader [ ] null null null 27 { } systemdLoader;
  systemdTuning = systemd.specialisation.togi.configuration;
  systemdMirrors = evaluateLoader [
    (mirror "/boot")
    (mirror "/boot2")
  ] null null null 27 { } systemdLoader;
  bothLoaders = evaluateLoader [ (mirror "/boot") ] null null null 27 { } {
    systemd-boot.enable = true;
  };
  neitherLoader = evaluateLoader [ ] null null null 27 { } {
    grub.enable = false;
  };
  xbootldr = evaluateLoader [ ] null null null 27 { } (
    systemdLoader
    // {
      efi.efiSysMountPoint = "/efi";
      systemd-boot = {
        enable = true;
        xbootldrMountPoint = "/boot";
      };
    }
  );
  overridden = evaluate [ (mirror "/boot") ] pkgs.hello null;
  delegated = evaluate [ (mirror "/boot") ] null "togi-hardware";
  overriddenTuning = overridden.specialisation.togi.configuration;
  day = evaluateDay [ (mirror "/boot") ] null null null 27 { enable = true; };
  dayTuning = day.specialisation.togi.configuration;
  watched = evaluateDay [ (mirror "/boot") ] null null null 27 {
    enable = true;
    watchdog = true;
  };
  watchedTuning = watched.specialisation.togi.configuration;
  dayUsers = evaluateDay [ (mirror "/boot") ] null null null 27 {
    enable = true;
    users = [ "alice" ];
  };
  dayUsersTuning = dayUsers.specialisation.togi.configuration;
  dayTwoUsers = evaluateDay [ (mirror "/boot") ] null null null 27 {
    enable = true;
    users = [
      "alice"
      "bob"
    ];
  };
  daySystemUser = evaluateDay [ (mirror "/boot") ] null null null 27 {
    enable = true;
    users = [ "svc" ];
  };
  lockupPanics =
    config:
    lib.filter (p: lib.hasInfix "lockup" p || lib.hasInfix "nmi_watchdog" p) config.boot.kernelParams
    ++ lib.filter (k: lib.hasInfix "lockup" k || lib.hasInfix "nmi_watchdog" k) (
      builtins.attrNames config.boot.kernel.sysctl
    );
  rejectsMirrors =
    config:
    lib.any (
      a: !a.assertion && lib.hasPrefix "services.togi.tuning.enable" a.message
    ) config.assertions;
  tuning = one.specialisation.togi.configuration;
  console =
    (evaluateDisplay [ (mirror "/boot") ] null null "console" 27).specialisation.togi.configuration;
  kmscon =
    (evaluateDisplay [ (mirror "/boot") ] null null "kmscon" 27).specialisation.togi.configuration;
  largeFont =
    (evaluateDisplay [ (mirror "/boot") ] pkgs.hello null "kmscon" 30)
    .specialisation.togi.configuration;
  consoleWatch = tuning.systemd.services.togi-watch;
  kmsconService = kmscon.systemd.services.togi-kmscon;
  restartLimitScript = tuning.systemd.services.togi-restart-limit.script;
  leaveTuningBoot = tuning.systemd.services.togi-leave-tuning-boot;
  togiService = tuning.systemd.services.togi;
  # ExecStopPost is the module's postStop script: the shipped package's zero-only emergency path, gated on an
  # abnormal result.
  stopHook = togiService.postStop;
  generatedConfig = one.environment.etc."togi/config.json";
  nightSetup = one.environment.etc."togi/night.json";
  expectedNight = hostPkgs.writeText "togi-module-night.json" (
    builtins.toJSON {
      loader = "grub";
      grubenv = "/boot/grub/grubenv";
      entry = "NixOS - togi";
    }
  );
  expectedSystemdNight = hostPkgs.writeText "togi-module-systemd-night.json" (
    builtins.toJSON {
      loader = "systemd-boot";
      esp = "/boot";
      boot = "/boot";
      state = "/var/lib/togi-boot";
      specialisation = "togi";
    }
  );
  expectedXbootldrNight = hostPkgs.writeText "togi-module-xbootldr-night.json" (
    builtins.toJSON {
      loader = "systemd-boot";
      esp = "/efi";
      boot = "/boot";
      state = "/var/lib/togi-boot";
      specialisation = "togi";
    }
  );
  expectedSettings = hostPkgs.writeText "togi-module-settings.json" (
    builtins.toJSON one.services.togi.settings
  );
in
assert rejectsMirrors zero;
assert lib.all (a: a.assertion) one.assertions;
assert !rejectsMirrors one;
assert rejectsMirrors two;
assert rejectsMirrors bothLoaders;
assert rejectsMirrors neitherLoader;
assert lib.all (a: a.assertion) systemd.assertions;
assert !rejectsMirrors systemdMirrors;
assert
  systemdTuning.systemd.services.togi.serviceConfig.ExecStart
  == "${lib.getExe package} run --tuning-boot /etc/togi/night.json";
assert lib.hasInfix "--tuning-boot /etc/togi/night.json"
  systemdTuning.systemd.services.togi-restart-limit.script;
assert lib.all
  (
    name:
    systemdTuning.systemd.services.${name}.unitConfig.RequiresMountsFor == "/boot /var/lib/togi-boot"
  )
  [
    "togi"
    "togi-restart-limit"
    "togi-leave-tuning-boot"
  ];
assert
  xbootldr.specialisation.togi.configuration.systemd.services.togi.unitConfig.RequiresMountsFor
  == "/efi /var/lib/togi-boot /boot";
assert builtins.elem "d /var/lib/togi-boot 0700 root root -" systemd.systemd.tmpfiles.rules;
assert builtins.elem systemd.systemd.package systemd.environment.systemPackages;
assert !(builtins.elem pkgs.grub2 systemd.environment.systemPackages);
assert lib.all
  (name: builtins.elem systemdTuning.systemd.package systemdTuning.systemd.services.${name}.path)
  [
    "togi"
    "togi-watch"
    "togi-restart-limit"
    "togi-leave-tuning-boot"
  ];
assert lib.hasInfix "night-end || {" systemdTuning.systemd.services.togi-leave-tuning-boot.preStop;
assert lib.hasInfix ''set-default ""''
  systemdTuning.systemd.services.togi-leave-tuning-boot.preStop;
assert lib.hasInfix ''set-oneshot ""''
  systemdTuning.systemd.services.togi-leave-tuning-boot.preStop;
assert lib.hasInfix "LoaderEntryLastBooted-4a67b082"
  systemdTuning.systemd.services.togi-leave-tuning-boot.preStop;
assert lib.hasInfix "--esp-path=/efi"
  xbootldr.specialisation.togi.configuration.systemd.services.togi-leave-tuning-boot.preStop;
assert !lib.hasInfix "grub-editenv" systemdTuning.systemd.services.togi-restart-limit.script;
assert !lib.hasInfix "@current" systemdTuning.systemd.services.togi-restart-limit.script;
assert generatedConfig.target == "togi/config.json";
assert one.services.togi.settings.backend_user == "togi-trial";
assert builtins.elem "noauto" tuning.fileSystems."/boot".options;
assert tuning.systemd.services.togi.unitConfig.RequiresMountsFor == "/boot/grub/grubenv";
assert lib.hasInfix "RequiresMountsFor=/boot/grub/grubenv" tuning.systemd.units."togi.service".text;
assert builtins.elem "systemd-pstore.service" tuning.systemd.services.togi.after;
assert builtins.elem "printk.always_kmsg_dump=1" tuning.boot.kernelParams;
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
# The watchdog is off by default; on, the normal boot's PID 1 watchdog is 60s without lockup panics.
assert !day.services.togi.day.watchdog;
assert !(day.systemd.settings.Manager ? RuntimeWatchdogSec);
assert !(one.systemd.settings.Manager ? RuntimeWatchdogSec);
assert watched.services.togi.day.watchdog;
assert watched.systemd.settings.Manager.RuntimeWatchdogSec == "60s";
assert builtins.elem "sp5100_tco" watched.boot.initrd.kernelModules;
assert !(builtins.elem "sp5100_tco" day.boot.initrd.kernelModules);
assert lockupPanics watched == [ ];
assert lockupPanics day == [ ];
assert watchedTuning.systemd.settings.Manager.RuntimeWatchdogSec == "30s";
# Around sleep the watchdog is stopped and restarted through the manager's RuntimeWatchdogUSec property: 0 disarms
# it, infinity restores the configured 60s. Without the option, and in the tuning boot, there is no such unit.
assert watched.systemd.services.togi-day-watchdog-sleep.wantedBy == [ "sleep.target" ];
assert watched.systemd.services.togi-day-watchdog-sleep.before == [ "sleep.target" ];
assert watched.systemd.services.togi-day-watchdog-sleep.unitConfig.StopWhenUnneeded;
assert lib.hasSuffix "RuntimeWatchdogUSec t 0" (
  builtins.unsafeDiscardStringContext watched.systemd.services.togi-day-watchdog-sleep.serviceConfig.ExecStart
);
assert
  map builtins.unsafeDiscardStringContext watched.systemd.services.togi-day-watchdog-sleep.serviceConfig.ExecStopPost
  == [
    "-${builtins.unsafeDiscardStringContext "${pkgs.coreutils}/bin/timeout"} --verbose --kill-after=2s 5s ${builtins.unsafeDiscardStringContext "${pkgs.kmod}/bin/modprobe"} -r sp5100_tco"
    "-${builtins.unsafeDiscardStringContext "${pkgs.coreutils}/bin/timeout"} --verbose --kill-after=2s 5s ${builtins.unsafeDiscardStringContext "${pkgs.kmod}/bin/modprobe"} sp5100_tco"
    "${builtins.unsafeDiscardStringContext "${watched.systemd.package}/bin/busctl"} set-property org.freedesktop.systemd1 /org/freedesktop/systemd1 org.freedesktop.systemd1.Manager RuntimeWatchdogUSec t 18446744073709551615"
  ];
assert watched.systemd.services.togi-day-watchdog-sleep.serviceConfig.TimeoutStopSec == "30s";
assert lib.hasInfix "TimeoutStopSec=30s"
  watched.systemd.units."togi-day-watchdog-sleep.service".text;
assert lib.hasInfix "--verbose --kill-after=2s 5s"
  watched.systemd.units."togi-day-watchdog-sleep.service".text;
assert !(watched.systemd.services.togi-day-watchdog-sleep.serviceConfig ? ExecStop);
assert !(day.systemd.services ? togi-day-watchdog-sleep);
assert !(watchedTuning.systemd.services ? togi-day-watchdog-sleep);
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
assert builtins.elem pkgs.grub2 kmsconService.path;
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
assert one.services.togi.tuning.display == "console";
assert
  tuning.systemd.units."togi-watch.service".text == console.systemd.units."togi-watch.service".text;
assert !(tuning.systemd.services ? togi-kmscon);
assert consoleWatch.wantedBy == [ "multi-user.target" ];
assert consoleWatch.environment.TERM == "linux";
assert consoleWatch.serviceConfig.StandardInput == "tty";
assert consoleWatch.serviceConfig.StandardOutput == "tty";
assert consoleWatch.serviceConfig.TTYPath == "/dev/tty1";
assert consoleWatch.serviceConfig.Restart == "always";
assert consoleWatch.serviceConfig.RestartSec == 1;
assert consoleWatch.serviceConfig.Nice == -10;
assert kmscon.systemd.services.togi-watch.wantedBy == [ ];
assert kmscon.systemd.services.togi-watch.serviceConfig == consoleWatch.serviceConfig;
assert kmscon.systemd.services.togi-watch.environment == consoleWatch.environment;
assert kmsconService.wantedBy == [ "multi-user.target" ];
assert kmsconService.serviceConfig.TTYPath == "/dev/tty1";
assert kmsconService.environment.TERM == "xterm-256color";
assert kmsconService.environment.COLORTERM == "truecolor";
assert kmsconService.serviceConfig.Restart == consoleWatch.serviceConfig.Restart;
assert kmsconService.serviceConfig.RestartSec == consoleWatch.serviceConfig.RestartSec;
assert kmsconService.serviceConfig.Nice == consoleWatch.serviceConfig.Nice;
assert (kmsconService.serviceConfig.User or "root") == (consoleWatch.serviceConfig.User or "root");
assert
  (kmsconService.serviceConfig.Group or "root") == (consoleWatch.serviceConfig.Group or "root");
assert kmsconService.startLimitBurst == 3;
assert kmsconService.startLimitIntervalSec == 180;
assert kmsconService.onFailure == [ "togi-watch-fallback.service" ];
assert kmsconService.conflicts == [ "togi-watch.service" ];
assert builtins.elem "togi-watch.service" kmsconService.after;
assert builtins.elem pkgs.dejavu_fonts kmscon.fonts.packages;
assert kmscon.hardware.graphics.enable;
assert kmscon.fonts.fontconfig.enable;
assert !tuning.hardware.graphics.enable;
assert lib.hasInfix "--font-size 27" kmsconService.serviceConfig.ExecStart;
assert lib.hasInfix "--font-size 30" largeFont.systemd.services.togi-kmscon.serviceConfig.ExecStart;
assert lib.hasSuffix "--login -- ${lib.getExe pkgs.hello} watch --tuning-boot"
  largeFont.systemd.services.togi-kmscon.serviceConfig.ExecStart;
assert lib.all (grab: lib.hasInfix "--grab-${grab} ''" kmsconService.serviceConfig.ExecStart) [
  "scroll-up"
  "scroll-down"
  "page-up"
  "page-down"
  "zoom-in"
  "zoom-out"
  "session-next"
  "session-prev"
  "session-close"
  "terminal-new"
  "rotate-cw"
  "rotate-ccw"
  "reboot"
];
assert lib.hasInfix "--no-session-control" kmsconService.serviceConfig.ExecStart;
assert lib.hasInfix "--switchvt" kmsconService.serviceConfig.ExecStart;
assert lib.hasInfix "--hwaccel" kmsconService.serviceConfig.ExecStart;
assert kmscon.systemd.services.togi-watch-fallback.after == [ "togi-kmscon.service" ];
assert lib.hasInfix ''[ "''${MONITOR_SERVICE_RESULT:-}" = start-limit-hit ] || exit 0''
  kmscon.systemd.services.togi-watch-fallback.script;
assert lib.hasInfix "--no-block start togi-watch.service"
  kmscon.systemd.services.togi-watch-fallback.script;
assert kmscon.systemd.units."togi.service".text == tuning.systemd.units."togi.service".text;
assert !(builtins.elem "togi.service" kmsconService.requires);
assert !(builtins.elem "togi.service" kmsconService.bindsTo);
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
hostPkgs.runCommand "togi-module-check" { nativeBuildInputs = [ hostPkgs.jq ]; } ''
  jq --exit-status --slurpfile expected ${expectedSettings} \
    '. == $expected[0]' ${generatedConfig.source}
  jq --exit-status --slurpfile expected ${expectedNight} \
    '. == $expected[0]' ${nightSetup.source}
  jq --exit-status --slurpfile expected ${expectedSystemdNight} \
    '. == $expected[0]' ${systemd.environment.etc."togi/night.json".source}
  jq --exit-status --slurpfile expected ${expectedXbootldrNight} \
    '. == $expected[0]' ${xbootldr.environment.etc."togi/night.json".source}
  jq --exit-status --raw-input --slurp \
    --arg executable ${lib.escapeShellArg "${lib.getExe package} day reapply"} \
    'contains("is-active togi-day.service || exit 0") and contains("exec " + $executable)' \
    ${day.systemd.services.togi-day-resume.serviceConfig.ExecStopPost}
  touch "$out"
''
