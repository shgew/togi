# togi finds offsets; BIOS applies them

Offsets written through `ryzen_smu` are volatile and vanish on reboot. togi reports the edges and the owner enters them in BIOS. There is no boot-time service that re-applies a profile: a bad saved profile applied on every boot could make the daily system unbootable, and BIOS stays the one place that decides daily offsets.
