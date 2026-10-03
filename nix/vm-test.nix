{
  pkgs,
  package,
  trialTests,
}:
let
  module = import ./module.nix { packages.${pkgs.stdenv.hostPlatform.system}.default = package; };
  documentedLockProvision = pkgs.writeText "togi-documented-lock-provision.py" (
    builtins.elemAt (pkgs.lib.splitString "\nPY\n" (builtins.elemAt (pkgs.lib.splitString "<<'PY'\n" (builtins.readFile ../docs/howto.md)) 1)) 0
  );
  tuningBoot =
    { pkgs, ... }:
    {
      imports = [ module ];
      virtualisation.useBootLoader = true;
      boot.loader.grub.enable = true;
      boot.loader.timeout = 1;
      virtualisation.qemu.options = [
        "-device i6300esb"
        "-watchdog-action reset"
      ];
      specialisation.togi.configuration.boot.initrd.kernelModules = [ "i6300esb" ];
      services.togi = {
        enable = true;
        tuning.enable = true;
      };
      hardware.cpu.amd.ryzen-smu.enable = false;
      environment.systemPackages = [ pkgs.grub2 ];
    };
in
{
  tuning-boot = pkgs.testers.runNixOSTest {
    name = "togi-tuning-boot";

    nodes.machine = {
      imports = [ tuningBoot ];
      console.font = "Lat2-Terminus16";
      virtualisation.cores = 4;
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

      machine.start(allow_reboot=True)
      machine.wait_for_unit("multi-user.target")
      machine.succeed("togi-trial-tests -test.run '^TestHardwareScope' -test.v -test.timeout 180s")
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
      machine.fail(
          "runuser -u unrelated -- togi-trial-tests -test.run '^TestHardwareScope$' "
          "-test.timeout 60s > /run/unrelated-lock-output 2>&1"
      )
      machine.succeed("grep -q 'permission denied' /run/unrelated-lock-output")
      machine.succeed("chown unrelated /run/lock/togi.lock")
      foreign_identity = machine.succeed("stat -c '%d:%i:%u:%g:%a' /run/lock/togi.lock").strip()
      machine.succeed("systemd-tmpfiles --create --prefix=/run/lock/togi.lock")
      foreign_status, foreign_output = machine.execute(
          "togi --state-dir /tmp/foreign-lock-state reset --all 2>&1"
      )
      assert foreign_status == 1, foreign_output
      assert "lock is owned by another user" in foreign_output, foreign_output
      assert machine.succeed("stat -c '%d:%i:%u:%g:%a' /run/lock/togi.lock").strip() == foreign_identity
      machine.succeed("test ! -e /tmp/foreign-lock-state; chown root /run/lock/togi.lock")
      machine.succeed("chmod 0640 /run/lock/togi.lock")
      readonly_status, readonly_output = machine.execute(
          "runuser -u togi-hardware -- togi --state-dir /tmp/readonly-lock-state reset --all 2>&1"
      )
      assert readonly_status == 1, readonly_output
      assert "lock owner must restrict legacy permissions" in readonly_output, readonly_output
      assert machine.succeed("stat -c '%a' /run/lock/togi.lock").strip() == "640"
      machine.succeed("test ! -e /tmp/readonly-lock-state; chmod 0660 /run/lock/togi.lock")
      machine.succeed("loginctl enable-linger togi-hardware")
      delegated_uid = machine.succeed("id -u togi-hardware").strip()
      machine.wait_for_unit(f"user@{delegated_uid}.service")
      machine.succeed(
          f"runuser -u togi-hardware -- env XDG_RUNTIME_DIR=/run/user/{delegated_uid} "
          f"DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/{delegated_uid}/bus "
          "togi-trial-tests -test.run '^TestHardwareScope$' -test.v -test.timeout 60s"
      )
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
          e["kind"] == "deadend" and e["condition"] == "preflight" and e["action"] == "clear_saved_entry"
          for e in events
      ), events
      assert any(
          e["kind"] == "boot.saved_entry" and e["before"] == "NixOS - togi" and e["after"] == ""
          for e in events
      ), events
      preflight = [
          json.loads(line)
          for line in machine.succeed("togi events --json --kind preflight.check").splitlines()
      ]
      watchdog = [e for e in preflight if e["check"] == "watchdog"]
      assert len(watchdog) == 1 and watchdog[0]["ok"], preflight
      assert preflight[0]["check"] == "watchdog", preflight
      machine.log(f"armed watchdog preflight: {watchdog[0]}")
      machine.succeed("test -z \"$(togi events --json --kind smu.intent,trial.intent)\"")
      machine.wait_for_unit("togi-watch.service")
      machine.fail("grep -q '^FONT=' /etc/vconsole.conf")
      machine.wait_until_succeeds("grep -aq 'dead end preflight' /dev/vcs1")
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
      machine.succeed("echo 1 > /sys/fs/cgroup/init.scope/cgroup.freeze")
      machine.wait_until_succeeds(
          "grep -qx 'frozen 1' /sys/fs/cgroup/init.scope/cgroup.events", timeout=10
      )
      watchdog_event = None
      deadline = time.monotonic() + 90
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
      togi = [json.loads(line) for line in restartLimit.succeed("journalctl -b -1 -u togi.service -o json").splitlines()]
      exits = [e["EXIT_STATUS"] for e in togi if "EXIT_STATUS" in e]
      results = [e["UNIT_RESULT"] for e in togi if "UNIT_RESULT" in e]
      assert exits == ["2", "2", "2"], f"togi.service exits before the reboot: {exits}"
      assert results == ["exit-code", "exit-code", "exit-code", "start-limit-hit"], f"togi.service results: {results}"
    '';
  };
}
