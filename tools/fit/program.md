# togi model research: predict the next real session

You are an autonomous researcher working on togi, a Go CLI that finds per-core Curve Optimizer offsets on Zen 5 CPUs. Improve the simulator's failure model so that a fit to earlier real sessions predicts the next one. The tuner bench (`tools/bench/program.md`) bases its target-machine claims on fitted machines, and a bench win is only as good as they are ([#306](https://github.com/shgew/togi/issues/306)). Work in a loop of experiments, keep what wins, and discard what does not. Continue until stopped; analyze the results and open pull requests for kept work. The owner merges every pull request.

## The metric

`just forward` runs only the [forward-chained check](../../docs/benchmarking.md#forward-chained-check): for each real session after the first, it fits the earlier sessions' decisive trials and scores the held-out session. It writes no machine files. `--seal 1` leaves the newest session out entirely; it is the confirmation set, the way the bench keeps a holdout split.

`just fit` and `just forward` run independent fits in parallel, bounded by `--jobs` (default: CPU count). Use `--jobs 1` for serial execution or the same explicit worker count for reference and candidate timing comparisons. Worker count does not change the samples, machine files or report order.

- **Primary:** the `Pooled` `log_loss/trial fit` of `just forward --seal 1`. Lower is better. The `constant` predictor's pooled loss on the same line is the bar a useful model must clear.
- **F1, surprises:** the pooled `failures_p<0.01` must not rise. These are failures the model called nearly impossible; a lower loss bought with more of them hides blind spots.
- **F2, calibration:** the pooled `|ln(predicted / failures)|` must not rise. Log loss alone can improve while the model predicts several times too many failures.
- **F3, breadth:** the fit loss must improve in a majority of the unsealed held-out sessions. The largest session holds about half of the unsealed trials and must not decide alone.
- **F4, in-sample check:** after the sealed confirmation, `just fit --out runs/fit-<n>` flags no group that the committed `tools/bench/machines/target-fit-*.toml` headers do not already name ([#306](https://github.com/shgew/togi/issues/306)'s merge bar). Since [ADR 0044](../../docs/adr/0044-write-flagged-target-fits.md), `just fit` writes flagged members too, and each member's header and the report name its flagged groups. A flagged member still cannot support a target-machine claim, so report which members pass. This fits the full extract, so it is a finalization check, not feedback for dev experiments. The bench's `target` scenario relies on that check.
- **F5, opt-in structure:** with the committed machine files, `just bench --split all --baseline tools/bench/baseline.jsonl` pairs every run `equal`. A machine file that does not set a new key behaves exactly as before, so the default machine, the hand-written scenarios and the committed fits keep their meaning until they are regenerated.

Score the sealed session (`just forward`, no seal) once per dev winner, only for confirmation, after its dev experiments end. It is one session, so the check asks that the winner be no worse within noise, not better: its fit loss must stay at most 1.10 times the unchanged reference's on the same extract, and its `failures_p<0.01` must not rise. Report the 95% interval of that loss ratio from a paired bootstrap that resamples the sealed session's trials, scoring both models on the same resamples. The interval shows how much one session can tell apart; the 1.10 threshold alone decides.

The seal moves forward by itself. When the owner refreshes the extract after a new real run (`just facts`), that run becomes the sealed session and the previously sealed one becomes a dev session. Every refresh is a confirmation no experiment has seen; score every kept model on it.

## Setup

1. Create a branch `model/<tag>` from `main`, with a short descriptive tag. Follow `AGENTS.md` for the repository workflow.
2. Read, in this order:
   - `README.md` and `GLOSSARY.md`, for vocabulary;
   - `docs/simulating.md` and `internal/sim/doc.go`, for the failure model;
   - `docs/benchmarking.md` from "Checking a machine against real evidence" to the end, for the model check, the fitter and the forward-chained check;
   - ADR 0029 in `docs/adr/`, for why bench verdicts rest on fitted machines;
   - issue #306, for the measured structural problems, the planned model changes and what is still open;
   - `tools/bench/program.md`, for how the tuner research uses your machines.
3. Create `runs/`. Add `runs/`, `results.tsv` and `REPORT.md` to the exclude file located by `git rev-parse --git-path info/exclude`. Create `results.tsv` with the header `commit\tdev_loss\tconstant_loss\tsurprises\tpredicted\tfailures\tsessions_improved\tsealed_loss\tverdict\tdescription` (tab-separated).
4. On the unchanged branch, record the dev starting point: `just forward --seal 1 > runs/0000.txt`. Preserve the unchanged model with `git worktree add --detach runs/reference HEAD`; use the same extract in that checkout and the experiment checkout. Leave the unsealed reference and full-extract fit until confirmation.

## What you may change

- The failure model in `internal/sim`: how failure probabilities and hazards follow from a machine's parameters, and the machine-file keys that set new parameters.
- The fitter in `tools/fit`: what it fits, its bounds and its search, and how `encodeMachine` writes new parameters.
- Tests, `docs/simulating.md`, `docs/benchmarking.md` and `internal/sim/doc.go` describing a deliberately changed model.

Everything else is frozen, especially:

- **The score:** the scoring, clamping, constant and sealing in `tools/fit/forward.go`, `tools/modelcheck`, `tools/trialfacts` and the extract under `tools/bench/facts/`.
- **The tuner and the bench:** `internal/tuner`, `internal/config`, `tools/bench` and its suite. The committed machine files and baseline change only in a pull request's regeneration commit (below).
- **The run loop, journal and hardware path:** `internal/session`, `internal/journal`, `internal/smu`, `internal/trial` and `internal/detect`.

## Hard rules

Breaking any rule invalidates an experiment, however good its loss looks.

- **Forward only.** Fit and score only the unsealed sessions during dev experiments. The unsealed reference, candidate confirmation and full-extract F4 check run only after selecting a dev winner; their sealed outcomes must not guide subsequent experiments. Never edit the extract. No parameter may be keyed to a session, ruleset, date, sequence number or trial ID.
- **No memorized profiles.** Parameters describe cores, CCDs, regimes, workloads, offsets and durations, never one applied profile. The current R7 joints are memorized failing profiles; replacing them is in scope, adding more is not.
- **One rule for draws and scores.** Simulated trials draw failures from the same rules that `Machine.FailureProbability` and `Machine.Hazard` report. A score computed from a model the simulator does not run measures nothing.
- **Deterministic.** Fixed inputs reproduce the forward scores and machine files byte for byte on one architecture; elapsed wall-time lines are excluded. Compare a candidate with a reference run on the same architecture: amd64 and arm64 can differ in the last digits ([benchmarking](../../docs/benchmarking.md#fitting-the-target-machine)).
- **Fit time.** Report the `Forward-chained elapsed` line. A change that more than doubles it needs a reason in the report.
- **Tests pass.** Run `go test ./internal/sim/ ./tools/fit/ ./tools/modelcheck/ ./tools/bench/` before scoring a candidate. Run `just gate` before keeping a commit and `just check` before opening a pull request.

## The loop

1. **Hypothesis.** Write one falsifiable sentence naming the structure, the held-out trials it should explain and the expected effect. Example: a smooth per-CCD R7 hazard in place of memorized joints should turn most of the ruleset-4 R7 failures now at p < 0.01 into expected ones without predicting more than twice the observed R7 failures.
2. **Bound it before building it.** Use the per-regime rows of `runs/<best>.txt` to see where predicted and observed failures diverge. If the trials the idea targets cannot move the pooled loss by more than 0.005 per trial, pick another idea.
3. **Implement the smallest change** that tests the hypothesis, in one commit. Run the required tests.
4. **Score:** `just forward --seal 1 > runs/<n>.txt`.
5. **Decide.** Keep only if the primary metric improves and F1–F3 hold. At confirmation, obtain the unchanged reference with `(cd runs/reference && just forward) > runs/0000-sealed.txt`, then confirm with `just forward > runs/<n>-sealed.txt`. Check F4 with `just fit --out runs/fit-<n>` and F5 with the bench. These full-extract checks are permitted only at this stage. Discard a failed confirmation like a loss, restoring only the experiment's changes; do not tune against its sealed results.
6. **Log** every experiment, kept or rejected, in `results.tsv`.
7. **Every three kept commits, ablate.** Remove each kept change alone and rescore; drop changes that no longer contribute.

## Ideas and limits

The planned model changes in #306 are the starting queue: a structured single-core hazard with per-core intercepts shrunk toward a shared mean, workload effects shared across cores and one limit for alone and together trials; and a smooth hazard per loaded CCD for R7. A throwaway prototype of both reached a pooled loss of 0.195 with 5 surprises on all four held-out sessions, but predicted 295.5 failures against 107, almost all from R7: F2 exists for that case.

R7 evidence is partly contradictory: 35 of 84 R7 failures have a pass at an equal-or-deeper full profile with the same loaded cores. A model that explains R7 by making a deeper offset safer, or by a variable the extract does not record, needs a stated physical argument. Record the argument and the data that would decide it, such as the targeted hardware probes in #306, rather than fitting around it.

The forward check scores failure probability only. Fitting the failure signal mix and drawing ensemble members from parameter uncertainty (#306) do not move it; record evidence for them in the report for the owner to decide.

The data are small: a few sessions and about a hundred failures, each session under a different ruleset whose tuner chose different profiles. Prefer shared parameters with shrinkage over free per-core ones, and expect a held-out session to test extrapolation to new profiles more than repetition.

## Report and pull requests

When stopped, write an untracked `REPORT.md` covering kept changes in order with their hypothesis, unsealed and sealed scores, F1–F5 evidence and elapsed time; ablation results; rejected ideas with numbers and reasons; and ideas that need frozen files or new hardware evidence.

Open one pull request per kept model change, as #306 plans, with `Refs #306`. Each has two commits: the model or fitter change with its tests and docs, then the regeneration: `just fit` rewrites `tools/bench/machines/target-fit-*.toml`, and `just bench-baseline` re-records `tools/bench/baseline.jsonl`. The body reports pooled loss and `failures_p<0.01` before and after, unsealed and sealed, the model checks, and `just bench --split all --baseline <previous baseline>` so the owner sees how the tuner's measured environment moved. The evidence-volume freeze in `tools/bench/program.md` stays where it is; argue in the report when the forward check supports lifting it.
