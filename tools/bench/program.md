# togi autoresearch: shorten the time to a conclusion

You are an autonomous researcher working on togi, a Go CLI that finds per-core Curve Optimizer offsets on Zen 5 CPUs. Make a tuning session reach its conclusion in less simulated time without making that conclusion worse. Work in a loop of experiments, keep what wins, and discard what does not. Continue until stopped; analyze the results and open pull requests for kept work. The owner merges every pull request and deploys to the target machine manually.

## What conclusion means

A session concludes when every core is at its limit, deepening can reach no more total depth, and a clean lap supports the resulting profile. Eligible earlier uncontradicted laps can supply that evidence. `tools/sim` stops at that point. Time to conclusion is simulated time from `session.start` to the last event, including the 90 s charged for each crash reboot. A full lap records tested coverage, not a guarantee of future stability.

## Setup

1. Create a branch `autoresearch/<tag>` from `main`, with a short descriptive tag. Follow `AGENTS.md` for the repository workflow.
2. Read, in this order:
   - `README.md` and `GLOSSARY.md`, for vocabulary;
   - `docs/spec/tuner.md` and `docs/spec/journal.md`, for normative decisions and their evidence;
   - ADRs 0020, 0023, 0024, 0027, 0028 and 0029 in `docs/adr/`, for decisions and rejected alternatives;
   - `docs/prior-art.md`;
   - `docs/simulating.md` and `internal/sim/doc.go`, for the failure model;
   - `docs/benchmarking.md`, then `tools/bench/suite.toml`, the fitted machines and the harness.
3. Confirm that `tools/bench/baseline.jsonl` exists and covers both splits of the current suite, including `target`. It must describe the unchanged starting tuner, ruleset and evaluation environment; the owner records it with `just bench-baseline`, equivalent to `just bench --split all --out tools/bench/baseline.jsonl`, whenever the tuner on `main`, the suite, the facts or the fitted machines change. If it is missing or stale, report the prerequisite to the owner rather than recording an experiment as the baseline. Pre-oracle baselines do not cover V4.
4. Create `runs/`. Add `runs/`, `results.tsv` and `REPORT.md` to the exclude file located by `git rev-parse --git-path info/exclude` (works in both regular checkouts and linked worktrees). Create `results.tsv` with the header `commit\tdev_ratio\tdev_ci\tholdout_ratio\tcrashes_delta\tdepth_delta\thazard_delta\treal_answer_share\tverdict\tdescription` (tab-separated).
5. Run `just bench --split dev --baseline tools/bench/baseline.jsonl --out runs/0000.jsonl` on the unchanged branch. Require ratios of 1.000, no violations, complete pairing and `ok` model checks for all nine target fits. A mismatch calls for investigating the starting checkout, baseline and harness before experiments. Copy the output to `runs/best.jsonl`. Run `just bench --split holdout --baseline tools/bench/baseline.jsonl --out runs/best-holdout.jsonl` for the unchanged holdout reference and require the same checks.

Measure bench wall time locally; do not use timings or scenario tables from an older ruleset. A pathological run hits the default 180 s per-run wall timeout and counts as not concluding.

## What you may change

- `internal/tuner/**`: search, hunts, deepening and checking decisions.
- Tests and specifications describing a deliberately changed rule, following the repository's conventions.

Everything else is frozen for tuner experiments, especially:

- **The environment:** `internal/sim` and `tools/bench`, including the suite, facts, fitted machines and baseline; `tools/facts`, `tools/fit`, `tools/modelcheck` and `tools/trialfacts`.
- **The run loop and journal:** `internal/session`, `internal/journal` and the journal schema.
- **The hardware path:** `internal/smu`, `internal/trial` and `internal/detect`.

A simulator or machine-file change is an evaluation change, not a tuner experiment. Record ideas needing frozen files in the report for a separate owner decision.

The defaults in `internal/config` are frozen too, and so is the evidence they ask for (the next section's **Evidence volume** rule). Trading evidence for time is safe only when the simulator predicts failures at depths the journal has not tested. The forward-chained check in `just fit` ([benchmarking](../../docs/benchmarking.md#forward-chained-check)) shows that it does not yet: the fit predicts later sessions worse than a constant failure rate. V2 judges hazard from that same simulator, so it cannot catch a shorter trial or fewer starts that miss a real failure. The owner lifts this freeze here, in a separate pull request, once the forward-chained check supports it.

After new real target-machine runs, the owner can refresh the privacy-safe extract with `just facts COPY-OF-STATE-DIR` and refit with `just fit`. Use a temporary copy, never the live state directory. These are owner-triggered evidence updates, not experiments: review their model checks and record a new baseline before comparing tuners in the changed environment. See [benchmarking](../../docs/benchmarking.md) for fitting limits and the checked, constrained bootstrap ensemble.

## Hard rules

Breaking any rule invalidates an experiment, however good its ratio looks.

- **Evidence only.** The tuner decides from journal evidence. It never reads simulator internals, fitted machines, facts extracts, seeds or scenario identities. Never special-case a core number, CCD or scenario's offsets; topology recorded in the journal can inform a tested hypothesis.
- **Traceable and deterministic.** Every new decision is a journal event with a cause and a plain-language `msg`, as the journal spec requires. Replaying the journal gives the same decisions.
- **Safe writes.** Offsets stay clamped to [-50, 0] and pass failure point or combination validation, including intermediate SMU writes.
- **Failure detection stays intact.** Failures remain evidence under the spec's coverage and supersession rules. Never ignore, retry away or reclassify a failure to save time.
- **Evidence volume.** Preserve the spec's start requirements: one R1 and one R2 start for an ordinary search step; the decisive starts set by `[evidence]` miss and rate for candidate-solo-limit checks, hunt groups, deepening checks and rerun obligations. Each trial runs for its configured duration from `[durations]`, and a full lap runs every regime start in `lap = [...]` in `[checking]`. A change may reorder these starts, skip work the spec already allows to be skipped, or avoid redundant trials. It may not ask for fewer starts, shorter trials or a smaller lap, whether through configuration or through tuner code.
- **Tests pass.** Run `go test ./internal/tuner/ ./internal/simrun/ ./cmd/togi/` before each candidate bench run. Update tests for deliberately changed rules; remove tests that only pin obsolete implementation behavior. Run `just gate` before keeping a commit and `just check` before opening a pull request.
- **Proof has three parts.** A kept change wins the bench, preserves every guardrail and bases target-machine claims on fitted files that pass the model check against real facts. The harness prints model flags but does not reject the verdict or change exit status for them: inspect them separately. A flagged target member blocks the target claim even if the verdict says ACCEPT.
- **No ruleset bump per experiment.** One bump covers the kept decision changes when they become pull requests; follow `AGENTS.md`'s breaking-pull-request and release rules. Recheck the final work before opening it. Agents open pull requests; the owner merges and deploys manually.

## The metric and guardrails

`just bench` runs the selected split and compares it with the baseline. [Benchmarking](../../docs/benchmarking.md#comparing-two-versions) defines pairing, the comparison ratio, bootstrap interval, V1–V4 guardrails and verdicts. Check printed verdicts and violations, not just command exit status.

Secondary measures are crashes, trials and hunts. At equal time, prefer fewer crashes; simulated reboot time already contributes to the primary metric.

The `target` scenario uses `target-fit-0.toml` (all decisive starts) and eight whole-trial bootstrap refits. Each member is checked against the original extract. Its replay oracle draws a real outcome when the whole applied profile and trial class match same-BIOS facts; otherwise it uses the fitted member. The tuner sees only the resulting journal. Record candidate and baseline `real_answer_share` from the verdict: the share measures direct real-fact coverage of the path, not confidence. Hazard metrics and model checks describe the fitted fallback, not an empirical oracle hazard.

## The loop

1. **Hypothesis.** Write one falsifiable sentence naming the mechanism, scenarios it should help and expected size. Example: starting a checking lap with the last failed regime should reduce wasted coverage before the next failure on `target` without slowing `default`.
2. **Bound it before building it.** Run `just bench --split dev --baseline runs/best.jsonl --keep runs/dirs` on the current best. Use the printed `runs/dirs/bench-*/<scenario>/dev-<seed>` directories with `just stats --state-dir DIRECTORY`. Measure how much time the mechanism could save; if the bound is under 3% of that scenario's time, pick another idea.
3. **Implement the smallest change** that tests the hypothesis, in one commit. Run the required scoped tests.
4. **Run dev** against the current best: `just bench --split dev --baseline runs/best.jsonl --out runs/<n>.jsonl --keep runs/experiment-<n>`. Require complete pairing and passing target model checks. Seeds are deterministic; rerunning the same commit cannot improve its draw.
5. **Check the mechanism.** Use `just stats --state-dir DIRECTORY` on kept run directories to see whether the predicted quantity moved. Investigate an unexplained win before keeping it.
6. **Decide.** For ACCEPT on dev, run `just bench --split holdout --baseline runs/best-holdout.jsonl --out runs/<n>-holdout.jsonl`. Keep only if holdout has no violations, complete pairing, passing target model checks and a ratio below 1.0. Run `just gate`, then copy the two outputs to `runs/best.jsonl` and `runs/best-holdout.jsonl`. Use holdout once per dev winner, only for confirmation. Discard NEUTRAL, REJECT and failed confirmations by restoring only the experiment's changes; preserve unrelated work.
7. **Log** every experiment, kept or rejected, in `results.tsv`, including model-check failures and real-answer coverage.
8. **Every five kept commits, ablate.** Remove each kept change alone in a separate experimental checkout and rerun dev against the current best. Drop changes that no longer contribute; confirm the revised combination on holdout before treating it as the best.

For a run error, inspect its kept simulator log. Fix a trivial implementation error and start a new logged experiment; otherwise discard it. A timeout is a failure to conclude, not a result to exclude from comparison.

## Ideas and model limits

Measure where the current runs spend time rather than trusting old tables. Possible mechanisms include fewer hunt groups, checking order, evidence reuse allowed by the spec, fewer redundant combination-backoff probes, and search or deepening step sizes. Shorter trials, fewer starts and earlier stopping are frozen under **Evidence volume**; record such ideas in the report with their measured bound.

ADR 0023 rejected probing the shallowest member first, always backing off the shallowest member, moving every member to its passing probe, and reusing member-probe evidence across hunts. Revisit these only with a new argument.

The simulator is a model, not the machine. The fitted ensemble expresses variation within the available real evidence and model constraints, not every hardware uncertainty. Unsupported boundaries, onset timing, idle exposure and attribution have limits described in `docs/benchmarking.md`. Prefer mechanisms grounded in evidence rules over constants fitted to the simulator. Keep the adversarial scenarios as checks beyond the target fit, and scope claims to the measured suite and real-answer coverage. Bench evidence does not authorize a hardware deployment.

## Report and pull requests

When stopped, write an untracked `REPORT.md` covering:

- kept commits in order: hypothesis, mechanism, dev and holdout ratios and intervals, crashes, depth, hazard, target model checks and real-answer shares;
- ablation results;
- rejected ideas with numbers and reasons;
- ideas requiring frozen-file changes;
- the spec sections and ADRs each kept change affects.

Compare the final kept combination against `tools/bench/baseline.jsonl` on both dev and holdout and record the cumulative result, not only incremental wins. Turn kept work into reviewable pull requests following `AGENTS.md`, with the single ruleset bump, matching specs and user-visible changelog entries. Run `just check` on each resulting layer. Include the report's measured proof and its limits in the pull requests. The owner decides what to merge and when to deploy it to the target machine.
