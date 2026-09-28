{ pkgs, package }:
let
  module = import ./module.nix { packages.${pkgs.stdenv.hostPlatform.system}.default = package; };
in
pkgs.testers.runNixOSTest {
  name = "togi-tuning-boot";

  nodes.machine =
    { pkgs, ... }:
    {
      imports = [ module ];
      virtualisation.useBootLoader = true;
      boot.loader.grub.enable = true;
      boot.loader.timeout = 1;
      services.togi = {
        enable = true;
        tuning.enable = true;
      };
      hardware.cpu.amd.ryzen-smu.enable = false;
      console.font = "Lat2-Terminus16";
      environment.systemPackages = [ pkgs.grub2 ];
    };

  testScript = ''
    import json

    machine.start(allow_reboot=True)
    machine.wait_for_unit("multi-user.target")
    normal_system = machine.succeed("readlink -f /run/current-system").strip()
    tuning_system = machine.succeed("readlink -f /run/current-system/specialisation/togi").strip()
    assert tuning_system != normal_system, (normal_system, tuning_system)
    machine.succeed("! systemctl is-active --quiet togi.service")
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
    machine.crash()
    machine.start(allow_reboot=True)
    machine.wait_for_unit("multi-user.target")
    booted_system = machine.succeed("readlink -f /run/current-system").strip()
    assert booted_system == tuning_system, f"crash left the tuning boot: {booted_system}"
    machine.wait_until_succeeds("systemctl is-failed togi.service")
    machine.succeed("grub-set-default 'NixOS - togi' && sync")
    machine.reboot()
    machine.wait_for_unit("multi-user.target")
    assert machine.succeed("readlink -f /run/current-system").strip() == normal_system
    machine.succeed("! systemctl is-active --quiet togi.service")
  '';
}
