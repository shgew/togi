{
  pkgs,
  package,
  trialTests,
  # The executable of crates/togi/tests/vm_recovery.rs at bin/togi-vm-recovery: `togi` with a persistent fake SMU.
  vmRecovery,
}:
let
  module = import ./module.nix { packages.${pkgs.stdenv.hostPlatform.system}.default = package; };
  documentedLockProvision = pkgs.writeText "togi-documented-lock-provision.py" (
    builtins.elemAt (pkgs.lib.splitString "\nPY\n" (builtins.elemAt (pkgs.lib.splitString "<<'PY'\n" (builtins.readFile ../docs/howto.md)) 1)) 0
  );
  # Both VM tests share this node: the NixOS module with the tuning specialisation, GRUB on a virtual disk
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
      users.groups.togi-hardware = { };
      users.users.togi-hardware = {
        isNormalUser = true;
        group = "togi-hardware";
      };
      users.users.unrelated.isNormalUser = true;
      systemd.services."user@".serviceConfig.Delegate = "cpu cpuset io memory pids";
      environment.etc."togi-test-lock-provision.py".source = documentedLockProvision;
      environment.systemPackages = [
        trialTests
        pkgs.util-linux
        pkgs.python3
      ];
    };

    testScript = ''
      import json
      import time

      hardware = "hardware:: --skip hardware::helper --skip every_r1_and_r2_workload_passes"
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
      provision = "python3 /etc/togi-test-lock-provision.py"
      provision_dir = machine.succeed("mktemp -d /run/togi-provision.XXXXXX").strip()
      try:
          for initial_mode in [None, "600", "644"]:
              for group, expected_owner in [("-", "root:root:600"), ("togi-hardware", "root:togi-hardware:660")]:
                  fixture = f"{provision_dir}/lock-{initial_mode}-{group}"
                  if initial_mode is not None:
                      machine.succeed(f"printf provision-content > {fixture}; chmod {initial_mode} {fixture}")
                      identity = machine.succeed(f"stat -c '%d:%i' {fixture}").strip()
                  machine.succeed(f"{provision} {fixture} {group}")
                  assert machine.succeed(f"stat -c '%U:%G:%a' {fixture}").strip() == expected_owner
                  if initial_mode is not None:
                      assert machine.succeed(f"stat -c '%d:%i' {fixture}").strip() == identity
                      assert machine.succeed(f"cat {fixture}") == "provision-content"
                  machine.log(f"documented provisioning: initial={initial_mode}, group={group}, result={expected_owner}")

          held = f"{provision_dir}/held-legacy"
          machine.succeed(f"printf held-content > {held}; chmod 0644 {held}")
          held_before = machine.succeed(f"stat -c '%d:%i:%U:%G:%a:%h' {held}; sha256sum {held}")
          # Keep an old read-only descriptor locked while running the published procedure.
          machine.succeed(
              f"(exec 9<{held}; flock --exclusive 9; "
              f"if {provision} {held} togi-hardware > {provision_dir}/held-output 2>&1; then exit 1; fi; "
              f"! flock --exclusive --nonblock {held} true)"
          )
          assert machine.succeed(f"stat -c '%d:%i:%U:%G:%a:%h' {held}; sha256sum {held}") == held_before
          machine.succeed(f"flock --exclusive --nonblock {held} true")
          machine.log("documented provisioning: held legacy lock refused without changing inode, content, or exclusion")
          machine.log(machine.succeed(f"cat {provision_dir}/held-output"))

          target = f"{provision_dir}/symlink-target"
          symlink = f"{provision_dir}/symlink-lock"
          machine.succeed(f"printf target-content > {target}; chmod 0644 {target}; ln -s {target} {symlink}")
          target_before = machine.succeed(f"stat -c '%d:%i:%U:%G:%a:%h' {target}; sha256sum {target}")
          machine.log(machine.fail(f"{provision} {symlink} togi-hardware"))
          assert machine.succeed(f"stat -c '%d:%i:%U:%G:%a:%h' {target}; sha256sum {target}") == target_before
          assert machine.succeed(f"readlink {symlink}").strip() == target
          machine.log("documented provisioning: symlink refused without modifying its target")
      finally:
          machine.succeed(f"rm -rf -- {provision_dir}")
      assert machine.succeed("stat -c '%d:%i' /run/lock/togi.lock").strip() == lock_identity
      assert machine.succeed("stat -c '%U:%G:%a' /run/lock/togi.lock").strip() == "root:togi-hardware:660"
      machine.fail("runuser -u unrelated -- touch /run/lock/unrelated-lock")
      machine.fail("runuser -u unrelated -- flock --nonblock /run/lock/togi.lock true")
      unrelated_status, unrelated_output = machine.execute(
          f"runuser -u unrelated -- togi-trial-tests {pin} {flags} 2>&1"
      )
      assert unrelated_status != 0, unrelated_output
      assert "permission denied" in unrelated_output.lower(), unrelated_output
      machine.succeed("chown unrelated /run/lock/togi.lock")
      foreign_identity = machine.succeed("stat -c '%d:%i:%u:%g:%a' /run/lock/togi.lock").strip()
      machine.succeed("systemd-tmpfiles --create --prefix=/run/lock/togi.lock")
      foreign_status, foreign_output = machine.execute(
          "togi --state-dir /tmp/foreign-lock-state reset --all 2>&1"
      )
      assert foreign_status == 1, foreign_output
      assert "is owned by another user" in foreign_output, foreign_output
      assert machine.succeed("stat -c '%d:%i:%u:%g:%a' /run/lock/togi.lock").strip() == foreign_identity
      machine.succeed("test ! -e /tmp/foreign-lock-state; chown root /run/lock/togi.lock")
      machine.succeed("chmod 0640 /run/lock/togi.lock")
      readonly_status, readonly_output = machine.execute(
          "runuser -u togi-hardware -- togi --state-dir /tmp/readonly-lock-state reset --all 2>&1"
      )
      assert readonly_status == 1, readonly_output
      assert "only its owner may restrict them" in readonly_output, readonly_output
      assert machine.succeed("stat -c '%a' /run/lock/togi.lock").strip() == "640"
      machine.succeed("test ! -e /tmp/readonly-lock-state; chmod 0660 /run/lock/togi.lock")
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
      machine.succeed(
          "test ! -e /sys/class/watchdog/watchdog0/state || "
          "grep -qx inactive /sys/class/watchdog/watchdog0/state"
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
      assert len(watchdog) == 1 and watchdog[0]["data"]["ok"], preflight
      assert preflight[0]["data"]["check"] == "watchdog", preflight
      machine.log(f"armed watchdog preflight: {watchdog[0]}")
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
      machine.succeed("systemd-tmpfiles --create --prefix=/run/lock/togi.lock")
      assert machine.succeed("stat -c '%d:%i' /run/lock/togi.lock").strip() == contention_inode
      assert machine.succeed("stat -c '%a' /run/lock/togi.lock").strip() == "644"
      for state_dir in ["/tmp/host-lock-state-a", "/tmp/host-lock-state-b"]:
          contention_status, contention_output = machine.execute(
              f"togi --state-dir {state_dir} reset --all 2>&1"
          )
          assert contention_status == 3, contention_output
          assert "another togi process holds the host lock" in contention_output, contention_output
          machine.succeed(f"test ! -e {state_dir}")
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
      released_status, released_output = machine.execute(
          "togi --state-dir /tmp/released-lock-state reset --all 2>&1"
      )
      assert released_status == 1, released_output
      assert "no journal at /tmp/released-lock-state/events.jsonl" in released_output
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

      hardware = "hardware:: --skip hardware::helper --skip every_r1_and_r2_workload_passes"
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
          ("togi-trial-tests", hardware, 11),
          ("togi-trial-tests", delegated_hardware, 9),
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
      assert len(started) == 1 and "scopes=togi-trial-" in started[0], started
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
}
