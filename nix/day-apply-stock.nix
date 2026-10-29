{
  pkgs,
  package,
  vmRecovery,
}:
let
  module = import ./module.nix { packages.${pkgs.stdenv.hostPlatform.system}.default = package; };
  python = pkgs.python3.withPackages (ps: [ ps.mfusepy ]);
  fixture = "/run/togi-fake-root";
in
pkgs.testers.runNixOSTest {
  name = "togi-day-apply-stock";
  passthru.tier = "ci";
  nodes.machine = {
    imports = [ module ];
    networking.useDHCP = false;
    virtualisation.cores = 8;
    virtualisation.qemu.options = [
      "-device i6300esb"
      "-watchdog-action reset"
    ];
    boot.initrd.kernelModules = [ "i6300esb" ];
    boot.kernelModules = [ "fuse" ];
    services.togi = {
      enable = true;
      day.enable = true;
      day.watchdog = true;
    };
    hardware.cpu.amd.ryzen-smu.enable = false;
    systemd.tmpfiles.rules = [
      "d /var/lib/togi-vm 0755 root root -"
      "d ${fixture} 0755 root root -"
    ];
    # The fake driver lives outside the production executable. Its synchronous
    # FUSE callbacks complete each response before the driver's immediate read.
    systemd.services.togi-fake-smu = {
      description = "VM-only synchronous SMU filesystem";
      wantedBy = [ "multi-user.target" ];
      before = [ "togi-day.service" ];
      environment.FUSE_LIBRARY_PATH = "${pkgs.lib.getLib pkgs.fuse3}/lib/libfuse3.so";
      serviceConfig = {
        ExecStart = "${python}/bin/python ${./fake-smu.py} ${fixture} /var/lib/togi-vm";
        ExecStartPost = pkgs.writeShellScript "await-fake-smu" ''
          until ${pkgs.util-linux}/bin/mountpoint -q ${fixture}; do
            ${pkgs.coreutils}/bin/sleep 0.01
          done
        '';
        TimeoutStartSec = 30;
      };
    };
    systemd.services.togi-day = {
      requires = [ "togi-fake-smu.service" ];
      after = [ "togi-fake-smu.service" ];
      serviceConfig = {
        # Bind the existing kernel directory, not a nonexistent sysfs leaf:
        # sysfs cannot create a mount destination for ryzen_smu_drv in this VM.
        BindPaths = [ "${fixture}/sys/kernel:/sys/kernel" ];
        BindReadOnlyPaths = [
          "${fixture}/proc/cpuinfo:/proc/cpuinfo"
          "${fixture}/sys/devices/system/cpu:/sys/devices/system/cpu"
          "${fixture}/sys/class/dmi:/sys/class/dmi"
        ];
      };
    };
    # A real generation switch changes this unit and restarts the same shipped
    # executable without changing the boot or the fixture's hardware state.
    specialisation.day-switch.configuration.systemd.services.togi-day.environment.TOGI_VM_GENERATION =
      "switched";
  };

  testScript = ''
    import json

    journal = "/var/lib/togi/events.jsonl"
    stock = {str(core): 0 for core in range(8)}


    def events(kind=None):
        rows = [json.loads(line) for line in machine.succeed(f"cat {journal}").splitlines() if line.strip()]
        return rows if kind is None else [row for row in rows if row["kind"] == kind]


    def protocol():
        return [json.loads(line) for line in machine.succeed("cat /var/lib/togi-vm/protocol.jsonl").splitlines()]


    def assert_stock():
        applied = events("day.applied")
        assert applied, events()
        by_seq = {row["seq"]: row for row in events()}
        for row in applied:
            data = row["data"]
            assert data["target"] == {"ccd": {"0": 0}, "cores": stock}, data
            assert data["readbacks"] == stock, data
            transaction = [by_seq[seq] for seq in data["transactions"]]
            assert [e["kind"] for e in transaction] == ["smu.intent", "smu.write"] + ["smu.readback"] * 8, transaction
            assert all(e["data"]["offset"] == 0 for e in transaction), transaction
            assert transaction[-1]["seq"] < row["seq"], (transaction, row)
        commands = protocol()
        assert not any(row["operation"] in ("refused-write", "unexpected-command") for row in commands), commands
        writes = [row for row in commands if row["operation"] == "write"]
        assert writes and all(row["targets"] == [0] * 8 for row in writes), writes
        assert all(row["value"] == 0 for row in commands if row["operation"] == "read"), commands


    machine.start()
    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("togi-fake-smu.service")
    machine.wait_until_succeeds("grep -qx active /sys/class/watchdog/watchdog0/state")
    machine.wait_until_succeeds("journalctl -u togi-day.service -o cat --no-pager | grep -F 'no session journal'")
    machine.succeed("systemctl stop togi-day.service; systemctl reset-failed togi-day.service")
    machine.succeed(f"test ! -e {journal}")
    # The helper only seeds current-build session metadata and a stock baseline.
    # Every day command under test is the actual installed production binary.
    machine.succeed("${vmRecovery}/bin/togi-vm-recovery --exact vm_recovery::helper --nocapture -- togi seed-shipped-session")
    machine.succeed("systemctl start togi-day.service")
    machine.wait_until_succeeds(f"test \"$(grep -c day.applied {journal})\" = 1")
    pid = machine.succeed("systemctl show togi-day.service -p MainPID --value").strip()
    assert machine.succeed(f"readlink -f /proc/{pid}/exe").strip() == "${package}/bin/togi"
    assert "specialisation" not in machine.succeed("readlink -f /run/current-system").strip()
    watchdog = events("day.boot_start")[0]["data"]["watchdog"]
    assert watchdog["state"] == "armed", watchdog
    assert "held by PID 1" in watchdog["detail"], watchdog
    assert_stock()
    boot = machine.succeed("cat /proc/sys/kernel/random/boot_id").strip()
    steps = events("day.step")
    caps = events("day.cap")
    machine.sleep(3)

    switched = machine.succeed("readlink -f /run/current-system/specialisation/day-switch").strip()
    machine.succeed(f"{switched}/bin/switch-to-configuration test")
    machine.wait_until_succeeds(f"test \"$(grep -c day.applied {journal})\" = 2")
    assert machine.succeed("cat /proc/sys/kernel/random/boot_id").strip() == boot
    assert machine.succeed("systemctl show togi-day.service -p MainPID --value").strip() != pid
    assert machine.succeed("readlink -f /run/current-system").strip() == switched
    assert events("day.step") == steps, (steps, events("day.step"))
    assert events("day.cap") == caps, (caps, events("day.cap"))
    assert events("day.applied")[-1]["data"]["cause"]["kind"] == "reapplication"
    assert_stock()

    # Stop issues another zero transaction and reads every core back before the
    # final active-time checkpoint and clean marker. A fallback cannot earn one.
    machine.sleep(2)
    before = len(events())
    writes = len([row for row in protocol() if row["operation"] == "write"])
    commands_before = len(protocol())
    machine.succeed("systemctl stop togi-day.service")
    assert machine.succeed("systemctl show togi-day.service -p Result --value").strip() == "success"
    stopped = events()[before:]
    assert [row["kind"] for row in stopped] == ["smu.intent", "smu.write"] + ["smu.readback"] * 8 + ["day.checkpoint", "day.clean_stop"], stopped
    assert {str(row["data"]["core"]): row["data"]["offset"] for row in stopped if row["kind"] == "smu.readback"} == stock, stopped
    assert stopped[-2]["data"]["applied_secs"] > 0, stopped
    stop_commands = protocol()[commands_before:]
    assert [row["operation"] for row in stop_commands] == ["write"] + ["read"] * 8, stop_commands
    assert stop_commands[0]["journal_kind"] == "smu.intent", stop_commands
    assert [row["core"] for row in stop_commands[1:]] == list(range(8)), stop_commands
    assert all(row["journal_seq"] < stopped[-2]["seq"] for row in stop_commands), (stop_commands, stopped)
    assert len([row for row in protocol() if row["operation"] == "write"]) == writes + 1
    assert_stock()
  '';
}
