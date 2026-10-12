# Bench children keep temporary journals

Status: see [the index](README.md).

Decides how the Rust bench's children hand their sessions back ([#613](https://github.com/shgew/togi/issues/613), [#868](https://github.com/shgew/togi/issues/868)), and narrows, for bench children, the "simulations never touch disk" rule of the open ADR issue [#611](https://github.com/shgew/togi/issues/611).

## Context

`tools/bench` runs every session as a `togi-tool-sim` child with its own state directory; the parent reads and parses the child's journal after it exits and derives the record from it. `same`, `smoke` and `audit` run their children the same way. #613 decided that the children should run in memory and report a digest of each journal with their exit status, so a full bench or `same` would write no journal, because writing the journals looked like the cost.

Measuring the Rust children showed the cost was elsewhere. Until [#871](https://github.com/shgew/togi/pull/871) the disk mode published `state.json` after every event; #871 publishes it only when a run ends or, after a crash, from its capsule (`PublicationPolicy::OnFinish`). The figures below are from one loaded host, release builds and tmpfs scratch, comparing the files mode before #871, after it, and a child kept in memory (its log only):

| | Before #871 | After #871 | In memory |
|---|---:|---:|---:|
| Default session (9,383 events, 3.64 MB journal): bytes written | 347.6 MB | 5.04 MB | 0.93 MB |
| Default session: child CPU | 0.85–1.1 s | ~0.2 s | ~0.2 s |
| Eleven-scenario sample: bytes written | 8.8 GiB | 153 MiB | 17 MiB |
| Eleven-scenario sample: child CPU | 48.5 s | 24.0 s | 27.9 s |

On the dev split (110 sessions, 2.05 M events, 680 MiB of journals and 161 MiB of logs), the bench took 37.5 s of wall time before #871 and 27.0 s after, and 518 s of user and system CPU against 330 s; the records were identical apart from `wall_s`. In memory, child CPU was within the noise (about 30%) of the files mode after #871, and resident memory was 0–30 MiB higher. The parent's read and parse of a journal took 19–66 ms per session, about 5.4 ms per MiB and about 1% of the dev split's CPU; a SHA-256 of a journal costs about 0.4 ms per MiB.

The rejected options below were costed piece by piece, not end to end.

## Decision

**Bench, `same`, `smoke` and `audit` children keep running with a temporary `--state-dir`, and the parent reads and parses each journal after the child exits, as the Go bench does.** #613's real cost was the per-event `state.json` publication, which #871 removed; what remains is cheaper than any change of protocol would save. `--keep` retains the run directories ([benchmarking](../benchmarking.md)).

**`same` still reruns a differing session with its files kept**, so the evidence of a difference is the journal itself.

This narrows #611's rule for bench children only: their journals are files on scratch storage, read back by the parent. Tests and other simulations are unaffected by this ADR.

## Considered Options

- **Events on the child's stdout:** rejected. The parent parses the same bytes it would read from the file, and a child that crashes leaves only what the pipe delivered, with no journal to diagnose.
- **A dedicated pipe for events:** rejected for the same reason: a crashed child leaves no journal behind, and the pipe adds a protocol to save a read the measurements put at about 1% of the CPU.
- **Children report only a digest:** rejected. The record needs the metrics, so about 560 lines of metric code would move into the child, and `same`'s base build would be tied to a digest protocol that the base tree must also speak.
- **Children run in memory:** rejected. After #871 it is no cheaper: child CPU is within the noise of the files mode, and resident memory is higher.

## Consequences

- Bench, `same`, `smoke` and `audit` write each session's journal to scratch storage for as long as the run lasts; a bench on a host whose temporary directory is on disk writes about 680 MiB of journals for the dev split.
- A crashed child leaves the journal it had written, for diagnosis, and `--keep` retains it.
- The child's protocol stays its exit status and its state directory, so any `togi-tool-sim` binary, including a base tree's, can be benched or compared.
- A later change that makes the parent's read the cost again reopens the question with new measurements, end to end.
