{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.shycler;
  package = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
  toml = pkgs.formats.toml { };
  grub = config.boot.loader.grub;
  grubenv = "${(lib.head grub.mirroredBoots).path}/grub/grubenv";
in
{
  options.services.shycler = {
    enable = lib.mkEnableOption "shycler, the per-core Curve Optimizer tuner";
    tuning.enable = lib.mkEnableOption "the shycler tuning boot, a GRUB entry that tunes unattended";
    backends.mprime.enable = lib.mkEnableOption "the mprime backend (unfree)";
    backends.ycruncher.enable = lib.mkEnableOption "the y-cruncher backend (unfree)";
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
          systemd.services."getty@tty1".enable = false;
          systemd.services."autovt@tty1".enable = false;
          systemd.services.shycler-console = {
            description = "shycler tuning boot log on tty1";
            after = [ "systemd-user-sessions.service" ];
            wantedBy = [ "multi-user.target" ];
            unitConfig.ConditionPathExists = "/dev/tty1";
            serviceConfig = {
              ExecStart = "${lib.getExe' config.systemd.package "journalctl"} --follow --lines 100 --no-pager --unit shycler.service --output cat";
              StandardOutput = "tty";
              StandardError = "tty";
              TTYPath = "/dev/tty1";
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
