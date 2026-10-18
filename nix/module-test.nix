# Evaluates the x86_64-linux NixOS configuration on every host, VM-free: GRUB mirror constraints, the
# grubenv mount dependency, package overrides and the tuning specialisation's spec'd 30s watchdog.
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
          boot.loader.grub = {
            enable = true;
            mirroredBoots = mirrors;
          };
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
            enable = true;
            package = lib.mkIf (selectedPackage != null) selectedPackage;
            inherit hardwareTestGroup;
            tuning.enable = true;
            tuning.display = lib.mkIf (display != null) display;
            tuning.kmscon.fontSize = fontSize;
            settings = {
              start_offsets."3" = -10;
              candidate_solo_limits."5" = -20;
              durations.search_trial_s = 60;
              checking.cycle = [ "R7" ];
              evidence = {
                miss = 0.001;
                rate = 0.25;
              };
            };
          };
          hardware.cpu.amd.ryzen-smu.enable = false;
          users.groups = lib.optionalAttrs (hardwareTestGroup != null) {
            ${hardwareTestGroup} = { };
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
  overridden = evaluate [ (mirror "/boot") ] pkgs.hello null;
  delegated = evaluate [ (mirror "/boot") ] null "togi-hardware";
  overriddenTuning = overridden.specialisation.togi.configuration;
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
  expectedSettings = hostPkgs.writeText "togi-module-settings.json" (
    builtins.toJSON one.services.togi.settings
  );
in
assert rejectsMirrors zero;
assert lib.all (a: a.assertion) one.assertions;
assert !rejectsMirrors one;
assert rejectsMirrors two;
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
assert lib.hasInfix "grub-editenv /boot/grub/grubenv unset saved_entry\nsystemctl reboot"
  restartLimitScript;
assert !lib.hasInfix "--force" restartLimitScript;
assert !lib.hasInfix "next_entry" restartLimitScript;
assert builtins.elem pkgs.coreutils tuning.systemd.services.togi-restart-limit.path;
assert lib.hasInfix "[ ! -e /run/togi-retry-tuning-boot ] || exit 0" leaveTuningBoot.preStop;
assert lib.hasInfix
  (builtins.unsafeDiscardStringContext "${pkgs.grub2}/bin/grub-editenv /boot/grub/grubenv unset saved_entry")
  leaveTuningBoot.preStop;
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
  touch "$out"
''
