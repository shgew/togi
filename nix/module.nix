{ packages }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.shycler;
  package = packages.${pkgs.stdenv.hostPlatform.system}.default;
  toml = pkgs.formats.toml { };
  grub = config.boot.loader.grub;
  grubenv = "${(lib.head grub.mirroredBoots).path}/grub/grubenv";
in
{
  options.services.shycler = {
    enable = lib.mkEnableOption "shycler, the per-core Curve Optimizer tuner";
    tuning.enable = lib.mkEnableOption "the shycler tuning boot, a GRUB entry that tunes unattended";
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
      description = "Configuration rendered to /etc/shycler/config.toml.";
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      {
        environment.systemPackages = [ package ];
        hardware.cpu.amd.ryzen-smu.enable = lib.mkDefault true;
        environment.etc."shycler/config.toml".source = toml.generate "shycler-config.toml" cfg.settings;
        services.shycler.settings.backends = {
          mprime = lib.mkIf cfg.backends.mprime.enable (lib.mkDefault "${pkgs.mprime}");
          ycruncher = lib.mkIf cfg.backends.ycruncher.enable (lib.mkDefault "${pkgs.y-cruncher}");
        };
      }
      (lib.mkIf cfg.tuning.enable {
        assertions = [
          {
            assertion = grub.enable && grub.mirroredBoots != [ ];
            message = "services.shycler.tuning.enable needs GRUB: the tuning boot relies on GRUB's saved entry";
          }
        ];
        boot.loader.grub.default = "saved";
        specialisation.shycler.configuration = {
          system.nixos.tags = [ "shycler" ];
          boot.loader.grub.configurationName = "shycler";
          systemd.defaultUnit = lib.mkForce "multi-user.target";
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
          systemd.services.shycler = {
            description = "shycler tuning boot";
            wantedBy = [ "multi-user.target" ];
            after = [
              "systemd-modules-load.service"
              "local-fs.target"
            ];
            path = [ pkgs.grub2 ];
            startLimitIntervalSec = 1800;
            startLimitBurst = 3;
            serviceConfig = {
              ExecStart = "${lib.getExe package} run --tuning-boot ${grubenv}";
              Restart = "on-failure";
              RestartSec = 60;
              RestartPreventExitStatus = "10 11 12 13 14 15 16 17";
              StateDirectory = "shycler";
            };
          };
          systemd.services.shycler-leave-tuning-boot = lib.mkIf cfg.tuning.leaveOnShutdown {
            description = "Return the next boot to the normal system after an orderly shutdown";
            wantedBy = [ "multi-user.target" ];
            before = [ "shycler.service" ];
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
          systemd.services.shycler-kernel-log = {
            description = "Send kernel messages to tty3 in the shycler tuning boot";
            wantedBy = [ "multi-user.target" ];
            before = [ "shycler-watch.service" ];
            serviceConfig = {
              Type = "oneshot";
              RemainAfterExit = true;
              ExecStart = "${pkgs.kbd}/bin/setlogcons 3";
            };
          };
          systemd.services.shycler-watch = {
            description = "shycler dashboard on tty1";
            after = [
              "systemd-user-sessions.service"
              "shycler-kernel-log.service"
            ];
            wants = [ "shycler-kernel-log.service" ];
            wantedBy = [ "multi-user.target" ];
            unitConfig.ConditionPathExists = "/dev/tty1";
            environment.TERM = "linux";
            serviceConfig = {
              ExecStart = "${lib.getExe package} watch";
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
          systemd.services.shycler-console = {
            description = "shycler tuning boot log on tty3";
            after = [ "systemd-user-sessions.service" ];
            wantedBy = [ "multi-user.target" ];
            unitConfig.ConditionPathExists = "/dev/tty3";
            serviceConfig = {
              ExecStart = "${lib.getExe' config.systemd.package "journalctl"} --follow --lines 100 --no-pager --unit shycler.service --output cat";
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
