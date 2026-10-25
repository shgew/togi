{
  pkgs,
  package,
  trialTests,
  # The executable of crates/togi/tests/vm_recovery.rs at bin/togi-vm-recovery: `togi` with a persistent fake SMU.
  vmRecovery,
}:
let
  module = import ./module.nix { packages.${pkgs.stdenv.hostPlatform.system}.default = package; };
  # The tuning VM tests share this node: the NixOS module with the tuning specialisation, GRUB on a virtual disk
  # and an i6300esb watchdog that resets the machine. Their nodes disable DHCP.
  tuningBoot =
    { pkgs, lib, ... }:
    {
      imports = [ module ];
      virtualisation.useBootLoader = true;
      networking.useDHCP = false;
      boot.loader.grub.enable = true;
      boot.loader.timeout = 1;
      virtualisation.qemu.options = [
        "-device i6300esb"
        "-watchdog-action reset"
      ];
      specialisation.togi.configuration = {
        boot.initrd.kernelModules = [ "i6300esb" ];
        # The module check pins the shipped 30s; only the test waits less for a reset.
        systemd.settings.Manager.RuntimeWatchdogSec = lib.mkForce "10s";
      };
      services.togi = {
        enable = true;
        tuning.enable = true;
      };
      hardware.cpu.amd.ryzen-smu.enable = false;
      environment.systemPackages = [ pkgs.grub2 ];
    };
in
{
  # Runs the shipped package under real systemd with a GRUB tuning boot and the retained trial scope tests
  # (trial-scope-tests) as root and as the delegated hardware user, without stress backends or SMU
  # access. Only the tuning specialisation's watchdog is overridden, to 10s (the module check pins the
  # shipped 30s); the test still freezes PID 1 and observes the real hardware watchdog reset.
  tuning-boot = pkgs.testers.runNixOSTest {
    name = "togi-tuning-boot";

    nodes.machine = {
      imports = [ tuningBoot ];
      console.font = "Lat2-Terminus16";
      # The guest reports each vCPU as its own CCD and the tuner refuses a topology of more than three.
      virtualisation.cores = 3;
      services.togi.hardwareTestGroup = "togi-hardware";
      # Exercise the shipped normal-boot sleep unit without needing SP5100 or actual ACPI sleep.
      services.togi.day.enable = true;
      services.togi.day.watchdog = true;
      users.groups.togi-hardware = { };
      users.users.togi-hardware = {
        isNormalUser = true;
        group = "togi-hardware";
      };
      systemd.services."user@".serviceConfig.Delegate = "cpu cpuset io memory pids";
      environment.systemPackages = [ trialTests ];
    };

    testScript = ''
      import json
      import time

      # Completion trials need a real SMU; controller clock trials need real cpufreq policies.
      hardware = (
          "hardware:: --skip hardware::helper --skip every_r1_and_r2_workload_passes "
          "--skip an_r8_trial --skip an_r9_trial "
          "--skip managed_mapping_caps_idle_ccd_policies --skip managed_all_core_trial_rotates_controller_host"
      )
      pin = "hardware::a_helper_cannot_pin_outside_its_scope"
      flags = "--test-threads 1 --nocapture"
      scopes = "systemctl {manager} list-units --all 'togi-*.scope' --plain --no-legend"


      def run_scope_tests(prefix, selection, manager):
          log = "/tmp/scope-tests.log"
          status, _ = machine.execute(f"{prefix} togi-trial-tests {selection} {flags} > {log} 2>&1")
          output = machine.succeed(f"cat {log}")
          machine.log(output)
          assert status == 0, output
          left = machine.succeed(f"{prefix} {scopes.format(manager=manager)}").strip()
          assert left == "", left


      machine.start(allow_reboot=True)
      machine.wait_for_unit("multi-user.target")
      assert json.loads(machine.succeed("cat /etc/togi/config.json"))["backend_user"] == "togi-trial"
      machine.log(machine.wait_until_succeeds("systemd-analyze critical-chain multi-user.target"))
      run_scope_tests("", hardware, "")
      lock_identity = machine.succeed("stat -c '%d:%i' /run/lock/togi.lock").strip()
      assert machine.succeed("stat -c '%U:%G:%a' /run/lock/togi.lock").strip() == "root:togi-hardware:660"
      machine.succeed("loginctl enable-linger togi-hardware")
      delegated_uid = machine.succeed("id -u togi-hardware").strip()
      machine.wait_for_unit(f"user@{delegated_uid}.service")
      delegated_prefix = (
          f"runuser -u togi-hardware -- env XDG_RUNTIME_DIR=/run/user/{delegated_uid} "
          f"DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/{delegated_uid}/bus"
      )
      run_scope_tests(delegated_prefix, pin, "--user")
      assert machine.succeed("stat -c '%d:%i' /run/lock/togi.lock").strip() == lock_identity
      assert machine.succeed("stat -c '%U:%G:%a' /run/lock/togi.lock").strip() == "root:togi-hardware:660"
      normal_system = machine.succeed("readlink -f /run/current-system").strip()
      tuning_system = machine.succeed("readlink -f /run/current-system/specialisation/togi").strip()
      assert tuning_system != normal_system, (normal_system, tuning_system)
      machine.succeed("! systemctl is-active --quiet togi.service")
      machine.wait_until_succeeds("grep -qx active /sys/class/watchdog/watchdog0/state")
      machine.succeed(
          "systemd-run --unit=togi-sleep-hold --property=Wants=sleep.target sleep infinity"
      )
      machine.wait_for_unit("togi-day-watchdog-sleep.service")
      machine.succeed("grep -qx inactive /sys/class/watchdog/watchdog0/state")
      machine.succeed(
          "test \"$(busctl get-property org.freedesktop.systemd1 /org/freedesktop/systemd1 "
          "org.freedesktop.systemd1.Manager RuntimeWatchdogUSec)\" = 't 0'"
      )
      # Stopping sleep.target follows the resume path. SP5100 cannot probe on i6300esb; its failure
      # must not prevent PID 1 re-arming the existing hardware watchdog.
      machine.succeed("systemctl stop sleep.target togi-sleep-hold.service")
      machine.wait_until_succeeds("grep -qx active /sys/class/watchdog/watchdog0/state")
      machine.succeed("! systemctl is-failed --quiet togi-day-watchdog-sleep.service")
      machine.succeed(
          "journalctl -b -u togi-day-watchdog-sleep.service -o cat --no-pager | "
          "grep -F 'sp5100_tco'"
      )
      machine.succeed("grep -q '^FONT=Lat2-Terminus16' /etc/vconsole.conf")
      machine.succeed("test -f /boot/grub/grub.cfg")
      machine.succeed("grep -Fq 'menuentry \"NixOS - togi\"' /boot/grub/grub.cfg")
      machine.succeed("grep -Fq 'set default=\"''${saved_entry}\"' /boot/grub/grub.cfg")
      machine.succeed("grub-set-default 'NixOS - togi'")
      assert "saved_entry=NixOS - togi" in machine.succeed("grub-editenv /boot/grub/grubenv list")
      machine.reboot()
      machine.wait_for_unit("multi-user.target")
      machine.log(machine.wait_until_succeeds("systemd-analyze critical-chain multi-user.target"))
      booted_system = machine.succeed("readlink -f /run/current-system").strip()
      assert booted_system == tuning_system, (booted_system, tuning_system)
      machine.wait_until_succeeds("grep -qx active /sys/class/watchdog/watchdog0/state")
      machine.succeed("grep -qx 'i6300ESB timer' /sys/class/watchdog/watchdog0/identity")
      # PID 1 may close an unrelated descriptor between the glob and its readlink.
      pid1_files = machine.succeed("readlink /proc/1/fd/* || true").splitlines()
      assert any(f.startswith("/dev/watchdog") for f in pid1_files), pid1_files
      machine.wait_until_succeeds("systemctl is-failed togi.service")
      status = machine.succeed("systemctl show togi.service -p ExecMainStatus --value").strip()
      assert status == "15", f"togi.service exited {status}, want 15 (dead end preflight)"
      restarts = machine.succeed("systemctl show togi.service -p NRestarts --value").strip()
      assert restarts == "0", f"togi.service restarted {restarts} times after a dead end"
      grubenv = machine.succeed("grub-editenv /boot/grub/grubenv list")
      assert "saved_entry" not in grubenv, f"saved_entry left in grubenv: {grubenv}"
      events = [
          json.loads(line)
          for line in machine.succeed("togi events --json --kind deadend,boot.saved_entry").splitlines()
      ]
      assert any(
          e["kind"] == "deadend"
          and e["data"]["condition"] == "preflight"
          and e["data"]["action"] == "clear_saved_entry"
          for e in events
      ), events
      assert any(
          e["kind"] == "boot.saved_entry"
          and e["data"]["before"] == "NixOS - togi"
          and e["data"]["after"] == ""
          for e in events
      ), events
      preflight = [
          json.loads(line)
          for line in machine.succeed("togi events --json --kind preflight.check").splitlines()
      ]
      watchdog = [e for e in preflight if e["data"]["check"] == "watchdog"]
      # i6300esb has no get_timeleft operation: active alone cannot establish counting.
      # Its real reset is still exercised below; SP5100 decoding recovery needs hardware.
      assert len(watchdog) == 1 and not watchdog[0]["data"]["ok"], preflight
      assert "open but not counting" in watchdog[0]["data"]["detail"], watchdog
      assert preflight[0]["data"]["check"] == "watchdog", preflight
      machine.log(f"unreadable watchdog counter preflight: {watchdog[0]}")
      # The dead end exit 15 is abnormal, so the ExecStopPost hook runs; the VM has no SMU, so it refuses before any
      # write and says offsets are unchanged.
      machine.succeed("journalctl -b -u togi.service -o cat --no-pager | grep -F 'emergency-stop:' | grep -F 'offsets unchanged'")
      machine.succeed("test -z \"$(togi events --json --kind smu.intent,trial.intent)\"")
      machine.wait_for_unit("togi-watch.service")
      machine.fail("grep -q '^FONT=' /etc/vconsole.conf")
      machine.wait_until_succeeds("grep -aq 'preflight: failed checks' /dev/vcs1")
      machine.succeed("systemctl kill --signal=SIGSTOP togi-watch.service")
      machine.succeed("echo '<0>togi-kmsg-probe' > /dev/kmsg")
      machine.wait_until_succeeds("grep -aq togi-kmsg-probe /dev/vcs3")
      machine.fail("grep -aq togi-kmsg-probe /dev/vcs1")
      machine.succeed("systemctl kill --signal=SIGCONT togi-watch.service")
      machine.sleep(3)
      for unit in ["togi-watch.service", "togi-console.service"]:
          state = machine.succeed(f"systemctl show {unit} -p ActiveState -p NRestarts").split()
          assert state == ["ActiveState=active", "NRestarts=0"], f"{unit}: {state}"
      contention_boot = machine.succeed("cat /proc/sys/kernel/random/boot_id").strip()
      contention_inode = machine.succeed("stat -c '%d:%i' /run/lock/togi.lock").strip()
      contention_state = machine.succeed("find /var/lib/togi -type f -exec sha256sum {} + | sort")
      machine.succeed("grub-set-default 'NixOS - togi'; chmod 0644 /run/lock/togi.lock")
      machine.succeed(
          "systemd-run --unit=togi-test-holder --property=Type=exec /bin/sh -c "
          "'set -e; exec 9</run/lock/togi.lock; ${pkgs.util-linux}/bin/flock --exclusive 9; "
          "${pkgs.coreutils}/bin/touch /run/togi-test-held; exec ${pkgs.coreutils}/bin/sleep infinity'"
      )
      machine.wait_until_succeeds("test -f /run/togi-test-held", timeout=10)
      machine.succeed(
          "mkdir -p /run/systemd/system/togi.service.d; "
          "printf '[Service]\\nRestartSec=1s\\n' > /run/systemd/system/togi.service.d/contention.conf; "
          "systemctl daemon-reload; systemctl reset-failed togi.service"
      )
      machine.succeed("systemctl start togi.service || true")
      machine.wait_until_succeeds("systemctl is-failed togi.service")
      assert machine.succeed("systemctl show togi.service -p ExecMainStatus --value").strip() == "3"
      machine.sleep(5)
      assert machine.succeed("systemctl show togi.service -p NRestarts --value").strip() == "0"
      assert machine.succeed("cat /proc/sys/kernel/random/boot_id").strip() == contention_boot
      assert "saved_entry=NixOS - togi" in machine.succeed("grub-editenv /boot/grub/grubenv list")
      assert machine.succeed("find /var/lib/togi -type f -exec sha256sum {} + | sort") == contention_state
      assert machine.succeed("stat -c '%d:%i' /run/lock/togi.lock").strip() == contention_inode
      assert machine.succeed("stat -c '%a' /run/lock/togi.lock").strip() == "600"
      machine.succeed("systemctl stop togi-test-holder.service")
      machine.succeed("grub-set-default 'NixOS - togi' && sync")
      frozen_boot = machine.succeed("cat /proc/sys/kernel/random/boot_id").strip()
      # Freeze only PID 1, not QEMU's clock or the driver's shell. The armed
      # hardware watchdog must reset the guest without an orderly shutdown.
      machine.succeed("grep -qx 1 /sys/fs/cgroup/init.scope/cgroup.procs")
      assert machine.qmp_client is not None
      machine.qmp_client.send("query-status")
      list(machine.qmp_client.events())
      machine.log(f"freezing PID 1 at {time.time():.6f}")
      machine.succeed("echo 1 > /sys/fs/cgroup/init.scope/cgroup.freeze")
      machine.wait_until_succeeds(
          "grep -qx 'frozen 1' /sys/fs/cgroup/init.scope/cgroup.events", timeout=10
      )
      watchdog_event = None
      deadline = time.monotonic() + 30
      while time.monotonic() < deadline and watchdog_event is None:
          machine.qmp_client.send("query-status")
          for event in machine.qmp_client.events():
              if event["event"] == "WATCHDOG":
                  watchdog_event = event
          time.sleep(0.1)
      assert watchdog_event is not None, "freezing PID 1 did not trip the hardware watchdog"
      assert watchdog_event["data"]["action"] == "reset", watchdog_event
      machine.log(f"hardware watchdog reset: {watchdog_event}")
      machine.connected = False
      machine.wait_for_unit("multi-user.target")
      reset_boot = machine.succeed("cat /proc/sys/kernel/random/boot_id").strip()
      assert reset_boot != frozen_boot, (frozen_boot, reset_boot)
      booted_system = machine.succeed("readlink -f /run/current-system").strip()
      assert booted_system == tuning_system, f"crash left the tuning boot: {booted_system}"
      machine.wait_until_succeeds("systemctl is-failed togi.service")
      machine.succeed("grub-set-default 'NixOS - togi' && sync")
      machine.reboot()
      machine.wait_for_unit("multi-user.target")
      assert machine.succeed("readlink -f /run/current-system").strip() == normal_system
      machine.succeed("! systemctl is-active --quiet togi.service")
    '';
  };

  kmscon-display = pkgs.testers.runNixOSTest {
    name = "togi-kmscon-display";
    enableOCR = true;
    nodes.machine = {
      imports = [ tuningBoot ];
      virtualisation.cores = 3;
      virtualisation.memorySize = 2048;
      virtualisation.qemu.options = [
        "-vga none"
        "-device virtio-gpu-pci,xres=2560,yres=1440"
      ];
      virtualisation.resolution = {
        x = 2560;
        y = 1440;
      };
      boot.initrd.kernelModules = [ "virtio_gpu" ];
      services.togi.tuning.display = "kmscon";
    };
    testScript = ''
      machine.start(allow_reboot=True)
      machine.wait_for_unit("multi-user.target")
      tuning_system = machine.succeed("readlink -f /run/current-system/specialisation/togi").strip()
      machine.succeed("grub-set-default 'NixOS - togi'")
      machine.reboot()
      machine.wait_for_unit("multi-user.target")
      assert machine.succeed("readlink -f /run/current-system").strip() == tuning_system
      machine.wait_for_unit("togi-kmscon.service")
      machine.wait_until_succeeds("pgrep -f '^/nix/store/[^ ]*/bin/togi watch --tuning-boot$'")
      kmscon_pid = machine.succeed("systemctl show togi-kmscon.service -p MainPID --value").strip()
      watch_pid = machine.succeed(f"pgrep -P {kmscon_pid} -x togi").strip()
      assert watch_pid, "kmscon did not launch the shipped watcher"
      machine.succeed(f"grep -zqx 'COLORTERM=truecolor' /proc/{watch_pid}/environ")
      machine.succeed(f"grep -zqx 'TERM=xterm-256color' /proc/{watch_pid}/environ")
      machine.succeed(f"test $(stat -c %u /proc/{watch_pid}) -eq 0")
      machine.succeed("test $(fgconsole) -eq 1")
      machine.succeed(f"test $(readlink /proc/{kmscon_pid}/fd/0) = /dev/tty1")
      machine.fail("systemctl is-active --quiet togi-watch.service")
      machine.wait_for_text("preflight", timeout=120)
      machine.screenshot("kmscon-dashboard")
      tuner_before = machine.succeed("systemctl show togi.service -p InvocationID -p ActiveState -p NRestarts")
      boot_before = machine.succeed("cat /proc/sys/kernel/random/boot_id")

      with subtest("kmscon lockdown leaves the dashboard and its single session intact"):
          for key in [
              "ctrl-meta_r-backspace", "ctrl-meta_r-ret",
              "ctrl-meta_r-left", "ctrl-meta_r-right",
              "shift-up", "shift-down", "shift-pgup", "shift-pgdn",
              "ctrl-minus", "ctrl-equal", "meta_r-equal", "meta_r-minus",
              "q", "ctrl-c", "ctrl-d",
          ]:
              machine.send_key(key)
          assert machine.succeed("systemctl show togi-kmscon.service -p MainPID --value").strip() == kmscon_pid
          assert machine.succeed(f"pgrep -P {kmscon_pid} -x togi").strip() == watch_pid
          machine.wait_for_text("preflight")

      with subtest("Ctrl Alt Fn reaches the login and logs while kmscon owns tty1"):
          machine.send_key("ctrl-alt-f2")
          machine.wait_until_succeeds("test $(fgconsole) -eq 2")
          machine.wait_for_unit("getty@tty2.service")
          machine.send_key("ctrl-alt-f3")
          machine.wait_until_succeeds("test $(fgconsole) -eq 3")
          machine.succeed("systemctl is-active --quiet togi-console.service")
          machine.send_key("ctrl-alt-f1")
          machine.wait_until_succeeds("test $(fgconsole) -eq 1")
          machine.wait_for_text("preflight")

      with subtest("three killed kmscon starts fall back to the Linux console"):
          # Fresh start-limit window, independent of how long the earlier subtests took.
          machine.succeed("systemctl reset-failed togi-kmscon.service && systemctl restart togi-kmscon.service")
          machine.wait_until_succeeds(
              "systemctl is-active --quiet togi-kmscon.service && "
              "test $(systemctl show togi-kmscon.service -p NRestarts --value) -eq 0"
          )
          machine.wait_until_succeeds("pgrep -f '^/nix/store/[^ ]*/bin/togi watch --tuning-boot$'")
          machine.fail("systemctl is-active --quiet togi-watch.service")
          for attempt in range(3):
              machine.succeed("systemctl kill --kill-whom=main --signal=SIGKILL togi-kmscon.service")
              if attempt < 2:
                  machine.wait_until_succeeds(
                      "systemctl is-active --quiet togi-kmscon.service && "
                      f"test $(systemctl show togi-kmscon.service -p NRestarts --value) -eq {attempt + 1}"
                  )
                  machine.fail("systemctl is-active --quiet togi-watch.service")
          machine.wait_until_succeeds(
              "test $(systemctl show togi-kmscon.service -p Result --value) = start-limit-hit"
          )
          machine.wait_for_unit("togi-watch.service")
          console_pid = machine.succeed("systemctl show togi-watch.service -p MainPID --value").strip()
          machine.succeed(f"test $(readlink /proc/{console_pid}/fd/0) = /dev/tty1")
          machine.succeed(f"grep -zqx 'TERM=linux' /proc/{console_pid}/environ")
          machine.wait_until_succeeds("grep -aq 'preflight: failed checks' /dev/vcs1")
          machine.screenshot("console-fallback")
          assert machine.succeed("systemctl show togi.service -p InvocationID -p ActiveState -p NRestarts") == tuner_before
          assert machine.succeed("cat /proc/sys/kernel/random/boot_id") == boot_before
          machine.log(machine.succeed("journalctl -b -u togi-kmscon -u togi-watch-fallback -u togi-watch --no-pager"))
    '';
  };

  # The host lock on a normal boot of the module with a hardware-test group: nix/provision-lock.py, the program
  # docs/howto.md gives manual installs, on fresh, legacy, held and symlinked locks; an unrelated user's denial;
  # and the installed `togi`'s refusals of a foreign owner, a read-only lock and a held lock. Its checks are exit
  # statuses, inodes, modes and contents, not message text.
  host-lock = pkgs.testers.runNixOSTest {
    name = "togi-host-lock";

    nodes.machine = {
      imports = [ module ];
      networking.useDHCP = false;
      services.togi = {
        enable = true;
        hardwareTestGroup = "togi-hardware";
      };
      hardware.cpu.amd.ryzen-smu.enable = false;
      users.groups.togi-hardware = { };
      users.users.togi-hardware = {
        isNormalUser = true;
        group = "togi-hardware";
      };
      users.users.unrelated.isNormalUser = true;
      environment.etc."togi-provision-lock.py".source = ./provision-lock.py;
      environment.systemPackages = [
        pkgs.util-linux
        pkgs.python3
      ];
    };

    testScript = ''
      lock = "/run/lock/togi.lock"
      provision = "python3 /etc/togi-provision-lock.py"


      def stat(path, format):
          return machine.succeed(f"stat -c '{format}' {path}").strip()


      def exits(command, want):
          status, output = machine.execute(f"{command} 2>&1")
          machine.log(output)
          assert status == want, (status, output)


      machine.wait_for_unit("multi-user.target")
      lock_identity = stat(lock, "%d:%i")
      assert stat(lock, "%U:%G:%a") == "root:togi-hardware:660"

      provision_dir = machine.succeed("mktemp -d /run/togi-provision.XXXXXX").strip()
      try:
          for initial_mode in [None, "600", "644"]:
              for group, expected_owner in [("-", "root:root:600"), ("togi-hardware", "root:togi-hardware:660")]:
                  fixture = f"{provision_dir}/lock-{initial_mode}-{group}"
                  if initial_mode is not None:
                      machine.succeed(f"printf provision-content > {fixture}; chmod {initial_mode} {fixture}")
                      identity = stat(fixture, "%d:%i")
                  machine.succeed(f"{provision} {fixture} {group}")
                  assert stat(fixture, "%U:%G:%a") == expected_owner
                  if initial_mode is not None:
                      assert stat(fixture, "%d:%i") == identity
                      assert machine.succeed(f"cat {fixture}") == "provision-content"
                  machine.log(f"provisioning: initial={initial_mode}, group={group}, result={expected_owner}")

          held = f"{provision_dir}/held-legacy"
          machine.succeed(f"printf held-content > {held}; chmod 0644 {held}")
          held_before = machine.succeed(f"stat -c '%d:%i:%U:%G:%a:%h' {held}; sha256sum {held}")
          # Keep an old read-only descriptor locked while running the program.
          machine.succeed(
              f"(exec 9<{held}; flock --exclusive 9; "
              f"if {provision} {held} togi-hardware; then exit 1; fi; "
              f"! flock --exclusive --nonblock {held} true)"
          )
          assert machine.succeed(f"stat -c '%d:%i:%U:%G:%a:%h' {held}; sha256sum {held}") == held_before
          machine.succeed(f"flock --exclusive --nonblock {held} true")

          target = f"{provision_dir}/symlink-target"
          symlink = f"{provision_dir}/symlink-lock"
          machine.succeed(f"printf target-content > {target}; chmod 0644 {target}; ln -s {target} {symlink}")
          target_before = machine.succeed(f"stat -c '%d:%i:%U:%G:%a:%h' {target}; sha256sum {target}")
          machine.fail(f"{provision} {symlink} togi-hardware")
          assert machine.succeed(f"stat -c '%d:%i:%U:%G:%a:%h' {target}; sha256sum {target}") == target_before
          assert machine.succeed(f"readlink {symlink}").strip() == target
      finally:
          machine.succeed(f"rm -rf -- {provision_dir}")
      assert stat(lock, "%d:%i") == lock_identity
      assert stat(lock, "%U:%G:%a") == "root:togi-hardware:660"

      machine.fail("runuser -u unrelated -- touch /run/lock/unrelated-lock")
      machine.fail(f"runuser -u unrelated -- flock --nonblock {lock} true")

      # The refusals run while another process holds the lock: `reset` checks the lock before taking it and exits 1
      # with no journal once it has it, so only a held lock tells a refusal (1) from a wrongly accepted lock (3).
      machine.succeed(
          "systemd-run --unit=togi-test-holder --property=Type=exec /bin/sh -c "
          f"'set -e; exec 9<{lock}; ${pkgs.util-linux}/bin/flock --exclusive 9; "
          "${pkgs.coreutils}/bin/touch /run/togi-test-held; exec ${pkgs.coreutils}/bin/sleep infinity'"
      )
      machine.wait_until_succeeds("test -f /run/togi-test-held", timeout=10)

      machine.succeed(f"chown unrelated {lock}")
      foreign_identity = stat(lock, "%d:%i:%u:%g:%a")
      machine.succeed(f"systemd-tmpfiles --create --prefix={lock}")
      exits("togi --state-dir /tmp/foreign-lock-state reset --all", 1)
      assert stat(lock, "%d:%i:%u:%g:%a") == foreign_identity
      machine.succeed(f"test ! -e /tmp/foreign-lock-state; chown root {lock}")

      machine.succeed(f"chmod 0640 {lock}")
      exits("runuser -u togi-hardware -- togi --state-dir /tmp/readonly-lock-state reset --all", 1)
      assert stat(lock, "%a") == "640"
      machine.succeed(f"test ! -e /tmp/readonly-lock-state; chmod 0644 {lock}")

      machine.succeed(f"systemd-tmpfiles --create --prefix={lock}")
      assert stat(lock, "%d:%i") == lock_identity
      assert stat(lock, "%a") == "644"
      for state_dir in ["/tmp/host-lock-state-a", "/tmp/host-lock-state-b"]:
          exits(f"togi --state-dir {state_dir} reset --all", 3)
          machine.succeed(f"test ! -e {state_dir}")
      assert stat(lock, "%d:%i") == lock_identity
      machine.succeed("systemctl stop togi-test-holder.service")
      # Without the holder the lock is taken and `reset` goes on to find no journal.
      exits("togi --state-dir /tmp/released-lock-state reset --all", 1)
      assert stat(lock, "%d:%i") == lock_identity
    '';
  };

  trial-scope = pkgs.testers.runNixOSTest {
    name = "togi-trial-scope";

    nodes.machine = {
      imports = [ tuningBoot ];
      virtualisation.cores = 4;
      services.togi.hardwareTestGroup = "togi-hardware";
      users.groups.togi-hardware = { };
      users.users.togi-hardware = {
        isNormalUser = true;
        group = "togi-hardware";
      };
      systemd.services."user@".serviceConfig.Delegate = "cpu cpuset io memory pids";
      environment.systemPackages = [ trialTests ];
    };

    testScript = ''
      import json

      # Stock-offset completion and controller clock trials need real SMU/cpufreq hardware.
      hardware = (
          "hardware:: --skip hardware::helper --skip every_r1_and_r2_workload_passes "
          "--skip an_r8_trial --skip an_r9_trial "
          "--skip managed_mapping_caps_idle_ccd_policies --skip managed_all_core_trial_rotates_controller_host"
      )
      delegated_hardware = f"{hardware} --skip a_backend_runs_as_the_account --skip the_sweep_removes_orphaned"
      flags = "--test-threads 1 --nocapture"
      scopes = "systemctl {manager} list-units --all 'togi-*.scope' --plain --no-legend"


      def run_scope_tests(prefix, binary, selection, manager):
          log = "/tmp/scope-tests.log"
          status, _ = machine.execute(f"{prefix} {binary} {selection} {flags} > {log} 2>&1")
          output = machine.succeed(f"cat {log}")
          machine.log(output)
          assert status == 0, output
          left = machine.succeed(f"{prefix} {scopes.format(manager=manager)}").strip()
          assert left == "", left


      machine.wait_for_unit("multi-user.target")
      assert json.loads(machine.succeed("cat /etc/togi/config.json"))["backend_user"] == "togi-trial"
      assert machine.succeed("stat -c '%U:%G:%a' /run/lock/togi.lock").strip() == "root:togi-hardware:660"
      lock_identity = machine.succeed("stat -c '%d:%i' /run/lock/togi.lock").strip()
      for binary, selection, count in [
          ("togi-trial-tests", hardware, 13),
          ("togi-trial-tests", delegated_hardware, 11),
          ("togi-trial-cached-errors", "cached_errors::", 2),
      ]:
          listed = machine.succeed(f"{binary} {selection} --list").splitlines()
          assert sum(line.endswith(": test") for line in listed) == count, listed
      run_scope_tests("", "togi-trial-tests", hardware, "")
      assert machine.succeed("stat -c '%d:%i' /run/lock/togi.lock").strip() == lock_identity
      machine.succeed("loginctl enable-linger togi-hardware")
      delegated_uid = machine.succeed("id -u togi-hardware").strip()
      machine.wait_for_unit(f"user@{delegated_uid}.service")
      delegated_prefix = (
          f"runuser -u togi-hardware -- env XDG_RUNTIME_DIR=/run/user/{delegated_uid} "
          f"DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/{delegated_uid}/bus"
      )
      run_scope_tests(delegated_prefix, "togi-trial-tests", delegated_hardware, "--user")
      run_scope_tests(delegated_prefix, "togi-trial-cached-errors", "cached_errors::", "--user")
      assert machine.succeed("stat -c '%d:%i' /run/lock/togi.lock").strip() == lock_identity
      assert machine.succeed("stat -c '%U:%G:%a' /run/lock/togi.lock").strip() == "root:togi-hardware:660"
    '';
  };

  # Systemd's restart-limit recovery with a stub package, so changes to the real binary reuse the cached
  # result: `run` and `restart-limit` exit 2 and other invocations sleep. It checks three failed starts, the
  # fallback leave reason when `restart-limit` itself fails, recovery to the normal generation, GRUB's saved
  # entry clearing and host-lock ownership.
  restart-limit = pkgs.testers.runNixOSTest {
    name = "togi-restart-limit";

    nodes.restartLimit =
      { lib, ... }:
      {
        imports = [ tuningBoot ];
        services.togi.package = pkgs.writeShellScriptBin "togi" ''
          if [ "$1" = run ]; then
            exit 2
          fi
          if [ "$1" = restart-limit ]; then
            exit 2
          fi
          if [ "$1" = emergency-stop ]; then
            echo "stub emergency-stop"
            exit 0
          fi
          exec ${pkgs.coreutils}/bin/sleep infinity
        '';
        services.togi.tuning.leaveOnShutdown = false;
        specialisation.togi.configuration = {
          systemd.services.togi.serviceConfig.RestartSec = lib.mkForce 1;
        };
      };

    testScript = ''
      import json

      restartLimit.start(allow_reboot=True)
      restartLimit.wait_for_unit("multi-user.target")
      normal_restart_system = restartLimit.succeed("readlink -f /run/current-system").strip()
      assert restartLimit.succeed("stat -c '%U:%G:%a' /run/lock/togi.lock").strip() == "root:root:600"
      tuning_restart_system = restartLimit.succeed(
          "readlink -f /run/current-system/specialisation/togi"
      ).strip()
      restartLimit.succeed("grub-set-default 'NixOS - togi' && sync")
      restartLimit.reboot()
      restartLimit.wait_for_console_text("reboot: Restarting system", timeout=120)
      restartLimit.wait_for_console_text("reboot: Restarting system", timeout=120)
      restartLimit.crash()
      restartLimit.start(allow_reboot=True)
      restartLimit.wait_for_unit("multi-user.target")
      booted_restart_system = restartLimit.succeed("readlink -f /run/current-system").strip()
      assert booted_restart_system == normal_restart_system, (booted_restart_system, normal_restart_system)
      assert normal_restart_system != tuning_restart_system
      grubenv = restartLimit.succeed("grub-editenv /boot/grub/grubenv list")
      assert "saved_entry=NixOS - togi" not in grubenv, f"restart limit left the tuning boot saved: {grubenv}"
      leave_reason = json.loads(next(
          line.split("=", 1)[1] for line in grubenv.splitlines() if line.startswith("togi_leave_reason=")
      ))
      assert leave_reason["count"] == 0, leave_reason
      assert leave_reason["reason"] == "restart-limit command failed; returning to the normal system", leave_reason
      restartLimit.succeed("test ! -e /run/togi-retry-tuning-boot")
      togi = [json.loads(line) for line in restartLimit.succeed("journalctl -b -1 -u togi.service -o json").splitlines()]
      assert leave_reason["id"].startswith("fallback-"), leave_reason
      invocation_id = leave_reason["id"].removeprefix("fallback-")
      assert len(invocation_id) == 32 and all(c in "0123456789abcdef" for c in invocation_id), leave_reason
      exits = [e["EXIT_STATUS"] for e in togi if "EXIT_STATUS" in e]
      results = [e["UNIT_RESULT"] for e in togi if "UNIT_RESULT" in e]
      assert exits == ["2", "2", "2"], f"togi.service exits before the reboot: {exits}"
      assert results == ["exit-code", "exit-code", "exit-code", "start-limit-hit"], f"togi.service results: {results}"
      hook_runs = [e for e in togi if e.get("MESSAGE") == "stub emergency-stop"]
      assert len(hook_runs) == 3, f"ExecStopPost ran {len(hook_runs)} times for three abnormal exits: {hook_runs}"
    '';
  };

  # Kills togi mid-trial under real systemd: the trial runs on real systemd scopes from the shipped trial runner and
  # the SMU is a persistent file, so the offsets stay applied after SIGKILL. The module's ExecStopPost hook must run
  # the package's `togi emergency-stop` (the production sweep, validation, zero-only write and readback; only the
  # machine behind the System seam is the VM's) and leave no trial scope and every core at 0 before the 60 s restart.
  # Only a successful exit skips the hook; every other exit status, a dead end included, runs it. Faults in the
  # validation, the sweep and the readback show the command's gates and exit status.
  emergency-stop = pkgs.testers.runNixOSTest {
    name = "togi-emergency-stop";

    nodes.machine = {
      imports = [ tuningBoot ];
      virtualisation.cores = 4;
      # `togi` is the test executable of crates/togi/tests/vm_recovery.rs: the same CLI on a fake SMU file.
      services.togi.package = pkgs.writeShellScriptBin "togi" ''
        exec ${vmRecovery}/bin/togi-vm-recovery --exact vm_recovery::helper --nocapture -- togi "$@"
      '';
      systemd.tmpfiles.rules = [
        "d /var/lib/togi-vm 0755 root root -"
        "f /var/lib/togi-vm/mode 0644 root root - kill"
      ];
    };

    testScript = ''
      state = "/var/lib/togi-vm"
      cores = 4
      baseline = [-5] * cores
      trial_offsets = [-30, -30, -10, -20]
      auto_restart = "systemctl show togi.service -p SubState --value | grep -qx auto-restart"
      inactive = "systemctl show togi.service -p ActiveState --value | grep -qx inactive"


      def prop(name):
          return machine.succeed(f"systemctl show togi.service -p {name} --value").strip()


      def offsets():
          raw = machine.succeed(f"cat {state}/offsets 2>/dev/null || true").split()
          return [int(value) for value in raw] if raw else baseline


      def smu_log():
          return machine.succeed(f"cat {state}/smu.log 2>/dev/null || true").splitlines()


      def zeroings():
          return [line for line in smu_log() if "emergency-zero wrote" in line]


      def scopes():
          return machine.succeed("systemctl list-units --all 'togi-trial-*.scope' --plain --no-legend").strip()


      def emergency_lines():
          journal = machine.succeed("journalctl -b -u togi.service -o cat --no-pager")
          return [line for line in journal.splitlines() if "emergency-stop:" in line]


      def plant(fault):
          machine.succeed(f"printf '%s' '-9 -9 -9 -9' > {state}/offsets; printf '%s' '{fault}' > {state}/fault")


      def run_unit(mode):
          machine.succeed(f"systemctl reset-failed togi.service; printf '%s' '{mode}' > {state}/mode; systemctl start togi.service")


      machine.start(allow_reboot=True)
      machine.wait_for_unit("multi-user.target")
      tuning_system = machine.succeed("readlink -f /run/current-system/specialisation/togi").strip()
      machine.succeed("grub-set-default 'NixOS - togi' && sync")
      machine.reboot()
      machine.wait_for_unit("multi-user.target")
      assert machine.succeed("readlink -f /run/current-system").strip() == tuning_system

      # SIGKILL mid-trial.
      machine.wait_until_succeeds(f"test -s {state}/trial", timeout=180)
      trial_scope = machine.succeed(f"cat {state}/trial").strip()
      started = [line for line in smu_log() if "trial started" in line]
      assert len(started) == 1, started
      observed = started[0].split("scopes=", 1)[1].split(",")
      assert observed == [f"{trial_scope.removesuffix('-c00')}-c00.scope", f"{trial_scope.removesuffix('-c00')}-c01.scope"], (observed, trial_scope)
      assert scopes() != "", "the trial runs on no systemd scope"
      assert offsets() == trial_offsets, offsets()
      assert prop("NRestarts") == "0"
      main_pid = prop("MainPID")
      allowed = machine.succeed(f"grep -h Cpus_allowed_list /proc/{main_pid}/task/*/status").split("\n")
      allowed = {line.split()[-1] for line in allowed if line.strip()}
      # The trial loads cores 0 and 1; core 2 is the least-deep unloaded core.
      assert allowed == {"2"}, f"togi threads may run on loaded cores: {allowed}"
      machine.succeed("systemctl kill --kill-whom=main --signal=SIGKILL togi.service")
      machine.wait_until_succeeds(auto_restart, timeout=90)
      assert prop("Result") == "signal", prop("Result")
      exit_at = int(prop("ExecMainExitTimestampMonotonic"))
      assert prop("NRestarts") == "0", "the restart already happened before the check"
      assert scopes() == "", scopes()
      assert offsets() == [0] * cores, offsets()
      log = smu_log()
      zero = next(i for i, line in enumerate(log) if "emergency-zero trial-scopes=" in line)
      assert log[zero].endswith("trial-scopes=none"), f"a trial scope was alive at the zeroing: {log[zero]}"
      assert f"pid={main_pid} " not in log[zero], "the killed process did not zero"
      assert log[zero + 1].endswith("emergency-zero wrote " + " ".join(["0"] * cores)), log[zero + 1]
      readbacks = sorted(line.split(" ", 1)[1] for line in log[zero + 2 : zero + 2 + cores])
      assert readbacks == [f"read core {core:02d} 0" for core in range(cores)], readbacks
      journal = emergency_lines()
      machine.log("\n".join(journal))
      assert any("trial scopes:" in line for line in journal), journal
      assert any(f"every core read back 0 ({cores} cores)" in line for line in journal), journal
      for core in range(cores):
          assert any(f"core {core:02d} read back 0" in line for line in journal), (core, journal)
      # The restart waits its 60 s after the cleanup, and only one restart happened.
      machine.wait_until_succeeds(f"test \"$(cat {state}/trial)\" != {trial_scope}", timeout=180)
      assert prop("NRestarts") == "1", prop("NRestarts")
      delay = int(prop("ExecMainStartTimestampMonotonic")) - exit_at
      assert delay >= 60_000_000, f"togi restarted {delay} us after it died, before RestartSec"
      assert prop("RestartUSec") == "1min", prop("RestartUSec")

      # A successful stop restores the baseline; the hook is skipped, so nothing zeroes it.
      assert offsets() == trial_offsets and scopes() != ""
      assert len(zeroings()) == 1
      emergency_total = len(emergency_lines())
      machine.succeed("systemctl stop togi.service")
      assert prop("Result") == "success", prop("Result")
      assert offsets() == baseline, offsets()
      assert scopes() == "", scopes()
      assert len(zeroings()) == 1
      assert len(emergency_lines()) == emergency_total

      # An abnormal exit status that is not one of togi's own runs the hook.
      run_unit("fail")
      machine.wait_until_succeeds(auto_restart, timeout=120)
      assert prop("Result") == "exit-code" and prop("ExecMainStatus") == "1"
      assert scopes() == "", scopes()
      assert offsets() == [0] * cores, offsets()
      assert len(zeroings()) == 2
      machine.succeed("systemctl stop togi.service")
      emergency_total = len(emergency_lines())
      assert emergency_total > 0

      # A dead-end status is abnormal too: the hook runs, and since this fake validates, it zeroes every core.
      run_unit("deadend")
      machine.wait_until_succeeds("systemctl is-failed togi.service", timeout=60)
      assert prop("Result") == "exit-code" and prop("ExecMainStatus") == "15"
      assert offsets() == [0] * cores, offsets()
      assert len(zeroings()) == 3 and len(emergency_lines()) > emergency_total
      emergency_total = len(emergency_lines())

      # The same dead end on a host whose SMU validation fails: the hook refuses and writes nothing.
      machine.succeed(f"printf '%s' validation > {state}/fault")
      run_unit("deadend")
      machine.wait_until_succeeds("systemctl is-failed togi.service", timeout=60)
      assert prop("ExecMainStatus") == "15", prop("ExecMainStatus")
      assert offsets() == baseline, offsets()
      assert len(zeroings()) == 3 and len(emergency_lines()) > emergency_total
      machine.succeed(f"rm -f {state}/fault")
      emergency_total = len(emergency_lines())

      # A success skips the hook and keeps the baseline the run restored.
      run_unit("clean")
      machine.wait_until_succeeds(inactive, timeout=60)
      assert prop("Result") == "success", prop("Result")
      assert offsets() == baseline, offsets()
      assert len(zeroings()) == 3 and len(emergency_lines()) == emergency_total

      # The command's gates: nothing is zeroed when the validation or the sweep fails, and a readback that is not
      # 0 fails the command after the zeroing.
      for fault in ["validation", "sweep"]:
          plant(fault)
          mark = len(smu_log())
          status, output = machine.execute("togi emergency-stop 2>&1")
          machine.log(output)
          assert status == 1, (fault, status, output)
          assert "by the test" in output, (fault, output)
          assert offsets() == [-9] * cores, (fault, offsets())
          assert not any("emergency-zero" in line for line in smu_log()[mark:]), fault
      plant("readback")
      status, output = machine.execute("togi emergency-stop 2>&1")
      machine.log(output)
      assert status == 1, (status, output)
      assert "core 01 reads -7" in output, output
      assert offsets() == [0] * cores, offsets()
      plant("")
      output = machine.succeed("togi emergency-stop 2>&1")
      machine.log(output)
      assert f"every core read back 0 ({cores} cores)" in output, output
      assert offsets() == [0] * cores, offsets()
      assert scopes() == "", scopes()
    '';
  };

  # Runs the shipped module's `togi-day` service in a normal boot over the vm_recovery fake SMU, a file the test never
  # lets the observer write: a clean stop ends with a final checkpoint of positive active time and then the clean
  # stop, and a hard reset of the machine leaves an end the next boot's observer records as unclean. Only a helper
  # command seeds the session journal with the current build; `togi day` itself never creates a session. The reset's
  # unclean end is then a pending question: a day.users member answers it without root over /run/togi/day.sock and
  # changes the answer, a user outside the togi group is refused, and the module's group, directory and notifier
  # unit are checked as installed.
  day-observer = pkgs.testers.runNixOSTest {
    name = "togi-day-observer";

    nodes.machine = {
      imports = [ module ];
      networking.useDHCP = false;
      virtualisation.cores = 4;
      services.togi = {
        enable = true;
        day.enable = true;
        day.users = [ "alice" ];
        package = pkgs.writeShellScriptBin "togi" ''
          exec ${vmRecovery}/bin/togi-vm-recovery --exact vm_recovery::helper --nocapture -- togi "$@"
        '';
      };
      hardware.cpu.amd.ryzen-smu.enable = false;
      users.users.alice.isNormalUser = true;
      users.users.mallory.isNormalUser = true;
      systemd.tmpfiles.rules = [
        "d /var/lib/togi-vm 0755 root root -"
      ];
    };

    testScript = ''
      import json

      journal = "/var/lib/togi/events.jsonl"


      def events():
          raw = machine.succeed(f"cat {journal}").splitlines()
          return [json.loads(line) for line in raw if line.strip()]


      def day(kind=None):
          return [e for e in events() if e["kind"].startswith("day.") and (kind is None or e["kind"] == kind)]


      machine.start(allow_reboot=True)
      machine.wait_for_unit("multi-user.target")
      # Without a session the observer refuses and creates no journal.
      machine.wait_until_succeeds("journalctl -u togi-day.service -o cat --no-pager | grep -F 'no session journal'")
      machine.succeed(f"test ! -e {journal}")
      machine.succeed("systemctl stop togi-day.service; systemctl reset-failed togi-day.service")
      assert machine.succeed("journalctl -u togi-day.service -o cat --no-pager").count("no session journal") >= 1
      assert "Storage=persistent" in machine.succeed("systemctl cat systemd-journald.service; cat /etc/systemd/journald.conf")
      assert "SyncIntervalSec=5s" in machine.succeed("cat /etc/systemd/journald.conf")
      machine.succeed("togi seed-session")

      # A clean stop writes the final checkpoint, then the stop.
      machine.succeed("systemctl start togi-day.service")
      machine.wait_until_succeeds(f"grep -q day.boot_start {journal}")
      machine.sleep(3)
      machine.succeed("systemctl stop togi-day.service")
      assert machine.succeed("systemctl show togi-day.service -p Result --value").strip() == "success"
      kinds = [e["kind"] for e in day()]
      assert kinds == ["day.boot_start", "day.checkpoint", "day.clean_stop"], kinds
      checkpoint = day("day.checkpoint")[0]
      assert checkpoint["data"]["applied_secs"] > 0, checkpoint
      # The observer only reads the fake SMU: it never wrote it.
      machine.succeed("! grep -E '^pid=[0-9]+ (set|set-all|emergency-zero|restore-baseline)' /var/lib/togi-vm/smu.log")
      machine.succeed("grep -q '^pid=[0-9]* read core' /var/lib/togi-vm/smu.log")

      # A reset leaves no clean stop; the next boot's observer records the end as unclean.
      machine.succeed("systemctl start togi-day.service")
      machine.wait_until_succeeds(f"test \"$(grep -c day.boot_start {journal})\" = 2")
      first_boot = machine.succeed("cat /proc/sys/kernel/random/boot_id").strip().replace("-", "")
      machine.sleep(2)
      machine.crash()
      machine.start(allow_reboot=True)
      machine.wait_for_unit("multi-user.target")
      machine.wait_for_unit("togi-day.service")
      machine.wait_until_succeeds(f"grep -q day.unclean_end {journal}")
      unclean = day("day.unclean_end")
      assert len(unclean) == 1, unclean
      assert unclean[0]["data"]["boot"].replace("-", "") == first_boot, (unclean, first_boot)
      reason = unclean[0]["data"]["reset_reason"]
      machine.log(json.dumps(reason))
      # The VM may not prove a successor's reset reason; then it is recorded as unknown, never as a CPU reset.
      assert reason.get("kind") in (None, "unknown", "Unknown") or reason["supported"], reason
      machine.wait_until_succeeds(f"test \"$(grep -c day.boot_start {journal})\" = 3")

      # The module wires the answer path: alice, named in day.users, is in the togi group; mallory is not.
      # /run/togi is root:togi 0750 and the service's socket root:togi 0660 inside it.
      groups = lambda user: machine.succeed(f"id -nG {user}").split()
      assert "togi" in groups("alice"), groups("alice")
      assert "togi" not in groups("mallory"), groups("mallory")
      assert machine.succeed("stat -c '%U:%G:%a' /run/togi").strip() == "root:togi:750"
      machine.wait_until_succeeds("test -S /run/togi/day.sock")
      assert machine.succeed("stat -c '%U:%G:%a' /run/togi/day.sock").strip() == "root:togi:660"

      # The notifier is a graphical-session user unit for the named user only, with notify-send on its PATH.
      notifier = machine.succeed("cat /etc/systemd/user/togi-day-notify.service")
      machine.log(notifier)
      assert "ExecStart=" in notifier and " day notify" in notifier, notifier
      assert "PartOf=graphical-session.target" in notifier and "After=graphical-session-pre.target" in notifier, notifier
      assert "After=graphical-session.target" not in notifier, notifier
      assert "BindsTo=graphical-session.target" in notifier, notifier
      assert "Restart=on-failure" in notifier and "StartLimitIntervalSec=0" in notifier, notifier
      assert "libnotify" in notifier, notifier
      # Supply a real user runtime in this headless VM before checking the graphical-session ordering.
      alice = machine.succeed("id -u alice").strip()
      machine.succeed("loginctl enable-linger alice")
      machine.wait_for_unit(f"user@{alice}.service")
      machine.succeed(
          f"runuser -u alice -- env XDG_RUNTIME_DIR=/run/user/{alice} "
          "systemd-analyze --user verify /etc/systemd/user/togi-day-notify.service"
      )
      machine.succeed("test -L /etc/systemd/user/graphical-session.target.wants/togi-day-notify.service")
      condition = next(
          line.removeprefix("ExecCondition=") for line in notifier.splitlines() if line.startswith("ExecCondition=")
      )
      machine.succeed(f"runuser -u alice -- {condition}")
      machine.fail(f"runuser -u mallory -- {condition}")

      # The forced reset left the end pending: nothing answers it yet.
      assert day("day.answer") == [], day("day.answer")
      crashed = unclean[0]["data"]["boot"]
      mallory = machine.succeed("id -u mallory").strip()

      # A user outside the group is refused at the socket and records nothing.
      machine.fail(f"runuser -u mallory -- togi day answer {crashed} frozen")
      assert day("day.answer") == [], day("day.answer")

      # The notifier decodes the pending end, with its per-core offsets, from the socket and raises one notification.
      machine.succeed("install -d -o alice /tmp/fake-notify")
      machine.succeed("""cat > /tmp/fake-notify/notify-send <<'EOF'
      #!/bin/sh
      echo "$@" > /tmp/fake-notify/args
      EOF""")
      machine.succeed("chmod 755 /tmp/fake-notify/notify-send")
      # The notifier never exits on its own; the timeout ends it, and only a decoded question leaves the file.
      machine.succeed("runuser -u alice -- sh -c 'PATH=/tmp/fake-notify:$PATH timeout 5 togi day notify; true'")
      machine.succeed(f"grep -q {crashed} /tmp/fake-notify/args")

      # The group user answers through the real socket without root; the service journals the answer.
      machine.succeed(f"runuser -u alice -- togi day answer {crashed} frozen")
      machine.wait_until_succeeds(f"grep -q day.answer {journal}")
      answers = day("day.answer")
      assert len(answers) == 1, answers
      assert answers[0]["data"]["boot"] == crashed, answers
      assert answers[0]["data"]["answer"] == "frozen", answers
      assert answers[0]["data"]["uid"] == int(alice), (answers, alice, mallory)
      assert len(day("day.unclean_end")) == 1

      # A later answer supersedes the first; both stay in the journal.
      machine.succeed(f"runuser -u alice -- togi day answer {crashed} unsure")
      machine.wait_until_succeeds(f"test \"$(grep -c day.answer {journal})\" = 2")
      assert [a["data"]["answer"] for a in day("day.answer")] == ["frozen", "unsure"], day("day.answer")
      machine.succeed("systemctl is-active togi-day.service")
      machine.succeed("systemctl stop togi-day.service")
      kinds = [e["kind"] for e in day()]
      assert kinds[-1] == "day.clean_stop", kinds
    '';
  };
}
