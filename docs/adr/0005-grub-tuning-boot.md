# Unattended tuning is a GRUB boot entry

Unattended tuning happens in a NixOS specialisation that GRUB remembers as the last booted entry, so crash reboots land back in tuning with no action. This matches how the owner already runs linux-corecycler. Only GRUB is supported at first; other bootloaders are [#27](https://code.marleb.org/shgew/shycler/issues/27).

## Considered Options

- **An "armed" flag checked on every normal boot, independent of the bootloader:** rejected because it means remembering to arm and disarm instead of simply booting into tuning.
- **A separate bootable image:** more to build and keep in sync with the host configuration.
