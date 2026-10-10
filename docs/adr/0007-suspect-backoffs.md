Status: see [the index](README.md).

# Unattributed failures back off as suspects; depth is regained only on request

With the whole profile resident, a crash without a core-naming MCE cannot be pinned on one core. togi backs off the loaded core first and escalates to every core if another unattributed failure follows before a clean rotation. These backoffs are recorded as suspect, so the lost depth is visible as unproven depth. `togi regain` retries it only when the owner asks, typically once the profile holds Silver or better. This converges quickly without silently giving up depth.

## Considered Options

- **Back off every core immediately:** simplest, but loses the most depth.
- **Automatic multi-boot bisection (linux-corecycler's crash hunt):** precise but the largest single source of complexity in the fork.
- **Automatic regain at a tier threshold:** deferred; it trades certificate progress for depth without the owner deciding.
