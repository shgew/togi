{ pkgs, package }:
let
  inherit (pkgs) lib;
  module = import ./module.nix { packages.${pkgs.stdenv.hostPlatform.system}.default = package; };
  evaluate =
    mirrors:
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
            tuning.enable = true;
          };
          hardware.cpu.amd.ryzen-smu.enable = false;
        }
      ];
    }).config;
  mirror = path: {
    inherit path;
    devices = [ "nodev" ];
  };
  zero = evaluate [ ];
  one = evaluate [ (mirror "/boot") ];
  two = evaluate [
    (mirror "/boot")
    (mirror "/boot2")
  ];
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
pkgs.runCommand "togi-module-check" { } ''
  touch "$out"
''
