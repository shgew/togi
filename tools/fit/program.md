# togi model research: predict the next real session

Rules for an autonomous researcher improving the simulator's failure model so that a fit to earlier real sessions predicts the next one. The tuner bench (`tools/bench/program.md`) bases its target-machine claims on fitted machines, and a bench win is only as good as they are ([#306](https://github.com/shgew/togi/issues/306)). Agents open pull requests under `AGENTS.md`.

## The metric

`just forward` runs only the [forward-chained check](../../docs/benchmarking.md#forward-chained-check): for each real session after the first, it fits the earlier sessions' decisive trials and scores the held-out session, writing no machine files. `--seal 1` leaves the newest session out entirely; it is the confirmation set, the way the bench keeps a holdout split. `--jobs` bounds the parallel fits; worker count does not change the samples, machine files or report order.

- **Primary:** the `Pooled` `log_loss/trial fit` of `just forward --seal 1`. Lower is better. The `constant` predictor's pooled loss on the same line is the bar a useful model must clear.
- **F1, surprises:** the pooled `failures_p<0.01` must not rise. These are failures the model called nearly impossible; a lower loss bought with more of them hides blind spots.
- **F2, calibration:** the pooled `|ln(predicted / failures)|` must not rise. Log loss alone can improve while the model predicts several times too many failures.
- **F3, breadth:** the fit loss must improve in a majority of the unsealed held-out sessions. The largest session holds about half of the unsealed trials and must not decide alone.
- **F4, in-sample check:** after the sealed confirmation, `just fit --out DIRECTORY` flags no group that the committed `tools/bench/machines/target-fit-*.json` files' `notes` arrays do not already name. `just fit` writes flagged members too ([ADR 0044](../../docs/adr/0044-write-flagged-target-fits.md)), and a flagged member cannot support a target-machine claim, so report which members pass. This fits the full extract, so it is a finalization check, not feedback for dev experiments.
- **F5, opt-in structure:** with the committed machine files, `just bench --split all --baseline tools/bench/baseline.jsonl` pairs every run `equal`. A machine file that does not set a new key behaves exactly as before.

Score the sealed session (`just forward`, no seal) once per dev winner, only for confirmation, after its dev experiments end. It is one session, so the check asks that the winner be no worse within noise: its fit loss must stay at most 1.10 times the unchanged reference's on the same extract, and its `failures_p<0.01` must not rise. Report the 95% interval of that loss ratio from a paired bootstrap that resamples the sealed session's trials; the 1.10 threshold alone decides.

The seal moves forward by itself: when the owner refreshes the extract after a new real run (`just facts`), that run becomes the sealed session and the previously sealed one becomes a dev session. Score every kept model on each refresh.

## What you may change

- The failure model in `internal/sim`: how failure probabilities and hazards follow from a machine's parameters, and the machine-file keys that set new parameters.
- The fitter in `tools/fit`: what it fits, its bounds and its search, and how `sim.EncodeMachine` writes new parameters as JSON. Preserve generated provenance, bootstrap provenance, model-check flags and forward-validation qualifications in the `notes` array of strings.
- Tests, `docs/simulating.md`, `docs/benchmarking.md` and `internal/sim/doc.go` describing a deliberately changed model.

Everything else is frozen:

- **The score:** the scoring, clamping, constant and sealing in `tools/fit/forward.go`, `tools/modelcheck`, `tools/trialfacts` and the extract under `tools/bench/facts/`.
- **The tuner and the bench:** `internal/tuner`, `internal/config`, `tools/bench` and its suite. The committed machine files and baseline change only in a pull request's regeneration commit.
- **The run loop, journal and hardware path:** `internal/session`, `internal/journal`, `internal/smu`, `internal/trial` and `internal/detect`.

## Hard rules

Breaking any rule invalidates an experiment, however good its loss looks.

- **Forward only.** Fit and score only the unsealed sessions during dev experiments. The unsealed reference, candidate confirmation and full-extract F4 check run only after selecting a dev winner, and their sealed outcomes must not guide subsequent experiments. Never edit the extract. No parameter may be keyed to a session, ruleset, date, sequence number or trial ID.
- **No memorized profiles.** Parameters describe cores, CCDs, regimes, workloads, offsets and durations, never one applied profile. The current R7 joints are memorized failing profiles; replacing them is in scope, adding more is not.
- **One rule for draws and scores.** Simulated trials draw failures from the same rules that `Machine.FailureProbability` and `Machine.Hazard` report. A score computed from a model the simulator does not run measures nothing.
- **Deterministic.** Fixed inputs reproduce the forward scores and machine files byte for byte on one architecture; elapsed wall-time lines are excluded. The committed machine files are generated on amd64: compare a candidate with a reference run on amd64, and on another architecture compare decoded values, not bytes ([benchmarking](../../docs/benchmarking.md#fitting-the-target-machine)).
- **Fit time.** Report the `Forward-chained elapsed` line. A change that more than doubles it needs a reason.
- **Tests pass.** Run the affected tests before scoring a candidate, and `just gate` before keeping a commit.

## Method

State one falsifiable hypothesis per experiment and bound it first: if the trials the idea targets cannot move the pooled loss by more than 0.005 per trial, pick another idea. Keep a change only if the primary metric improves and F1–F3 hold; discard a failed confirmation like a loss, restoring only the experiment's changes, and do not tune against its sealed results. Record every experiment, kept or rejected, and ablate kept changes periodically.

R7 evidence is partly contradictory: 35 of 84 R7 failures have a pass at an equal-or-deeper full profile with the same loaded cores. A model that explains R7 by making a deeper offset safer, or by a variable the extract does not record, needs a stated physical argument; record it with the data that would decide it rather than fitting around it. The data are small, and each session ran under a different ruleset whose tuner chose different profiles. Prefer shared parameters with shrinkage over free per-core ones.

One pull request per kept model change, with `Refs #306`, in two commits: the model or fitter change with its tests and docs, then the regeneration (`just fit` rewrites `tools/bench/machines/target-fit-*.json`, `just bench-baseline` re-records the baseline). The body reports pooled loss and `failures_p<0.01` before and after, unsealed and sealed, the model checks, and `just bench --split all --baseline <previous baseline>`. The evidence-volume freeze in `tools/bench/program.md` stays where it is.
