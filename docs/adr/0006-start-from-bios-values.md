Status: superseded by [0082](0082-bios-stays-at-zero.md) for its BIOS-start choice; Ruleset 13 keeps BIOS Curve Optimizer at zero, with no nonzero starting baseline.

# Search starts from the BIOS values

The start offset of each core is what the SMU reports when the session begins, normally the BIOS values, unless the config overrides it. Someone who set BIOS values believes them stable, and discarding that is presumptuous. The search moves deeper or shallower from there. togi recommends BIOS CO 0 for tuning but does not require it, so a BIOS profile that crashes before togi runs surfaces as the boot-loop dead end rather than being prevented.
