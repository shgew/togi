# Decisions

These decisions govern the design being built in [model.md](model.md), not a claim that the shipped code already implements it. [The tuning guide](how-togi-tunes.md) and specs describe today. Change this page or the relevant spec when a decision changes; put the reasoning in the pull request. Keep this page under about 100 lines. [ADRs](adr/README.md) are history, not authority; add no new ADRs.

## In force

- togi chooses offsets automatically; the owner accepts crash risk rather than choosing or approving profiles, because manual per-core tuning is the work being automated.
- No question gates tuning progress; unclean nonzero ends cause automatic back-off because recovery must not depend on an answer.
- Desktop crashes while searching are accepted measurements, even more than once a day; speed, crash frequency and safety are balanced by the loop.
- CCDs come first for early benefit and the per-core floor; cores follow because per-core offsets are the product.
- Edge, margin and doubling clean-time wait regulate movement; unstable values price themselves out instead of being forbidden.
- Clean applied-profile hours, including idle hours, are the only tuning clock; calendar time does not measure use.
- Other cores idle at their day values during a unit probe; a stock neighbour can hide failures the desktop exposes.
- The supervising process uses an unloaded core when available and shares a loaded one otherwise; it never requires a stock-offset core.
- The three scheduled test shapes are build, game and idle; they represent the target desktop's use without the compulsory nine-shape matrix.
- A pass requires verified results from every loaded core; CPU-time progress alone is not correctness evidence.
- The first night must expose a known-bad profile within minutes; tests that miss the known failure cannot justify deeper tuning.
- Short calibrated probes find edges and one whole-profile soak tests them together; isolated results alone do not qualify a profile.
- The firmware stays at stock and offsets are applied after boot; a reset must start from stock.
- Offsets remain in [-50, 0], with durable intent before each normal write and readback after; loss of control returns the CPU to stock.
- Tuning and nonzero day profiles require an armed hardware watchdog; an advisory warning cannot protect an unattended machine.
- Narrowing writes immediately and crash back-off widens at the next boot; clean-time evidence should yield benefit without an extra reboot.
- Only the owner starts a night with `togi night`; one first-use risk confirmation is enough and subsequent nights reboot at once.
- A night has no fixed time cap and ends when its work is done or the owner ends it; useful work should not be cut off by a schedule.
- Every night end returns to stock and boots the desktop; its own checks decide whether a day profile can be applied.
- Pause holds the profile with the most clean time, stock if none, and stops moves and counting; resume continues the saved loop.
- Notifications report profile moves only; status explains the current stage, evidence, last move and next action without question-driven progress.
- Power, temperature and clock measurements at stock and the current profile show gain; offsets alone do not show the benefit.
- The journal is authoritative and readers reconstruct state without a persisted cache; a second state representation adds invalidation without a measured benefit.
- The new loop starts a fresh journal, neither reads nor imports older generations, and reset archives then starts over; a clean break avoids carrying old policy obligations.
- Runtime support is NixOS with GRUB and one or two full eight-core CCDs; other layouts and bootloaders add unproved paths outside the target.
- macOS supports development, reading and replay, not hardware operation or live watching; the runtime target is Linux.
- Hardware tests run as root and NixOS provisions the host lock; alternate delegation and manual provisioning add unused paths.
- The public commands are night, status, pause, resume, watch, reset and doctor; run is event-only and lifecycle helpers are internal to keep one interface.
- The tuning display is a plain console using standard widgets, with one layout that shows less in small terminals; a GPU terminal adds no tuning value.
- Raw diagnostics have a storage budget, keeping failures first and summaries views use; unread diagnostics do not justify permanent storage.
- The experimental banner stays until a complete search, soak, day application and back-off cycle is observed on the target desktop; software mechanics alone do not establish that result.
- The simulator proves crash recovery, safety and agreement with the page, not tuning policy; each tuning-behavior change reaches hardware before the next is designed.
- The owner runs togi on the target desktop and invokes retros optionally; an agent session there reads the journal read-only to keep execution under owner control.
- Before 1.0 the target follows owner-chosen updates from main, with no releases, new fragments or breaking-change ceremony; model history records behavior changes.
- Incompatible journal or decision changes still bump SCHEMA or RULESET and refuse old journals; abandoning release ceremony does not permit ambiguous replay.
- Direct owner requests use the pull request or large-change tracking issue as their record; separate issues remember deferred work rather than gate every edit.
- Safety review follows the rule or enforcement being changed, wherever its file lives; directory names do not establish risk.
- Existing write, recovery, containment and durability safety assertions survive removals and ruleset changes; retiring policy does not retire safety invariants.

## Rejected

- An operator-set pin or profile is not the product; it makes the owner choose the values togi exists to find.
- Artificial offset depth limits are rejected; they slow the search instead of learning from failures.
- Fixed 50-hour stock or per-move holds are rejected; clean-time margin and doubling wait regulate risk without fixed delays.
- A lifetime crash allowance is rejected; past crashes should price retries, not consume a permanent entitlement.
- Permanent stops, sticky vetoes and guarded restoration are rejected; unstable values remain retryable at an increasing cost.
- Scheduled or shutdown-triggered nights are rejected; only explicit owner-started nights may take over the desktop.
- Crash questions and a "that was me" annotation are rejected; automatic back-off makes a wrongly blamed crash cheap without another interface.
- A predictive simulator, fitted machine models and successor scores are rejected; synthetic forecasts cannot decide how the target desktop tunes.
- Mandatory reproduction nights are rejected; retained failures and the doubling wait suffice without another gate.
- Old-journal decoding and import after the new loop's cutover are rejected; they preserve superseded policy and compatibility work.
- A persisted state cache and build fingerprint are rejected; journal-only reading was faster in the measured comparison and has fewer dependencies.
- Fancy graphics and a GPU terminal are rejected; the night needs a readable plain-console screen, not a separate display service.
