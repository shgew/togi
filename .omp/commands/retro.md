---
description: Write the retro of a target-machine run and file its actions
argument-hint: "[session-id] [--state-dir DIR]"
---

Write the retro of the target-machine run that `$ARGUMENTS` names: a session ID, a copied state directory, both, or nothing for the newest session in `/var/lib/togi`. Read `AGENTS.md`, the Retros and Scoring a forecast sections of `docs/reviewing.md`, and `docs/issues.md` first. `docs/reviewing.md` owns what the retro says and `docs/issues.md` how its actions are filed; this command owns the order of the work. Post and file without asking: the repository's workflow approves it. Use `gh` for every GitHub action, keep scratch files outside the repository, and delete them when done. Every number you publish traces to a command output or an event seq, never to memory.

## 1. Identify the run

`just stats` (with `--state-dir` when given) names the session, its runs, builds and rulesets. On the target machine, match its runs to `journalctl --list-boots`. Read #260's comments for the last session that has a retro, and note any run between it and this one that has none. Done when you know the session, its time range in UTC, every boot it spanned, how it started and how it ended.

## 2. Gather the evidence

Export once to a scratch directory: `just stats`, `togi events`, `togi events --json` and `togi status`, and on the target machine `journalctl -b <boot>` and `journalctl -k -b <boot>` for every boot of the run and the one before it. `/var/lib/togi` cannot be listed without root, but these commands and archived journals read by name work.

Then fan out in ONE parallel task call. Give every agent the export paths; an agent that must run a command needs a shell.

- **Boots:** each boot's entry (tuning or normal), how it ended, the reset reason the next boot reports, startup times, devices that dropped out or came back late, and filesystem, machine-check and thermal messages. Recovery compared with the previous retro.
- **Decisions:** the `tuner.decision`, `failure`, `checking.*` and `hunt.*` events checked against `docs/spec/tuner.md`, each conformant, a spec gap, or a defect with its code location, citing the event seqs and the spec text that decide it; reruns and repeated work counted in trials and hours.
- **Forecast:** when `tools/bench/forecasts/` or an open pull request holds a forecast anchored before this run, its score and flags, as `docs/reviewing.md` says.

Done when every agent reported and you have checked each figure you will publish against the exports.

## 3. Ask the operator

Ask only what needs eyes on the machine: what the dashboard on tty1 showed, by the checks `docs/reviewing.md` lists. Skip it when the run never used the tuning boot.

## 4. Write the retro

Write it by `docs/reviewing.md`, in the full or the short form. Lead each item with the finding, then the numbers; mark inferences. The system journal is full of hostnames, usernames and paths: publish none of them (`AGENTS.md`, Writing). Done when every section the form requires is there and every number traces to step 2.

## 5. File the actions

For each action, find the issue that already owns it (`gh issue list --search`, `just board`) before filing one:

- A defect: a `bugfix` issue with a priority and `Touches:`, under its block.
- A change to tuner decisions: an `idea` sub-issue of the open `Ruleset N` issue, added to that issue's Candidates list.
- Evidence about an open issue's question: a comment on that issue.
- A finding that holds beyond one run: an issue for the pull request that moves it where `docs/reviewing.md` says.

Triage what you file. Done when every action links an issue or a comment.

## 6. Post

Post the retro as a new comment on #260 and reread it there.

## 7. Report

In chat: the retro link, every issue filed or commented, and your overall take: what works, what worries you and what you would do next, each with the number behind it. Offer to grill it. When #492's Status says an audit is due, say so.
