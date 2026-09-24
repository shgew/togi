{ pkgs, self }:
pkgs.testers.runNixOSTest {
  name = "shycler-tuning-boot";

  nodes.machine =
    { pkgs, ... }:
    {
      imports = [ self.nixosModules.default ];
      services.shycler = {
        enable = true;
        tuning.enable = true;
      };
      hardware.cpu.amd.ryzen-smu.enable = false;
      environment.systemPackages = [ pkgs.grub2 ];
    };

  testScript = ''
    machine.start()
    machine.wait_for_unit("multi-user.target")
    machine.succeed("mkdir -p /boot/grub && grub-editenv /boot/grub/grubenv create && grub-editenv /boot/grub/grubenv set saved_entry=shycler-test")
    machine.succeed("/run/current-system/specialisation/shycler/bin/switch-to-configuration test")
    machine.wait_until_succeeds("systemctl is-failed shycler.service")
    status = machine.succeed("systemctl show shycler.service -p ExecMainStatus --value").strip()
    assert status == "15", f"shycler.service exited {status}, want 15 (dead end preflight)"
    restarts = machine.succeed("systemctl show shycler.service -p NRestarts --value").strip()
    assert restarts == "0", f"shycler.service restarted {restarts} times after a dead end"
    grubenv = machine.succeed("grub-editenv /boot/grub/grubenv list")
    assert "saved_entry" not in grubenv, f"saved_entry left in grubenv: {grubenv}"
    saved = machine.succeed("shycler events --kind boot.saved_entry")
    assert "shycler-test cleared" in saved, saved
    deadend = machine.succeed("shycler events --kind deadend")
    assert "preflight" in deadend and "clearing GRUB's saved entry" in deadend, deadend
    machine.wait_for_unit("shycler-console.service")
  '';
}
