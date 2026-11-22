# togi on one page

The design being built, agreed with the owner on 2026-11-10. It is not yet what the code does. Numbers marked "start" are starting values the machine will correct.

The first version runs CCD nights only. A night finds, soaks and measures the day profile it proposes, but the desktop stays at stock and counts no clean time. Until day tuning lands, Return boots the desktop at stock, the Day section's moves and their notifications do not happen, and the proposed profile's measurements are not gain, which compares stock with the profile in use. This paragraph goes when day tuning lands.

## Words

- **Offset**: a core's Curve Optimizer value, 0 (stock) down to -50. Deeper means more negative.
- **Profile**: one offset for every core.
- **Unit**: what is being tuned. A CCD in stage one, a core in stage two.
- **Edge**: the deepest offset at which a unit passed the night tests; offsets between steps are not tried.
- **Margin**: one number. By day every unit runs at its edge backed off by the margin.
- **Clean time**: hours the desktop has been on with the day profile applied and no crash. It is the only clock togi uses; idle hours count.
- **Crash**: any unclean end while a nonzero profile is applied. togi does not ask why.
- **Wait**: the clean time required before a margin that crashed is tried again.
- **Settled**: the next try is more than 100 clean hours away (start), about two weeks of normal use. It is a label, not a stop: the rule keeps running.

## Night

The owner starts it with `togi night`. The first time, the command states the risk and asks once; after that it reboots at once. Once means once per journal: after a `togi reset` it asks again. The machine reboots into the tuning boot and togi runs everything from there. A night ends when its work is done or the owner ends it; there is no time limit. A crash during a night is expected: the watchdog resets the machine, the tuning boot comes back and the night continues. If togi fails to start three times within 30 minutes, the night ends at stock and says so; a crash during a test is not a failed start.

1. **Self-check** (first night only). A profile known to freeze this desktop is run through each shape for up to 10 minutes (start). At least one shape must fail. If none does, togi stops and says its tests cannot see that failure. A shape that did not fail is recorded as not seeing it. On hardware with no such profile on record, togi refuses the first night before writing any offset.
2. **Baseline** (first night only). Run the three shapes at stock and measure power, temperature and clock speed. This is a measurement, not a test of an offset. A shape that fails at stock ends the night: no offset can be trusted on a machine that fails at stock. A setup problem or a missing sensor is not a failure.
3. **Find edges.** The first probes move every core together, 5 deeper each step (start), until one fails; the last step that passed is where each CCD's search starts, stock if the first step fails. Then each unit is probed on its own: step deeper, 2 each step (start), with short probes until one fails. Each step runs all three shapes. A probe loads only the cores of the unit under test. Every other core sits idle at its day value (its edge backed off by the margin), so a probe sees the neighbours the day will have. The whole-profile soak is the test of all edges together.
4. **Soak.** Run the whole edge profile through all three shapes for longer: twice the probe length (start). A failure that names a core moves that unit's edge back one unit step, the step each unit is probed with; a reset moves every edge back one unit step. A changed profile is soaked again.
5. **Measure.** Run the three shapes on the profile the day will use and record power, temperature and clock speed. A profile that fails here is not proposed: its edges move back as after a soak failure and it is soaked again.
6. **Return.** Boot back into the desktop. The day profile is applied and idle hours count as clean time.

Every night ends the same way, whatever ended it: work done, the owner (F10 or the power button), or a safety stop (failed self-check, failed shape at stock, three failed starts, lost watchdog, failed journal, failed readback). The CPU goes to stock and the machine boots into the desktop at once. `togi status` then leads with why the night ended. The desktop applies the day profile only if its own checks pass (watchdog armed, journal writable, readback matches); otherwise it stays at stock and says why.

Three test shapes, named after what the desktop is used for:

- **build**: every core under test loaded at once.
- **game**: one or a few cores at full boost.
- **idle**: mostly idle, with short bursts hopping between cores.

togi's own supervising process runs on a core that is not loaded when there is one and shares a loaded core when there is none; it never needs a core at stock. A test passes only when every loaded core returned verified results. Wrong result, error, hang or reset is a failure. Probe length is twice (start) how long the self-check took to fail under that shape, never more than the self-check's full time and never less than the shape needs to verify every loaded core; a shape that did not fail there runs for the self-check's full time.

## Day

Automatic at every normal boot. No question is ever asked.

- **Day profile** = each unit's edge backed off by the margin, and never past stock: a unit whose edge is shallower than the margin runs at stock until the margin narrows. The margin starts wide (start: 10).
- **Clean time narrows the margin** by one step (start: 2, then 1 after the first crash) as soon as the required clean time has passed (start: 8 hours). The new profile is written right away, not at the next boot.
- **A crash widens the margin** by one step at the next boot.
- **Wait doubles.** Each crash at a margin doubles the clean time required before that margin is tried again: 8, 16, 32, 64 hours. Nothing is forbidden; an unstable value prices itself out, and a crash togi did not cause costs one short wait.
- **Settled** when the next try is more than 100 clean hours away (start).

## Stages

1. **CCDs.** Units are CCDs (the whole CPU when it has one). The benefit starts the first day a profile is applied.
2. **Cores.** Units are cores. Each core's edge is searched from its CCD's value. The core stage has its own margin: by day a core runs at its edge backed off by that margin, or at its CCD's value, whichever is deeper. Core nights may start as soon as a CCD profile is running. Core day moves start when the CCD stage's next try is more than 56 clean hours away (start), about a week of normal use. From then on the CCD margin no longer narrows; it can still widen.

If the desktop still crashes with every core back at its CCD value, the CCD margin widens. If a night shows a core failing at its CCD's value, that CCD's edge moves back to what the core passed. That is how "the CCD value was wrong after all" is handled.

## Pause

`togi pause` holds the profile with the most clean time (stock if there is none) and stops all moves; clean time does not count while paused. `togi resume` returns to the profile and margin that were running and continues counting from where it stopped.

## Always

- Offsets stay in [-50, 0].
- Every write is journaled before and read back after.
- The firmware stays at stock; togi applies offsets after boot, so a reset always starts at stock.
- The CPU returns to stock whenever togi loses control: lost watchdog, failed journal, failed readback.
- No tuning and no nonzero day profile without an armed hardware watchdog.

## What the owner sees

- `togi status`: stage, today's profile, the margin, clean time, the last move and why, what happens next, whether a night would help, and what was gained.
- A desktop notification only when the profile moves, including when a night's return moves it.
- The night screen on the tuning boot's console: what it is doing, how far along, crashes so far, how to end it.
- **Gain**: for each of the three shapes, power, temperature and clock speed at stock and at the current profile, as measured at night.
