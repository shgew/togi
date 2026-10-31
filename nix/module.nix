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
  systemdBoot = config.boot.loader.systemd-boot;
  esp = config.boot.loader.efi.efiSysMountPoint;
  boot = if systemdBoot.xbootldrMountPoint == null then esp else systemdBoot.xbootldrMountPoint;
  bootState = "/var/lib/togi-boot";
  tuningBoot = if grub.enable then grubenv else "/etc/togi/night.json";
  bootMounts =
    if grub.enable then
      grubenv
    else
      lib.concatStringsSep " " (
        [
          esp
          bootState
        ]
        ++ lib.optional (boot != esp) boot
      );
  bootTools = lib.optional grub.enable pkgs.grub2 ++ [ config.systemd.package ];
  clearBootSelection =
    if grub.enable then
      "${pkgs.grub2}/bin/grub-editenv ${grubenv} unset saved_entry"
    else
      ''
        ${config.systemd.package}/bin/bootctl --esp-path=${lib.escapeShellArg esp} set-default "" &&
        ${config.systemd.package}/bin/bootctl --esp-path=${lib.escapeShellArg esp} set-oneshot "" &&
        {
          saved=/sys/firmware/efi/efivars/LoaderEntryLastBooted-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f
          if [ -e "$saved" ]; then
            ${pkgs.e2fsprogs}/bin/chattr -i "$saved" &&
            ${pkgs.coreutils}/bin/rm "$saved"
          fi
        }
      '';
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
    tuning.enable = lib.mkEnableOption "the togi tuning boot, a GRUB or systemd-boot entry that tunes unattended";
    day.enable = lib.mkEnableOption "the togi permitted day-profile application and observer in the normal boot. It requires a CO 0 baseline and an armed, counting PID 1 hardware watchdog, applies the committed profile with all-core readback, and reapplies without promotion after restart or resume. Pause, stop and emergency recovery still zero offsets. It holds the host lock and journal writer lock while it runs, so `togi run` and `togi reset` fail lock-busy until it stops; `togi day record` hands records to the running service. It refuses to start without a session, never exists in the tuning boot, and keeps the journal on disk with a 5s sync interval";
    day.watchdog = lib.mkEnableOption "the PID 1 hardware watchdog in the normal boot with the day observer, RuntimeWatchdogSec=60s, so a hang resets the machine and the observer can record the unclean end. A unit ordered before sleep.target releases the watchdog before suspend or hibernation, reloads sp5100_tco after resume to restore its timer decoding after deep sleep, and re-arms PID 1's watchdog even if the reload fails. The pause is retained for s2idle, where the timer may keep counting. It sets no lockup panic kernel parameter and never applies in the tuning boot, which keeps its own 30s watchdog";
    day.users = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "alice" ];
      description = "Users who may answer questions about an unclean end and control day pause/resume and freeze/unfreeze without root, over /run/togi/day.sock. Each joins the togi group, which the module creates; /run/togi is root:togi 0750 and the socket 0660. Each also runs a graphical-session user service for questions, pause-expiry warnings and verified profile-reapplication notifications. Membership starts at the user's next login. The accounts are declared elsewhere in users.users, which validates them. The default delegates to nobody and leaves control to root. Only meaningful with day.enable.";
    };
    tuning.leaveOnShutdown = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Restore the normal boot selection (`togi night-end`) on an orderly operator shutdown or reboot of the tuning boot, so the next boot is the normal system. This is how the power button ends a night. Restart-limit retry reboots preserve the tuning entry; crash reboots return to it either way.";
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
        # The day service binds /run/togi/day.sock inside this directory. It is a tmpfiles directory, not the unit's
        # RuntimeDirectory, so the group stays togi and the directory outlives a service restart or a failure.
        systemd.tmpfiles.rules = [ "d /run/togi 0750 root togi -" ];
        users.groups.togi = { };
        users.users = lib.genAttrs cfg.day.users (_: {
          extraGroups = [ "togi" ];
        });
        # The notifier runs only in the graphical sessions of the named users: ConditionUser takes one name and
        # ConditionGroup would admit anyone added to togi by hand, so ExecCondition (exit 1 skips quietly) matches the list.
        # Restarts never run out: the day service may be down when the session starts. graphical-session.target keeps
        # its default dependencies, so it is already ordered after every unit it wants; ordering the notifier after it
        # too would be a cycle, and the notifier orders after graphical-session-pre.target instead.
        systemd.user.services.togi-day-notify = lib.mkIf (cfg.day.users != [ ]) {
          description = "togi day question and pause notifier";
          wantedBy = [ "graphical-session.target" ];
          partOf = [ "graphical-session.target" ];
          bindsTo = [ "graphical-session.target" ];
          after = [ "graphical-session-pre.target" ];
          startLimitIntervalSec = 0;
          path = [ pkgs.libnotify ];
          serviceConfig = {
            ExecCondition = pkgs.writeShellScript "togi-day-notify-condition" ''
              case "$(${pkgs.coreutils}/bin/id -un)" in
                ${lib.concatMapStringsSep "|" lib.escapeShellArg cfg.day.users}) exit 0 ;;
                *) exit 1 ;;
              esac
            '';
            ExecStart = "${lib.getExe cfg.package} day notify";
            Restart = "on-failure";
            RestartSec = 10;
          };
        };
        systemd.services.togi-day = {
          description = "togi permitted day-profile application and observer";
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
          # A killed process cannot run its journaled stop transaction. This hook has only zero
          # authority and does not need the journal; lock/identity/topology refusals still stand.
          postStop = ''
            [ "''${SERVICE_RESULT:-}" != success ] || exit 0
            exec ${lib.getExe cfg.package} emergency-stop
          '';
        };
        # Start before watchdog sleep so reverse stop ordering restores its counting timer before
        # the day process verifies and reapplies committed D without promotion. The hook never starts an inactive day service.
        systemd.services.togi-day-resume = {
          description = "Reapply togi permitted committed profile after sleep";
          wantedBy = [ "sleep.target" ];
          before = [
            "sleep.target"
            "togi-day-watchdog-sleep.service"
          ];
          unitConfig.StopWhenUnneeded = true;
          serviceConfig = {
            Type = "oneshot";
            RemainAfterExit = true;
            ExecStart = "${pkgs.coreutils}/bin/true";
            ExecStopPost = pkgs.writeShellScript "togi-day-resume" ''
              ${config.systemd.package}/bin/systemctl --quiet is-active togi-day.service || exit 0
              exec ${lib.getExe cfg.package} day reapply
            '';
          };
        };
      })
      (lib.mkIf (cfg.day.enable && cfg.day.watchdog) (
        let
          busctl = "${config.systemd.package}/bin/busctl";
          runtimeWatchdog =
            value:
            "${busctl} set-property org.freedesktop.systemd1 /org/freedesktop/systemd1 org.freedesktop.systemd1.Manager RuntimeWatchdogUSec t ${value}";
        in
        {
          boot.initrd.kernelModules = [ "sp5100_tco" ];
          systemd.settings.Manager.RuntimeWatchdogSec = lib.mkDefault "60s";
          # Writing 0 to the manager's RuntimeWatchdogUSec disarms and closes the device; writing infinity drops
          # the override and re-arms the configured RuntimeWatchdogSec. sleep.target stops after resume, which
          # stops this unit. The restore is ExecStopPost, not ExecStop: systemd skips ExecStop when the start
          # failed, but PID 1 may have applied the 0 before busctl failed, and the restore is harmless after a
          # start that applied nothing.
          systemd.services.togi-day-watchdog-sleep = {
            description = "Pause PID 1's hardware watchdog and restore SP5100 after sleep";
            wantedBy = [ "sleep.target" ];
            before = [ "sleep.target" ];
            unitConfig.StopWhenUnneeded = true;
            serviceConfig = {
              Type = "oneshot";
              RemainAfterExit = true;
              TimeoutStopSec = "30s";
              ExecStart = runtimeWatchdog "0";
              # Firmware can clear the SP5100 timer's MMIO decode on deep resume. Probe restores it.
              # A failed or stalled module command must not skip re-arming: '-' ignores failures, while
              # timeout kills each command within 7s, well before systemd's 30s stop deadline.
              ExecStopPost = [
                "-${pkgs.coreutils}/bin/timeout --verbose --kill-after=2s 5s ${pkgs.kmod}/bin/modprobe -r sp5100_tco"
                "-${pkgs.coreutils}/bin/timeout --verbose --kill-after=2s 5s ${pkgs.kmod}/bin/modprobe sp5100_tco"
                (runtimeWatchdog "18446744073709551615")
              ];
            };
          };
        }
      ))
      (lib.mkIf cfg.tuning.enable {
        assertions = [
          {
            assertion = grub.enable != systemdBoot.enable;
            message = "services.togi.tuning.enable requires exactly one supported boot loader: GRUB or systemd-boot";
          }
          {
            assertion = !grub.enable || builtins.length grub.mirroredBoots == 1;
            message = "services.togi.tuning.enable with GRUB requires exactly one mirroredBoots entry: togi's tuning boot supports a single GRUB environment";
          }
        ];
        boot.loader.grub.default = lib.mkIf grub.enable "saved";
        # The same setup file is read in the normal boot and its tuning specialisation.
        environment.systemPackages = bootTools;
        systemd.tmpfiles.rules = lib.optional systemdBoot.enable "d ${bootState} 0700 root root -";
        environment.etc."togi/night.json".source = json.generate "togi-night.json" (
          if grub.enable then
            {
              loader = "grub";
              grubenv = toString grubenv;
              entry = "${config.system.nixos.distroName} - togi";
            }
          else
            {
              loader = "systemd-boot";
              inherit esp boot;
              state = bootState;
              specialisation = "togi";
            }
        );
        specialisation.togi.configuration = {
          # The tuning boot inherits the normal configuration; the observer is a normal-boot service.
          services.togi.day.enable = lib.mkForce false;
          system.nixos.tags = [ "togi" ];
          boot.loader.grub.configurationName = lib.mkIf grub.enable "togi";
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
            unitConfig.RequiresMountsFor = bootMounts;
            path = [ pkgs.coreutils ] ++ bootTools;
            serviceConfig.Type = "oneshot";
            script = ''
              [ "''${MONITOR_SERVICE_RESULT:-}" = start-limit-hit ] || exit 0
              if ${lib.getExe cfg.package} restart-limit --tuning-boot ${tuningBoot}; then
                exit 0
              fi
              rm -f /run/togi-retry-tuning-boot || true
              ${
                if grub.enable then
                  ''
                    grub-editenv ${grubenv} set "togi_leave_reason={\"id\":\"fallback-$INVOCATION_ID\",\"count\":0,\"reason\":\"restart-limit command failed; returning to the normal system\"}" || true
                    ${clearBootSelection}
                  ''
                else
                  ''
                    ${lib.getExe cfg.package} night-end || {
                      ${clearBootSelection}
                    }
                  ''
              }
              systemctl reboot
            '';
          };
          systemd.services.togi = {
            description = "togi tuning boot";
            onFailure = [ "togi-restart-limit.service" ];
            unitConfig.RequiresMountsFor = bootMounts;
            wantedBy = [ "multi-user.target" ];
            after = [
              "systemd-modules-load.service"
              "systemd-pstore.service"
              "local-fs.target"
            ];
            path = bootTools;
            startLimitIntervalSec = 1800;
            startLimitBurst = 3;
            serviceConfig = {
              ExecStart = "${lib.getExe cfg.package} run --tuning-boot ${tuningBoot}";
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
            unitConfig.RequiresMountsFor = bootMounts;
            path = bootTools;
            preStop = ''
              [ ! -e /run/togi-retry-tuning-boot ] || exit 0
              ${lib.getExe cfg.package} night-end || {
                ${clearBootSelection}
              }
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
            # F10 stops the run, restores the boot selection and reboots.
            path = bootTools;
            environment.TERM = "linux";
            serviceConfig = {
              Type = "exec";
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
            path = bootTools;
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
