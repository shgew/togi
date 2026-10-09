# Record work that waits on the target machine

Some changes can only be verified on the target machine: the SMU and PM-table paths, the CPU and driver checks, and the real mprime and y-cruncher workloads. The NixOS VM tests cannot reach them, because QEMU provides no SMU mailbox and the CPU check accepts only Granite Ridge desktops. When the owner is away from the machine, such a pull request either waits or merges with nothing recording what is still owed. [#417](https://github.com/shgew/togi/issues/417) and [#418](https://github.com/shgew/togi/issues/418), with pull requests [#469](https://github.com/shgew/togi/pull/469) and [#476](https://github.com/shgew/togi/pull/476), were the first two cases: their Acceptance requires `just hardware`, and the owner decided on 2026-10-07 to merge them before that run and keep the run as a recorded obligation ([#478](https://github.com/shgew/togi/issues/478)).

## Decision

- The `needs-hardware` label marks an issue that waits on a run on the target machine. A comment on the issue gives the exact steps: what to check out, which commands to run, what each must show and what it unblocks.
- `just board` lists the open `needs-hardware` issues in their own section, "Waiting on the target machine (needs-hardware)", right after "Waiting on you (needs-decision)", and nowhere else: not under Ready, In progress or Overlaps, whether assigned or not.
- A pull request that needs the machine merges once everything else is green. It says `Refs #N` instead of `Closes #N`, and its issue keeps the label.
- The queue must be empty before the owner updates the target machine's togi input or cuts a release. The release workflow refuses to commit a release while any issue, open or closed, carries `needs-hardware`, naming each issue by number and title and marking the closed ones, before it waits on the check run; its token reads issues for that. Updating the machine's input happens outside this repository, so that half is a rule in `docs/issues.md` only.
- At the machine, an agent may work the queue without asking: run each issue's steps, post the output as a comment, and remove the label once they pass; then close the issue when nothing else in its Acceptance remains. On a failure it files a `bugfix` issue with a priority, links it from the original, and leaves the original with its label.

## Considered Options

- **One pinned checklist issue of owed runs:** rejected. It duplicates the state the issues already hold, and each run, closure or relabeling leaves a line someone must edit by hand.
- **Appending owed runs to [#260](https://github.com/shgew/togi/issues/260):** rejected. That issue holds the retros of target-machine runs; mixing obligations into a history hides what is still owed, and neither `just board` nor the release can query it.
- **Keeping such pull requests open until the run:** rejected. The owner's time at the machine would then hold back reviewed, otherwise green changes, the stack layers above them and every issue whose `Touches:` overlaps them.
- **A VM check for the backend suite:** rejected. QEMU has no SMU, the CPU check accepts only Granite Ridge desktops, GitHub-hosted runners do not guarantee AVX-512, and mprime's license does not clearly allow pushing built binaries to the public Cachix cache the checks substitute from.
- **Unlinking the branch or pull request from the issue before merging:** rejected. It adds a manual step to every such merge, and one forgotten unlink silently drops the issue from the queue; counting the label holds whatever GitHub does to the issue's state.

## Consequences

- `main` can hold merged code that has not yet run on the target machine. The label and `just board` say which, and the release refusal keeps it out of a release.
- `just release-preview` reads no GitHub data, so it does not check the queue; the refusal comes from the workflow run `just release` starts. Run `just board` first to see the queue.
- A release whose commit already reached `main` and only needs publishing again is not refused, so a half-finished release can always complete.
- Nothing enforces the rule for updating the target machine's togi input; it relies on the owner checking `just board` first.

## Note, 2026-10-09

`just claim` stopped linking branches with `gh issue develop` ([#500](https://github.com/shgew/togi/issues/500)), so a merge no longer closes an issue whose pull request says only `Refs`. Before that, GitHub closed such issues on merge ([#417](https://github.com/shgew/togi/issues/417), [#418](https://github.com/shgew/togi/issues/418)), and the board and the docs counted the label over the open state, including closed issues. The board now queries open issues only. The release refusal still reads issues in every state, which costs nothing and fails safe.
