# shycler

Finds the deepest per-core Curve Optimizer offsets a Zen 5 desktop CPU sustains, then keeps testing them so the result earns a durability tier: Bronze, Silver, Gold, Platinum.

- Runs in a normal session with `sudo shycler run`, or unattended from its own GRUB boot entry, resuming across crash reboots.
- Tests with self-checking workloads (mprime, y-cruncher) across light, heavy, load-step, medium, SMT, idle and all-core regimes.
- Records every action in a plain-text journal you can read to see what it did and why.
- Reports the offsets; you enter them in BIOS.

Status: design. See `docs/ROADMAP.md`.

Start with `CONTEXT.md` for the vocabulary, then `docs/spec/tuner.md`.
