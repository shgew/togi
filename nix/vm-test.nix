{
  pkgs,
  package,
  trialTests,
}:
let
  module = import ./module.nix { packages.${pkgs.stdenv.hostPlatform.system}.default = package; };
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
      environment.systemPackages = [
        trialTests
        pkgs.util-linux
      ];
    };

    testScript = ''
      import json
      import time

      machine.start(allow_reboot=True)
      machine.wait_for_unit("multi-user.target")
      machine.succeed("togi-trial-tests -test.run '^TestHardwareScope' -test.v -test.timeout 60s")
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
      machine.succeed("ls -l /proc/1/fd | grep /dev/watchdog > /dev/null")
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
        services.togi.tuning.leaveOnShutdown = false;
        specialisation.togi.configuration = {
          services.togi.settings.bogus = 1;
          systemd.services.togi.serviceConfig.RestartSec = lib.mkForce 1;
        };
      };

    testScript = ''
      import json

      restartLimit.start(allow_reboot=True)
      restartLimit.wait_for_unit("multi-user.target")
      normal_restart_system = restartLimit.succeed("readlink -f /run/current-system").strip()
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
