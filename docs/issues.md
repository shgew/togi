# Issues

Read this before filing, triaging or claiming an issue. [AGENTS.md](../AGENTS.md#workflow) owns the change process. Work the owner asks for directly needs no issue, claim or board entry; a tracking issue records the current large change. Issues remember work for later, including deferred review findings and hardware evidence still owed.

Use the templates in `.github/ISSUE_TEMPLATE/`. Keep the body current: Why, Decided, Acceptance, Open, Pull requests, Links and `Touches: path, path` for planned work. A large design lists independently acceptable, revertible [merge units](../REVIEW.md#merge-units) in landing order. Put lasting decisions on [the decisions page](decisions.md) or in a spec; the pull request holds the reasoning, not a new ADR. Use `Refs #N` until all Acceptance is met, then `Closes #N`.

- **Labels:** `idea` is not yet decided; `design` is decided; `feature` is ready to build; `bugfix` repairs a defect; `research` remembers research. `P0`–`P3` set priority; no priority means deliberately unscheduled. Remove `needs-triage` when triaged. `needs-decision` names an owner decision still needed; `needs-hardware` names evidence still owed. Before 1.0, breaking titles and labels are unnecessary.
- **Coordination:** for work chosen from the backlog, check overlapping `Touches:` paths and existing assignees, then record who is working and on which branch. Comments record a changed state or blocker, not recurring status. Unassign unfinished work with a branch and a precise remaining scope. Blocks and GitHub's blocked-by relations express real dependencies, not a required hierarchy for direct requests.
- **Temporary tools:** `just board` and `just claim N BRANCH PLAN [BASE]` remain available until R22 removes their choreography. Neither is a gate in `ship`, CI or `land`; direct requests can omit both. Claim assigns an issue and comments but creates no branch. The board still displays old readiness, stale-claim and Ruleset-issue categories; those categories do not create new obligations.
- **Public interactions:** togi currently takes no outside contributions (`CONTRIBUTING.md`). The collaborator-only GitHub interaction limit expires after at most six months; renew it with `just lock-interactions` when needed.

## Work that waits on the target machine

Record each owed hardware result in the tracking issue or a deferred issue with `needs-hardware`: what merged commit to install, what the owner must run, what it must show and what it unblocks. An otherwise-green pull request can merge using `Refs #N`; the hardware Acceptance remains open. Unassign when only the owner's run remains. If a bug blocks it, retain the remaining steps and a blocked-by link; restore the hardware request once a fix is available on main.

[Hardware first](../AGENTS.md#testing) governs execution: the owner chooses when to update from main and runs `togi night`; afterwards an agent session on the target desktop reads its journal read-only. Agents never run, reset or write togi's state there. A tuning-behavior result blocks designing the next tuning change, not landing unrelated work. A retrospective is optional and owner-invoked.

The old release workflow still refuses publication while any open or closed issue has `needs-hardware`, until R23 removes release tooling. This is not a gate on updating from main; do not start a release to obtain a hardware fix ([releasing](releasing.md)).

## The tech debt register

The pinned Tech debt register (#1281) indexes deferred findings that no feature owns; earlier audits and decisions are in #492. Schedule findings only when useful, and run audits when the owner asks. Retrospectives do not require a triage or debt-analytics round.
