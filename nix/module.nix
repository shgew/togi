{ packages }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.togi;
  json = pkgs.formats.json { };
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
    day.enable = lib.mkEnableOption "the togi day observer, a read-only service in the normal boot that records the machine's day in togi's journal. It holds the host lock and the journal writer lock while it runs, so `togi run` and `togi reset` fail lock-busy until it stops, and it refuses to start while there is no session. It never exists in the tuning boot, and enabling it keeps the journal on disk with a 5s sync interval";
    tuning.leaveOnShutdown = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Clear GRUB's saved entry on an orderly operator shutdown or reboot of the tuning boot, so the next boot is the normal system. Restart-limit retry reboots preserve the tuning entry; crash reboots return to it either way.";
    };
    tuning.display = lib.mkOption {
      type = lib.types.enum [
        "console"
        "kmscon"
      ];
      default = "console";
      description = "Dashboard display in the tuning boot. console keeps the Linux virtual console; kmscon uses a GPU-accelerated terminal on tty1 with truecolour advertised to the dashboard. Repeated kmscon failures fall back to the console for this boot, without affecting tuning.";
    };
    tuning.kmscon.fontSize = lib.mkOption {
      type = lib.types.ints.positive;
      default = 27;
      description = "DejaVu Sans Mono font size in pixels for the kmscon tuning display. The default gives 16x32 pixel cells, at least 160x45 cells on a 2560x1440 output. With multiple outputs, kmscon clones the display and uses the smallest grid.";
    };
    backends.mprime.enable = lib.mkEnableOption "the mprime backend (unfree)";
    backends.ycruncher.enable = lib.mkEnableOption "the y-cruncher backend (unfree)";
    tuning.consoleFont = lib.mkOption {
      type = lib.types.nullOr (lib.types.either lib.types.str lib.types.path);
      default = null;
      example = lib.literalExpression ''"''${pkgs.terminus_font}/share/consolefonts/ter-i32b.psf.gz"'';
      description = "Console font of the tuning boot, as console.font takes it. The default, null, is the kernel's built-in font whatever the system sets: 8x16 below 2560x1080 and Terminus 16x32 bold from there. The 8x16 font gives 240x67 at 1920x1080; 16x32 gives 160x45 (Medium) at 2560x1440 and 240x67 (Wide) at 3840x2160. Both cover IBM437, whose block and box glyphs the dashboard draws with. A font set here must cover them too: Terminus' ter-i fonts do, while ter-v and Lat2-Terminus fonts lack the half block the digits use.";
    };
    settings = lib.mkOption {
      type = json.type;
      default = { };
      description = "Configuration rendered to /etc/togi/config.json. backend_user defaults to the module-declared togi-trial system user; togi stays root while workloads run as that user with no supplementary groups.";
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      {
        environment.systemPackages = [ cfg.package ];
        hardware.cpu.amd.ryzen-smu.enable = lib.mkDefault true;
        environment.etc."togi/config.json".source = json.generate "togi-config.json" cfg.settings;
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
      (lib.mkIf cfg.day.enable {
        # The journal must survive a hang or reset, and a short sync interval bounds what the final seconds can lose.
        services.journald.settings.Journal = {
          Storage = lib.mkDefault "persistent";
          SyncIntervalSec = lib.mkDefault "5s";
        };
        systemd.services.togi-day = {
          description = "togi day observer";
          wantedBy = [ "multi-user.target" ];
          after = [
            "systemd-modules-load.service"
            "local-fs.target"
          ];
          serviceConfig = {
            ExecStart = "${lib.getExe cfg.package} day --state-dir /var/lib/togi";
            Restart = "on-failure";
            RestartSec = 60;
            StateDirectory = "togi";
          };
        };
      })
      (lib.mkIf cfg.tuning.enable {
        assertions = [
          {
            assertion = grub.enable && builtins.length grub.mirroredBoots == 1;
            message = "services.togi.tuning.enable requires GRUB with exactly one mirroredBoots entry: togi's tuning boot supports a single GRUB environment";
          }
        ];
        boot.loader.grub.default = "saved";
        specialisation.togi.configuration = {
          # The tuning boot inherits the normal configuration; the observer is a normal-boot service.
          services.togi.day.enable = lib.mkForce false;
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
            description = "Recover when togi reaches its service restart limit";
            unitConfig.RequiresMountsFor = grubenv;
            path = [
              pkgs.coreutils
              pkgs.grub2
              pkgs.systemd
            ];
            serviceConfig.Type = "oneshot";
            script = ''
              [ "''${MONITOR_SERVICE_RESULT:-}" = start-limit-hit ] || exit 0
              if ${lib.getExe cfg.package} restart-limit --tuning-boot ${grubenv}; then
                exit 0
              fi
              rm -f /run/togi-retry-tuning-boot || true
              grub-editenv ${grubenv} set "togi_leave_reason={\"id\":\"fallback-$INVOCATION_ID\",\"count\":0,\"reason\":\"restart-limit command failed; returning to the normal system\"}" || true
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
              "systemd-pstore.service"
              "local-fs.target"
            ];
            path = [
              pkgs.grub2
              config.systemd.package
            ];
            startLimitIntervalSec = 1800;
            startLimitBurst = 3;
            serviceConfig = {
              ExecStart = "${lib.getExe cfg.package} run --tuning-boot ${grubenv}";
              Restart = "on-failure";
              RestartSec = 60;
              RestartPreventExitStatus = lib.concatMapStringsSep " " toString ([ 3 ] ++ lib.range 10 18);
              StateDirectory = "togi";
            };
            # An abnormal exit, whatever its status, may leave workloads and offsets applied through RestartSec, so
            # only a success skips the hook. `togi emergency-stop` takes the host lock and validates the SMU before
            # it writes, so a contended lock or an unsupported host exits with offsets untouched.
            postStop = ''
              [ "''${SERVICE_RESULT:-}" != success ] || exit 0
              exec ${lib.getExe cfg.package} emergency-stop
            '';
          };
          systemd.services.togi-leave-tuning-boot = lib.mkIf cfg.tuning.leaveOnShutdown {
            description = "Return the next boot to the normal system after an orderly shutdown";
            wantedBy = [ "multi-user.target" ];
            before = [ "togi.service" ];
            restartIfChanged = false;
            unitConfig.RequiresMountsFor = grubenv;
            preStop = ''
              [ ! -e /run/togi-retry-tuning-boot ] || exit 0
              ${pkgs.grub2}/bin/grub-editenv ${grubenv} unset saved_entry
            '';
            serviceConfig = {
              Type = "oneshot";
              RemainAfterExit = true;
              ExecStart = "${pkgs.coreutils}/bin/true";
            };
          };
          console.font = lib.mkForce cfg.tuning.consoleFont;
          boot.kernelParams = lib.mkForce (
            lib.filter (p: builtins.match "console=tty[0-9]+(,.*)?" p == null) config.boot.kernelParams
            ++ [
              "console=tty3"
              "printk.always_kmsg_dump=1"
            ]
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
            wantedBy = lib.optional (cfg.tuning.display == "console") "multi-user.target";
            unitConfig.ConditionPathExists = "/dev/tty1";
            environment.TERM = "linux";
            serviceConfig = {
              ExecStart = "${lib.getExe cfg.package} watch --tuning-boot";
              StandardInput = "tty";
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
          fonts.packages = lib.mkIf (cfg.tuning.display == "kmscon") [ pkgs.dejavu_fonts ];
          fonts.fontconfig.enable = lib.mkIf (cfg.tuning.display == "kmscon") true;
          hardware.graphics.enable = lib.mkIf (cfg.tuning.display == "kmscon") true;
          systemd.services.togi-kmscon = lib.mkIf (cfg.tuning.display == "kmscon") {
            description = "togi dashboard in kmscon on tty1";
            wantedBy = [ "multi-user.target" ];
            after = [
              "systemd-user-sessions.service"
              "togi-kernel-log.service"
              "togi-watch.service"
            ];
            wants = [ "togi-kernel-log.service" ];
            conflicts = [ "togi-watch.service" ];
            onFailure = [ "togi-watch-fallback.service" ];
            unitConfig.ConditionPathExists = "/dev/tty1";
            startLimitIntervalSec = 180;
            startLimitBurst = 3;
            environment = {
              COLORTERM = "truecolor";
              TERM = "xterm-256color";
            };
            serviceConfig = {
              ExecStart = lib.escapeShellArgs (
                [
                  "${pkgs.kmscon}/bin/kmscon"
                  "--vt"
                  "1"
                  "--no-libseat"
                  "--switchvt"
                  "--no-reset-env"
                  "--no-issue"
                  "--no-blink"
                  "--no-mouse"
                  "--dpms-timeout"
                  "0"
                  "--oneshot"
                  "--session-max"
                  "1"
                  "--no-session-control"
                  "--sb-size"
                  "0"
                  "--font-engine"
                  "pango"
                  "--font-name"
                  "DejaVu Sans Mono"
                  "--font-size"
                  (toString cfg.tuning.kmscon.fontSize)
                  "--term"
                  "xterm-256color"
                  "--hwaccel"
                ]
                ++
                  lib.concatMap
                    (grab: [
                      "--grab-${grab}"
                      ""
                    ])
                    [
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
                    ]
                ++ [
                  "--login"
                  "--"
                  (lib.getExe cfg.package)
                  "watch"
                  "--tuning-boot"
                ]
              );
              # Direct VT/DRM access needs root; the child keeps the console watcher's existing root identity.
              StandardInput = "tty";
              StandardOutput = "journal";
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
          systemd.services.togi-watch-fallback = lib.mkIf (cfg.tuning.display == "kmscon") {
            description = "Fall back to the console after kmscon exhausts its starts";
            after = [ "togi-kmscon.service" ];
            serviceConfig.Type = "oneshot";
            script = ''
              [ "''${MONITOR_SERVICE_RESULT:-}" = start-limit-hit ] || exit 0
              exec ${config.systemd.package}/bin/systemctl --no-block start togi-watch.service
            '';
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
