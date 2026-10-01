{ pkgs, package }:
let
  inherit (pkgs) lib;
  module = import ./module.nix { packages.${pkgs.stdenv.hostPlatform.system}.default = package; };
  evaluate =
    mirrors: selectedPackage: hardwareTestGroup:
    (import (pkgs.path + "/nixos/lib/eval-config.nix") {
      inherit pkgs;
      system = pkgs.stdenv.hostPlatform.system;
      modules = [
        module
        {
          system.stateVersion = "26.05";
          boot.loader.grub = {
            enable = true;
            mirroredBoots = mirrors;
          };
          fileSystems = {
            "/" = {
              device = "/dev/vda1";
              fsType = "ext4";
            };
            "/boot" = {
              device = "/dev/vda2";
              fsType = "ext4";
              options = [ "noauto" ];
            };
          };
          services.togi = {
            enable = true;
            package = lib.mkIf (selectedPackage != null) selectedPackage;
            inherit hardwareTestGroup;
            tuning.enable = true;
          };
          hardware.cpu.amd.ryzen-smu.enable = false;
          users.groups = lib.optionalAttrs (hardwareTestGroup != null) {
            ${hardwareTestGroup} = { };
          };
        }
      ];
    }).config;
  mirror = path: {
    inherit path;
    devices = [ "nodev" ];
  };
  zero = evaluate [ ] null null;
  one = evaluate [ (mirror "/boot") ] null null;
  two = evaluate [
    (mirror "/boot")
    (mirror "/boot2")
  ] null null;
  overridden = evaluate [ (mirror "/boot") ] pkgs.hello null;
  delegated = evaluate [ (mirror "/boot") ] null "togi-hardware";
  overriddenTuning = overridden.specialisation.togi.configuration;
  rejectsMirrors =
    config:
    lib.any (
      a: !a.assertion && lib.hasPrefix "services.togi.tuning.enable" a.message
    ) config.assertions;
  tuning = one.specialisation.togi.configuration;
in
assert rejectsMirrors zero;
assert lib.all (a: a.assertion) one.assertions;
assert !rejectsMirrors one;
assert rejectsMirrors two;
assert builtins.elem "noauto" tuning.fileSystems."/boot".options;
assert tuning.systemd.services.togi.unitConfig.RequiresMountsFor == "/boot/grub/grubenv";
assert lib.hasInfix "RequiresMountsFor=/boot/grub/grubenv" tuning.systemd.units."togi.service".text;
assert builtins.elem "f /run/lock/togi.lock :0600 :root :root - -" one.systemd.tmpfiles.rules;
assert builtins.elem "f /run/lock/togi.lock :0660 :root :togi-hardware - -"
  delegated.systemd.tmpfiles.rules;
assert builtins.elem "3" (
  lib.splitString " " tuning.systemd.services.togi.serviceConfig.RestartPreventExitStatus
);
assert lib.all
  (
    code:
    builtins.elem code (
      lib.splitString " " tuning.systemd.services.togi.serviceConfig.RestartPreventExitStatus
    )
  )
  [
    "10"
    "11"
    "12"
    "13"
    "14"
    "15"
    "16"
    "17"
    "18"
  ];
assert one.services.togi.package == package;
assert builtins.elem pkgs.hello overridden.environment.systemPackages;
assert builtins.elem pkgs.hello overriddenTuning.environment.systemPackages;
assert
  overriddenTuning.systemd.services.togi.serviceConfig.ExecStart
  == "${lib.getExe pkgs.hello} run --tuning-boot /boot/grub/grubenv";
assert
  overriddenTuning.systemd.services.togi-watch.serviceConfig.ExecStart
  == "${lib.getExe pkgs.hello} watch";
pkgs.runCommand "togi-module-check" { } ''
  touch "$out"
''
