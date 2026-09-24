# Changelog

All notable changes to shycler are documented in this file. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- The wordmark and tagline print above the usage when `shycler` runs without a command ([#11]).
- `shycler regain` queues one count of regain on cores with unproven depth, and `shycler reset` restarts one core's search or archives the whole session ([#9]).
- Durability tiers, `shycler status` for the session at a glance, and `shycler cert` for the certificate with the edges to enter in BIOS ([#8]).
- The guard: after every core is confirmed, all offsets are tested together in endless rotations through R1 to R7, backing off a core that fails and counting clean hours ([#7]).
- `shycler run --sim <seed>` runs a whole session on a simulated machine, and a run after a crash attributes it to the action in flight and continues ([#6]).
- A seeded simulator of a 16-core Zen 5 machine, with hidden per-core edges, random failures and crashes ([#5]).
- Per-core search, in steps of 5 and then 1 from the starting offset towards -50, and confirmation across regimes R1 to R5 at the candidate edge ([#4]).
- The journal: every action and decision is recorded crash-safely in `events.jsonl`, and `shycler events` prints it, filtered by core, kind, trial or time ([#3]).
- The `shycler` command, its TOML configuration, which rejects unknown keys and out-of-range values, and a Nix flake with the package and a dev shell ([#2]).

### Changed

- Every command's `--help` gives a description and examples, lists its own flags before the global ones, and shows every flag in `--long` form ([#12]).

[#2]: https://code.marleb.org/shgew/shycler/pulls/2
[#3]: https://code.marleb.org/shgew/shycler/pulls/3
[#4]: https://code.marleb.org/shgew/shycler/pulls/4
[#5]: https://code.marleb.org/shgew/shycler/pulls/5
[#6]: https://code.marleb.org/shgew/shycler/pulls/6
[#7]: https://code.marleb.org/shgew/shycler/pulls/7
[#8]: https://code.marleb.org/shgew/shycler/pulls/8
[#9]: https://code.marleb.org/shgew/shycler/pulls/9
[#11]: https://code.marleb.org/shgew/shycler/pulls/11
[#12]: https://code.marleb.org/shgew/shycler/pulls/12
