{ packages }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.togi;
  toml = pkgs.formats.toml { };
  grub = config.boot.loader.grub;
  grubenv = "${(lib.head grub.mirroredBoots).path}/grub/grubenv";
in
{
  options.services.togi = {
    enable = lib.mkEnableOption "togi, the per-core Curve Optimizer tuner";
    package = lib.mkOption {
      type = lib.types.package;
      default = packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "inputs.togi.packages.\${pkgs.stdenv.hostPlatform.system}.default";
      description = "The togi package to install and run in the tuning boot.";
    };
    hardwareTestGroup = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "togi-hardware";
      description = "An explicitly authorized existing group whose members may acquire the root-owned host lock for delegated hardware tests. The default permits root only. This does not grant access to hardware or a delegated cpuset controller.";
    };
    tuning.enable = lib.mkEnableOption "the togi tuning boot, a GRUB entry that tunes unattended";
    tuning.leaveOnShutdown = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Clear GRUB's saved entry on an orderly shutdown or reboot of the tuning boot, so the next boot is the normal system. Crash reboots return to the tuning boot either way.";
    };
    backends.mprime.enable = lib.mkEnableOption "the mprime backend (unfree)";
    backends.ycruncher.enable = lib.mkEnableOption "the y-cruncher backend (unfree)";
    tuning.consoleFont = lib.mkOption {
      type = lib.types.nullOr (lib.types.either lib.types.str lib.types.path);
      default = null;
      example = lib.literalExpression ''"''${pkgs.terminus_font}/share/consolefonts/ter-i32b.psf.gz"'';
      description = "Console font of the tuning boot, as console.font takes it. The default, null, is the kernel's built-in font whatever the system sets: 8x16 below 2560x1080 and Terminus 16x32 bold from there, which make the 240x67 frame the dashboard is laid out for at 1080p and 4K, and which cover IBM437, whose block and box glyphs the dashboard draws with. A font set here must cover them too: Terminus' ter-i fonts do, while ter-v and Lat2-Terminus fonts lack the half block the digits use.";
    };
    settings = lib.mkOption {
      type = toml.type;
      default = { };
      description = "Configuration rendered to /etc/togi/config.toml. backend_user defaults to the module-declared togi-trial system user; togi stays root while workloads run as that user with no supplementary groups.";
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      {
        environment.systemPackages = [ cfg.package ];
        hardware.cpu.amd.ryzen-smu.enable = lib.mkDefault true;
        environment.etc."togi/config.toml".source = toml.generate "togi-config.toml" cfg.settings;
        users.groups.togi-trial = { };
        users.users.togi-trial = {
          isSystemUser = true;
          group = "togi-trial";
        };
        services.togi.settings.backend_user = lib.mkDefault "togi-trial";
        systemd.tmpfiles.rules = [
          "f /run/lock/togi.lock :${if cfg.hardwareTestGroup == null then "0600" else "0660"} :root :${
            if cfg.hardwareTestGroup == null then "root" else cfg.hardwareTestGroup
          } - -"
        ];
        services.togi.settings.backends = {
          mprime = lib.mkIf cfg.backends.mprime.enable (lib.mkDefault "${pkgs.mprime}");
          ycruncher = lib.mkIf cfg.backends.ycruncher.enable (lib.mkDefault "${pkgs.y-cruncher}");
        };
      }
      (lib.mkIf cfg.tuning.enable {
        assertions = [
          {
            assertion = grub.enable && builtins.length grub.mirroredBoots == 1;
            message = "services.togi.tuning.enable requires GRUB with exactly one mirroredBoots entry: togi's tuning boot supports a single GRUB environment";
          }
        ];
        boot.loader.grub.default = "saved";
        specialisation.togi.configuration = {
          system.nixos.tags = [ "togi" ];
          boot.loader.grub.configurationName = "togi";
          systemd.defaultUnit = lib.mkForce "multi-user.target";
          boot.initrd.kernelModules = [ "sp5100_tco" ];
          boot.kernel.sysctl = {
            "kernel.panic" = 10;
            "kernel.panic_on_oops" = 1;
            "kernel.hardlockup_panic" = 1;
            "kernel.softlockup_panic" = 1;
          };
          systemd.settings.Manager.RuntimeWatchdogSec = "30s";
          services.journald.settings.Journal = {
            Storage = "persistent";
            SyncIntervalSec = "1s";
          };
          systemd.sleep.settings.Sleep = {
            AllowSuspend = false;
            AllowHibernation = false;
            AllowHybridSleep = false;
            AllowSuspendThenHibernate = false;
          };
          systemd.services.togi-restart-limit = {
            description = "Return to the normal boot when togi reaches its restart limit";
            unitConfig.RequiresMountsFor = grubenv;
            path = [
              pkgs.grub2
              pkgs.systemd
            ];
            serviceConfig.Type = "oneshot";
            script = ''
              [ "''${MONITOR_SERVICE_RESULT:-}" = start-limit-hit ] || exit 0
              grub-editenv ${grubenv} unset saved_entry
              systemctl reboot
            '';
          };
          systemd.services.togi = {
            description = "togi tuning boot";
            onFailure = [ "togi-restart-limit.service" ];
            unitConfig.RequiresMountsFor = grubenv;
            wantedBy = [ "multi-user.target" ];
            after = [
              "systemd-modules-load.service"
              "local-fs.target"
            ];
            path = [ pkgs.grub2 ];
            startLimitIntervalSec = 1800;
            startLimitBurst = 3;
            serviceConfig = {
              ExecStart = "${lib.getExe cfg.package} run --tuning-boot ${grubenv}";
              Restart = "on-failure";
              RestartSec = 60;
              RestartPreventExitStatus = "3 10 11 12 13 14 15 16 17 18";
              StateDirectory = "togi";
            };
          };
          systemd.services.togi-leave-tuning-boot = lib.mkIf cfg.tuning.leaveOnShutdown {
            description = "Return the next boot to the normal system after an orderly shutdown";
            wantedBy = [ "multi-user.target" ];
            before = [ "togi.service" ];
            restartIfChanged = false;
            unitConfig.RequiresMountsFor = grubenv;
            serviceConfig = {
              Type = "oneshot";
              RemainAfterExit = true;
              ExecStart = "${pkgs.coreutils}/bin/true";
              ExecStop = "${pkgs.grub2}/bin/grub-editenv ${grubenv} unset saved_entry";
            };
          };
          console.font = lib.mkForce cfg.tuning.consoleFont;
          boot.kernelParams = lib.mkForce (
            lib.filter (p: builtins.match "console=tty[0-9]+(,.*)?" p == null) config.boot.kernelParams
            ++ [ "console=tty3" ]
          );
          systemd.services."getty@tty1".enable = false;
          systemd.services."autovt@tty1".enable = false;
          systemd.services."getty@tty3".enable = false;
          systemd.services."autovt@tty3".enable = false;
          systemd.services.togi-kernel-log = {
            description = "Send kernel messages to tty3 in the togi tuning boot";
            wantedBy = [ "multi-user.target" ];
            before = [ "togi-watch.service" ];
            serviceConfig = {
              Type = "oneshot";
              RemainAfterExit = true;
              ExecStart = "${pkgs.kbd}/bin/setlogcons 3";
            };
          };
          systemd.services.togi-watch = {
            description = "togi dashboard on tty1";
            after = [
              "systemd-user-sessions.service"
              "togi-kernel-log.service"
            ];
            wants = [ "togi-kernel-log.service" ];
            wantedBy = [ "multi-user.target" ];
            unitConfig.ConditionPathExists = "/dev/tty1";
            environment.TERM = "linux";
            serviceConfig = {
              ExecStart = "${lib.getExe cfg.package} watch";
              StandardOutput = "tty";
              StandardError = "journal";
              TTYPath = "/dev/tty1";
              TTYReset = true;
              TTYVHangup = true;
              TTYVTDisallocate = true;
              Restart = "always";
              RestartSec = 1;
              Nice = -10;
            };
          };
          systemd.services.togi-console = {
            description = "togi tuning boot log on tty3";
            after = [ "systemd-user-sessions.service" ];
            wantedBy = [ "multi-user.target" ];
            unitConfig.ConditionPathExists = "/dev/tty3";
            serviceConfig = {
              ExecStart = "${lib.getExe' config.systemd.package "journalctl"} --follow --lines 100 --no-pager --unit togi.service --output cat";
              StandardOutput = "tty";
              StandardError = "tty";
              TTYPath = "/dev/tty3";
              TTYReset = true;
              TTYVHangup = true;
              TTYVTDisallocate = true;
              Restart = "always";
            };
          };
        };
      })
    ]
  );
}
