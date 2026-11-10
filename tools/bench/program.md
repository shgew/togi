# togi tuner research: improve the registered day/night metrics

**Historical only.** Day/night scoring and campaign launching are [retired](../../docs/benchmarking.md#daynight-gate). This retained program is not executable guidance or authorization for a new campaign. Use the [original program at `archive/pre-cut`](https://github.com/shgew/togi/blob/archive/pre-cut/tools/bench/program.md) when reading historical results.

## Successor registration

The [archived successor registration](https://github.com/shgew/togi/blob/archive/pre-cut/tools/bench/day-night-successor.json) owned the separate
successor experiment's numbers and implemented comparator commands; see
[its registration status](../../docs/benchmarking.md#successor-registration).
D's source stays explicitly pending until a separate reviewed pinning commit
registers implemented candidate commands; no D scoring is permitted before it.
Holdout is refused by the pinned runner and requires a later reviewed
implementation and the repository owner's explicit authorization; only the
owner runs the confirmation. This registration neither
changes the historical program below nor authorizes a campaign, confirmation,
compatibility switch or deployment.

## Original Ruleset 13 program

Historical rules for the **production policy experiment** mode of a [research campaign](https://github.com/shgew/togi/blob/archive/pre-cut/docs/benchmarking.md#research-campaigns). The objective was the Ruleset 13 [day/night gate's registered metrics](https://github.com/shgew/togi/blob/archive/pre-cut/docs/benchmarking.md#daynight-gate) on development seeds, not a session's time to conclusion. The [archived registration](https://github.com/shgew/togi/blob/archive/pre-cut/tools/bench/day-night.json) owns the scenarios, seeds, calendar, hazard budget, thresholds and tail treatment.

Observation campaigns change no tuner code: they measure the unchanged tuner, adding only the analysis tools and research-only fixtures their brief names and leaving the frozen environment below as it is. Their brief:

- [#503](research/503.md): fixed-policy observation, feasibility of an online risk learner.

#503 runs only in that mode: a simulated gain never lifts the evidence volume freeze below.

## Start

Branch from `main` and follow `AGENTS.md`. Read the [tuner spec](../../docs/spec/tuner.md), [workload spec](../../docs/spec/workloads.md), [journal spec](../../docs/spec/journal.md), [prior art](../../docs/prior-art.md) and [benchmarking guide](../../docs/benchmarking.md). State one falsifiable hypothesis, the registered metrics it should improve and the evidence rules it preserves.

The retired dispatcher read a campaign's approved brief, finite hypothesis queue, writable paths, pinned inputs, limits and confirmation allowance from its manifest. It registered only model-development and fixed-policy-observation briefs and forbade tuner edits. There is no current campaign-launch interface.

`tools/bench/baseline.jsonl` is the historical legacy session-suite baseline. Model development's F5 check still pins it; it is not the day/night objective or a reference for its scores. This program changes neither that guard nor the baseline.

## What you may change

- The current tuner policy modules in `crates/togi-tuner/src/policy/`.
- Tests and specifications describing a deliberately changed policy rule, without changing the frozen evaluation or evidence requirements.

Everything else is frozen for tuner experiments:

- **The environment:** the simulator (`togi-sim`, `togi-simrun`, `togi-tool-sim`) and the bench (`togi-tool-bench` and `tools/bench`, including the registrations, suites, facts, fitted machines and historical baseline); `togi-tool-facts`, `togi-tool-fit`, `togi-tool-modelcheck`, `togi-tool-trialfacts` and `togi-facts`.
- **The run loop and journal:** `togi-session`, `togi-journal` and the journal schema.
- **The hardware path:** `togi-smu`, `togi-trial` and `togi-detect`.
- **Workloads, configuration defaults and evidence requirements:** workload content, intended durations, warm-up, coverage and placement requirements stay unchanged. A simulated gain cannot justify less hardware evidence.
- **The registration:** development and holdout seeds, scenarios, calendar, hazard budget, fallback slopes, thresholds and treatment of censored or nonconverged runs stay unchanged. Never tune on holdout or relax a threshold after seeing a score.

A simulator or machine-file change is an evaluation change, not a tuner experiment; record ideas needing frozen files separately. Refreshing evidence with `just facts` and `just fit` is a separately approved evaluation update, never part of a policy campaign. Use a copy of the state directory, never the live one.

## Hard rules

Breaking any rule invalidates an experiment, however good its metrics look.

- **Evidence only.** The tuner decides from journal evidence. It never reads simulator internals, fitted machines, facts extracts, seeds or scenario identities. Never special-case a core number, CCD or scenario's offsets; topology recorded in the journal can inform a tested hypothesis.
- **Traceable and deterministic.** Every new decision is a journal event with a cause and a plain-language `msg`, as the journal spec requires. Replaying the journal gives the same decisions.
- **Safe writes.** Preserve the [tuner invariants](../../docs/spec/tuner.md#invariants) and the [SMU write transaction](../../docs/spec/runtime.md#offset-types-and-the-write-transaction).
- **Failure detection stays intact.** Failures remain evidence under the spec's validity and qualification rules. Never ignore, retry away or reclassify a failure to improve a score.
- **Evidence volume.** Preserve the [workload matrix](../../docs/spec/workloads.md#coverage-matrix), [adequate-exposure boundary](../../docs/spec/tuner.md#adequate-exposure), and all required durations, repeats and placement rules. Reorder work only where the spec permits; do not shorten trials, omit required cells or count unobserved time as exposure.
- **Tests prove correctness.** Run the affected ordinary tests for each candidate. Simulations are [research tools, not correctness checks](../../docs/development.md#simulations-and-research); a passing day/night score cannot replace tests.
- **No ruleset bump per experiment.** One bump covers the kept decision changes when they become pull requests; file each as a sub-issue of the open `Ruleset N` issue (`docs/issues.md`).

## Method

The [historical commands](https://github.com/shgew/togi/blob/archive/pre-cut/tools/bench/program.md#method) require the archived evaluator, not the current checkout.

- Compare each candidate with the unchanged reference on the same registered development seeds and fallback slopes. Record every applicable metric and per-scenario verdict, not only an aggregate gain.
- Judge results under the [registered criteria](../../docs/benchmarking.md#daynight-gate). Keep censored and nonconverged runs in their denominators; comparator scores cannot pass the real policy's gate.
- Explain a gain by checking that the predicted quantity moved. Ablate kept changes periodically and drop any that no longer contribute.
- Record every experiment, kept or rejected, with its numbers, in the campaign's ledger.
- Reserve holdout for the final real policy's once-only confirmation at each registered fallback slope. An interrupted confirmation stays spent under the campaign rules; never retry it to select a better draw.
- Report the final policy's measured metrics, verdicts and limitations in each pull request. Discard a failed confirmation rather than changing the registration.

The simulator is a model, not the machine. The calendar gate idealizes zero trial setup/teardown and applies no controller-placement exclusion or required repeat; its scores omit that overhead, which must be measured on hardware. Prefer mechanisms grounded in evidence rules over constants fitted to the simulator, and scope claims to the registered scenarios. Bench evidence does not authorize a hardware deployment.
