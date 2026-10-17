# togi model research: predict the next real session

Rules for the **model development** mode of a [research campaign](../../docs/benchmarking.md#research-campaigns): improving the simulator's failure model so that a fit to earlier real sessions predicts the next one. The tuner bench (`tools/bench/program.md`) bases its target-machine claims on fitted machines, and a bench win is only as good as they are ([#306](https://github.com/shgew/togi/issues/306)). The campaign's limits, prohibitions, record and end state are in [Research campaigns](../../docs/benchmarking.md#research-campaigns); the issue's brief names its hypotheses and data:

- [#427](research/427.md): one structured single-core hazard.
- [#480](research/480.md): diagnosing the failed shared-voltage forward score, development only.
- #428's brief waits for #427's parameterization: its uncertainty draws need their own metric, since the pooled forward loss below need not move for them.

## The data

A campaign reads only the sessions its brief approves, never the whole committed extract. Its development artifact, `ARTIFACT` below, is a gzip extract of the original seven sessions, 20260924T204352Z through 20261004T000701Z: their 3,297 decompressed rows, in extract order, hash to SHA-256 `4b47590e12bd55bc73ee3ae468a782173acbafd4599892ef003e2371a8c066b7`. The extract committed at `455704be` is exactly these rows, and so are the committed extract's rows for those sessions. A different hash stops the campaign.

- **Development:** the first six sessions, through 20261002T235548Z.
- **Historical confirmation:** 20261004T000701Z. It was scored before (#480's first candidate) and is not blind; report it as a historical check, never a fresh blind confirmation.
- **Forbidden:** #480's spent score sessions 20261004T211726Z and 20261010T230611Z, every session recorded after them, and anything their scored outcomes suggested. They never feed fitting, selection, features or ablations, and a refreshed extract does not make them development data. A seal count is not a permission: on the committed extract `--seal 1` scores the spent 20261004T211726Z.

A new blind claim needs new evidence recorded after the candidate freezes, scored once through a separate owner-approved step.

## The metric

`just forward --facts ARTIFACT` runs only the [forward-chained check](../../docs/benchmarking.md#forward-chained-check) on the approved artifact: for each session after the first, it fits the earlier sessions' decisive trials and scores the held-out session, writing no machine files. `--seal 1` leaves the historical confirmation session out. `--jobs` bounds the parallel fits; worker count does not change the samples, machine files or report order.

- **Primary:** the `Pooled` `log_loss/trial fit` of `just forward --facts ARTIFACT --seal 1`. Lower is better. The `constant` predictor's pooled loss on the same line is the bar a useful model must clear.
- **F1, surprises:** the pooled `failures_p<0.01` must not rise. These are failures the model called nearly impossible; a lower loss bought with more of them hides blind spots.
- **F2, calibration:** the pooled `|ln(predicted / failures)|` must not rise. Log loss alone can improve while the model predicts several times too many failures. A subset with no failures reports its predicted count instead of a ratio.
- **F3, breadth:** the fit loss must improve in a majority of the development held-out sessions. The largest session holds about half of their trials and must not decide alone.
- **F4, in-sample check:** after the confirmation, `just fit --facts ARTIFACT --out DIRECTORY`, with `DIRECTORY` inside the campaign's output directory, flags no group that the committed `tools/bench/machines/target-fit-*.json` files' `notes` arrays do not already name. `just fit` writes flagged members too ([ADR 0044](../../docs/adr/0044-write-flagged-target-fits.md)), and a flagged member cannot support a target-machine claim, so report which members pass. This fits the whole artifact, so it is a finalization check, not feedback for dev experiments.
- **F5, opt-in structure:** with the committed machine files, `just bench --split all --baseline tools/bench/baseline.jsonl` pairs every run `equal`. A machine file that does not set a new key behaves exactly as before.

Score the historical confirmation session (`just forward --facts ARTIFACT`, no seal) once per campaign, for its one dev winner, after its dev experiments end. It is one session, so the check asks that the winner be no worse within noise: its fit loss must stay at most 1.10 times the unchanged reference's on the same artifact, and its `failures_p<0.01` must not rise. Report the 95% interval of that loss ratio from a paired bootstrap that resamples the session's trials; the 1.10 threshold alone decides. A failed confirmation ends the campaign; its outcome never reopens the candidate queue.

## What you may change

- The failure model in the `togi-sim` crate: how failure probabilities and hazards follow from a machine's parameters (`Model::failure_probability`, `Model::hazard` and the sources behind them), and the machine-file keys that set new parameters.
- The fitter in `togi-tool-fit`: what it fits, its bounds and its search, and how `togi_sim::file::encode` writes new parameters as JSON. Preserve generated provenance, bootstrap provenance, model-check flags and forward-validation qualifications in the `notes` array of strings.
- Tests, `docs/simulating.md`, `docs/benchmarking.md` and the `togi-sim` crate documentation describing a deliberately changed model.

Everything else is frozen:

- **The score:** the scoring, clamping, constant and sealing in `crates/togi-tool-fit/src/forward.rs`, the `togi-tool-modelcheck` and `togi-tool-trialfacts` crates, and the extract under `tools/bench/facts/`.
- **The tuner and the bench:** `togi-tuner`, `togi-config`, `togi-tool-bench` and `tools/bench` with its suite, committed machine files and baseline.
- **The run loop, journal and hardware path:** `togi-session`, `togi-journal`, `togi-smu`, `togi-trial` and `togi-detect`.

## Hard rules

Breaking any rule invalidates an experiment, however good its loss looks.

- **Forward only.** Fit and score only the development sessions during dev experiments. The confirmation and F4 run only after selecting the dev winner, and their outcomes must not guide another experiment. Never edit the extract. No parameter may be keyed to a session, ruleset, date, sequence number or trial ID.
- **No memorized profiles.** Parameters describe cores, CCDs, regimes, workloads, offsets and durations, never one applied profile. The current R7 joints are memorized failing profiles; replacing them is in scope, adding more is not.
- **One rule for draws and scores.** Simulated trials draw failures from the same rules that `Model::failure_probability` and `Model::hazard` report. A score computed from a model the simulator does not run measures nothing.
- **Deterministic.** Fixed inputs reproduce the forward scores and machine files byte for byte on one host; elapsed wall-time lines are excluded. Compare a candidate with a reference run on the same host; across hosts compare decoded values, not bytes ([benchmarking](../../docs/benchmarking.md#fitting-the-target-machine)).
- **Fit time.** Report the `Forward-chained elapsed` line. A change that more than doubles it needs a reason.
- **Tests pass.** Run the affected tests before scoring a candidate, and `just gate` before keeping a commit.

## Method

State one falsifiable hypothesis per experiment and bound it first: if the trials the idea targets cannot move the pooled loss by more than 0.005 per trial, pick another idea. Keep a change only if the primary metric improves and F1–F3 hold; discard a failed confirmation like a loss, restoring only the experiment's changes. Record every experiment, kept or rejected, in the campaign's ledger, and ablate kept changes periodically.

R7 evidence is partly contradictory: 35 of 84 R7 failures have a pass at an equal-or-deeper full profile with the same loaded cores. A model that explains R7 by making a deeper offset safer, or by a variable the extract does not record, needs a stated physical argument; record it with the data that would decide it rather than fitting around it. The data are small, and each session ran under a different ruleset whose tuner chose different profiles. Prefer shared parameters with shrinkage over free per-core ones.

A kept model becomes one pull request with `Refs` to its issue and #306: the model or fitter change with its tests and docs, and a body reporting pooled loss and `failures_p<0.01` before and after on development and historical confirmation, the model checks, and F5. It regenerates nothing: the fitted ensemble, anchor, overlays and baseline wait until a later #480 candidate passes a fresh score, and each regeneration is its own owner-approved step. The evidence-volume freeze in `tools/bench/program.md` stays where it is.
