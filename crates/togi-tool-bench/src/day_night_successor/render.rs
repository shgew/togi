#![expect(
    clippy::cast_precision_loss,
    reason = "report counts and hour totals are far below 2^52"
)]

//! Markdown rendering of a [`Score`]: verdict, safety and usefulness columns, every criterion's exact
//! fraction, risk and learning accounts, per-unit tables and the identities behind each event.
//!
//! A reduced development score leads with the exact warning
//! [`REDUCED_DEV_LABEL`](super::registration::REDUCED_DEV_LABEL) and never prints a gate `PASS`/`FAIL`
//! for a scenario or the experiment; a registered score prints them only when the experiment is complete.

use std::fmt::{Display, Write};

use togi_simrun::day_night::successor::{Classification, Period, Phase, RunProfile, UnitMode};

use super::registration::REDUCED_DEV_LABEL;
use super::score::learning::RestorationStatus;
use super::score::measure::SeedMetrics;
use super::score::{Column, FaultKind, Group, GroupStatus, Score, Verdict};

/// Renders the experiment summary: verdict, scenario table, criteria, accounts, distributions, faults and runtime.
pub fn render(score: &Score) -> String {
    let mut out = String::new();
    header(&mut out, score);
    scenarios(&mut out, score);
    for group in &score.groups {
        group_section(&mut out, score, group);
    }
    faults(&mut out, score);
    runtime(&mut out, score);
    out
}

/// Renders the summary and, for every complete seed, its identities, units, events, ledger and restorations.
pub fn render_seeds(score: &Score) -> String {
    let mut out = render(score);
    for group in &score.groups {
        for seed in &group.seeds {
            seed_section(&mut out, group, seed);
        }
    }
    out
}

fn put(out: &mut String, text: impl Display) {
    let _ = writeln!(out, "{text}");
}

fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

fn num(value: f64) -> String {
    if value.is_infinite() {
        "∞".into()
    } else {
        let text = format!("{value:.4}");
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

fn day(value: Option<f64>) -> String {
    value.map_or_else(|| "∞".into(), num)
}

fn mode(mode: UnitMode) -> &'static str {
    match mode {
        UnitMode::CcdUniform => "ccd-uniform",
        UnitMode::PerCore => "per-core",
    }
}

fn depths(values: &[u8]) -> String {
    values
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// The classification's recorded kebab-case name, so every variant prints as the record states it.
fn classification(value: Classification) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => name,
        _ => format!("{value:?}"),
    }
}

fn status_word(score: &Score, status: GroupStatus) -> &'static str {
    let gate = score.complete()
        && score.run_profile == RunProfile::Registered
        && score.policy_role == super::registration::PolicyRole::Candidate;
    match (status, gate) {
        (GroupStatus::Incomplete, _) => "INCOMPLETE",
        (GroupStatus::Met, true) => "PASS",
        (GroupStatus::NotMet, true) => "FAIL",
        (GroupStatus::Met, false) => "criteria met",
        (GroupStatus::NotMet, false) => "criteria not met",
    }
}

fn met(ok: bool) -> &'static str {
    if ok { "met" } else { "not met" }
}

fn header(out: &mut String, score: &Score) {
    if score.run_profile == RunProfile::ReducedDev || score.verdict == Verdict::ExploratoryReduced {
        put(out, format!("**{REDUCED_DEV_LABEL}**\n"));
    }
    put(out, "# Successor day/night score\n");
    put(out, format!("- Verdict: **{}**", score.verdict.label()));
    put(
        out,
        format!(
            "- Registration: `{}` (SHA-256 `{}`)",
            score.registration_id, score.registration_sha256
        ),
    );
    put(
        out,
        format!(
            "- Policy: `{}`, resolved identity `{}`, role {:?}",
            score.policy, score.policy_identity, score.policy_role
        ),
    );
    put(
        out,
        format!(
            "- Split: {:?}; run profile: {:?}",
            score.split, score.run_profile
        ),
    );
    put(
        out,
        format!(
            "- Slopes scored: {} of registered {}",
            score
                .selected_slopes
                .iter()
                .map(|slope| num(*slope))
                .collect::<Vec<_>>()
                .join(", "),
            score
                .registered_slopes
                .iter()
                .map(|slope| num(*slope))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
    put(
        out,
        format!(
            "- Complete cells: {}/{}",
            score.complete_cells, score.expected_cells
        ),
    );
    if score.verdict == Verdict::ExploratoryReduced {
        put(
            out,
            "- A reduced development read is never a registered score or a gate verdict; scenario rows state only whether criteria are met.",
        );
    }
    if !score.complete() {
        put(
            out,
            "- **The experiment is not complete.** Missing, duplicated, unexpected, invalid, incompatible, truncated and error cells preclude a pass; descriptive fractions below use the complete records as their denominator.",
        );
    }
    for issue in &score.issues {
        put(out, format!("- Issue: {}", cell(issue)));
    }
    put(out, "");
}

fn scenarios(out: &mut String, score: &Score) {
    put(out, "## Scenarios\n");
    put(
        out,
        "Safety and usefulness are separate columns; each row is one scenario, unit mode and slope, with no averaging across rows.\n",
    );
    put(
        out,
        "| Scenario | Mode | Slope | Complete seeds | Safety | Usefulness | Status |",
    );
    put(out, "|---|---|---|---|---|---|---|");
    for group in &score.groups {
        let complete = group.seeds.len() == group.expected_seeds.len();
        let (safety, usefulness) = if group.seeds.is_empty() {
            ("-", "-")
        } else {
            (met(group.safety_met), met(group.usefulness_met))
        };
        let suffix = if complete { "" } else { " (descriptive)" };
        put(
            out,
            format!(
                "| {} | {} | {} | {}/{} | {safety}{suffix} | {usefulness}{suffix} | {} |",
                cell(&group.scenario),
                mode(group.unit_mode),
                num(group.slope),
                group.seeds.len(),
                group.expected_seeds.len(),
                status_word(score, group.status)
            ),
        );
    }
    put(out, "");
}

fn group_section(out: &mut String, score: &Score, group: &Group) {
    put(
        out,
        format!(
            "## {} — {} — slope {}\n",
            cell(&group.scenario),
            mode(group.unit_mode),
            num(group.slope)
        ),
    );
    put(
        out,
        format!(
            "Status: **{}**. Complete seeds {}/{}.\n",
            status_word(score, group.status),
            group.seeds.len(),
            group.expected_seeds.len()
        ),
    );
    put(out, "| Criterion | Column | Rule | Measured | Result |");
    put(out, "|---|---|---|---|---|");
    for item in &group.criteria {
        let column = match item.column {
            Column::Safety => "safety",
            Column::Usefulness => "usefulness",
        };
        put(
            out,
            format!(
                "| {} | {column} | {} | {} | {} |",
                item.name,
                cell(&item.rule),
                cell(&item.measured),
                met(item.passed)
            ),
        );
    }
    put(out, "");
    let diagnostics = &group.diagnostics;
    put(out, "Outcomes and risk accounts (complete seeds):\n");
    put(
        out,
        format!(
            "- Outcomes: {} converged, {} unconverged crash-free (no true CO daily crash), {} unconverged with CO crashes.",
            diagnostics.outcomes.converged,
            diagnostics.outcomes.unconverged_crash_free,
            diagnostics.outcomes.unconverged_with_co_crashes
        ),
    );
    put(
        out,
        format!(
            "- Post-finish true CO daily crashes K = {} over T = {} active machine-hours; integrated expected CO-added failures H = {}.",
            diagnostics.k,
            num(diagnostics.t),
            num(diagnostics.h)
        ),
    );
    match (diagnostics.t > 0.0).then(|| diagnostics.k as f64 / diagnostics.t) {
        Some(rate) => put(
            out,
            format!(
                "- K/T = {} per hour; H/T = {} per hour.",
                num(rate * 1e6) + "e-6",
                num(diagnostics.h / diagnostics.t * 1e6) + "e-6"
            ),
        ),
        None => put(
            out,
            "- K/T and H/T undefined: no post-finish exposure (T = 0 fails the risk rule, it is not 0 per hour).",
        ),
    }
    put(
        out,
        format!(
            "- One-sided 95% Poisson upper mean for K: {}; upper rate: {} per hour. **Model-based diagnostic assuming a constant-rate Poisson process, not a guarantee for an adaptive policy.**",
            num(diagnostics.poisson_upper_mean),
            diagnostics
                .poisson_upper_rate
                .map_or_else(|| "undefined".into(), |rate| num(rate * 1e6) + "e-6")
        ),
    );
    put(
        out,
        format!(
            "- Seeds with zero true CO learning crashes (diagnostic, not a gate): {}.",
            diagnostics.zero_co_learning_crashes
        ),
    );
    put(
        out,
        format!(
            "- Seeds with zero total daily interruptions: {}; with zero true CO daily crashes: {} (separate counts).",
            diagnostics.zero_total_daily_interruptions, diagnostics.zero_daily_co_crashes
        ),
    );
    put(
        out,
        format!(
            "- Mean unit-total depth: {}; mean physical-core depth: {}.",
            diagnostics
                .mean_unit_total_depth
                .map_or_else(|| "n/a".into(), num),
            diagnostics
                .mean_core_depth
                .map_or_else(|| "n/a".into(), num)
        ),
    );
    put(
        out,
        format!(
            "- Median joint settled day over all registered seeds (∞ for unconverged or missing): {}.\n",
            day(diagnostics.median_settled_day)
        ),
    );
    if group.seeds.is_empty() {
        return;
    }
    distribution(out, group);
    units(out, group);
    sources(out, group);
    safety_table(out, group);
    learning_table(out, group);
}

fn distribution(out: &mut String, group: &Group) {
    let mut joint: Vec<f64> = group
        .seeds
        .iter()
        .map(SeedMetrics::settled_or_infinite)
        .collect();
    joint.sort_by(f64::total_cmp);
    put(
        out,
        format!(
            "Joint settled-day distribution, sorted: {}\n",
            joint
                .iter()
                .map(|value| num(*value))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
    let units = group.seeds.first().map_or(0, |seed| seed.units.len());
    for unit in 0..units {
        let mut finish: Vec<f64> = group
            .seeds
            .iter()
            .map(|seed| {
                seed.units
                    .get(unit)
                    .and_then(|item| item.finish)
                    .map_or(f64::INFINITY, |(_, day)| day)
            })
            .collect();
        let mut settled: Vec<f64> = group
            .seeds
            .iter()
            .map(|seed| {
                seed.units
                    .get(unit)
                    .and_then(|item| item.settled_at)
                    .unwrap_or(f64::INFINITY)
            })
            .collect();
        finish.sort_by(f64::total_cmp);
        settled.sort_by(f64::total_cmp);
        put(
            out,
            format!(
                "- Unit {unit} finish days, sorted: {}",
                finish
                    .iter()
                    .map(|value| num(*value))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
        put(
            out,
            format!(
                "- Unit {unit} settled days, sorted: {}",
                settled
                    .iter()
                    .map(|value| num(*value))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }
    put(out, "");
}

fn units(out: &mut String, group: &Group) {
    put(
        out,
        "Per-unit totals over complete seeds (unit totals never stand in for per-core depth):\n",
    );
    put(
        out,
        "| Unit | Finished | Mean finish depth | Mean final depth | Promotions | Retreat steps | Hold h | Qualification launches | Qualification h | Night h | Loss co | Loss background | Loss non-cpu | Loss unattributed | Loss history | CO attributions |",
    );
    put(
        out,
        "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|",
    );
    let count = group.seeds.first().map_or(0, |seed| seed.units.len());
    for unit in 0..count {
        let rows: Vec<_> = group
            .seeds
            .iter()
            .filter_map(|seed| seed.units.get(unit))
            .collect();
        let n = rows.len().max(1) as f64;
        let finished = rows.iter().filter(|row| row.finish.is_some()).count();
        let finish_depths: Vec<f64> = rows
            .iter()
            .filter_map(|row| row.finish_depth)
            .map(f64::from)
            .collect();
        let mean_finish = (!finish_depths.is_empty())
            .then(|| finish_depths.iter().sum::<f64>() / finish_depths.len() as f64);
        put(
            out,
            format!(
                "| {unit} | {finished}/{} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                rows.len(),
                mean_finish.map_or_else(|| "n/a".into(), num),
                num(rows
                    .iter()
                    .map(|row| f64::from(row.final_depth))
                    .sum::<f64>()
                    / n),
                rows.iter().map(|row| row.promotions).sum::<u64>(),
                rows.iter().map(|row| row.retreat_steps).sum::<u64>(),
                num(rows.iter().map(|row| row.hold_hours).sum()),
                rows.iter()
                    .map(|row| row.qualification_launches)
                    .sum::<u64>(),
                num(rows.iter().map(|row| row.qualification_hours).sum()),
                num(rows.iter().map(|row| row.night_hours).sum()),
                rows.iter().map(|row| row.losses.co).sum::<u64>(),
                rows.iter().map(|row| row.losses.background).sum::<u64>(),
                rows.iter().map(|row| row.losses.non_cpu).sum::<u64>(),
                rows.iter().map(|row| row.losses.unattributed).sum::<u64>(),
                rows.iter().map(|row| row.losses.history).sum::<u64>(),
                rows.iter().map(|row| row.co_attributions).sum::<u64>(),
            ),
        );
    }
    put(
        out,
        "\nCO attributions are not additive: one interaction crash implicating several units counts once in K and is attributed to each implicated unit here.\n",
    );
}

fn sources(out: &mut String, group: &Group) {
    put(
        out,
        "Per-source risk over complete seeds (the shared T is never summed per source):\n",
    );
    put(
        out,
        "| Source | Learning K | Learning H | Post-finish K | Post-finish H |",
    );
    put(out, "|---|---|---|---|---|");
    let mut rows: Vec<(String, u64, f64, u64, f64)> = Vec::new();
    for seed in &group.seeds {
        for row in &seed.risk.sources {
            match rows.iter_mut().find(|item| item.0 == row.source) {
                Some(item) => {
                    item.1 += row.learning_k;
                    item.2 += row.learning_h;
                    item.3 += row.post_finish_k;
                    item.4 += row.post_finish_h;
                }
                None => rows.push((
                    row.source.clone(),
                    row.learning_k,
                    row.learning_h,
                    row.post_finish_k,
                    row.post_finish_h,
                )),
            }
        }
    }
    for (source, learning_k, learning_h, post_k, post_h) in rows {
        put(
            out,
            format!(
                "| {} | {learning_k} | {} | {post_k} | {} |",
                cell(&source),
                num(learning_h),
                num(post_h)
            ),
        );
    }
    put(out, "");
}

fn safety_table(out: &mut String, group: &Group) {
    put(
        out,
        "Retained-profile safety per seed (first violation, maximum ratios and violating hours include later retreats and restorations; no grace interval):\n",
    );
    put(
        out,
        "| Seed | Final in A | First finish | Post-finish T h | Violating h | First violation | Max joint ratio | Max unit ratio | K | H | H/T ≤ λ |",
    );
    put(out, "|---|---|---|---|---|---|---|---|---|---|---|");
    for seed in &group.seeds {
        let post = &seed.post_finish;
        put(
            out,
            format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                seed.seed,
                seed.final_in_a,
                day(seed.first_finish_day),
                num(post.hours),
                num(post.violating_hours),
                post.first_violation.map_or_else(|| "none".into(), num),
                post.max_joint_ratio.map_or_else(|| "n/a".into(), num),
                post.max_unit_ratio.map_or_else(|| "n/a".into(), num),
                seed.risk.k,
                num(seed.risk.h),
                seed.risk.rate_ok
            ),
        );
    }
    put(out, "");
}

fn learning_table(out: &mut String, group: &Group) {
    put(
        out,
        "Learning contract per seed (limit 2 shared across phases; consumption counts every charged unique event):\n",
    );
    put(
        out,
        "| Seed | Limit | Consumed | Remaining | Overspend | Exhausted day | Charged learning | Charged post-finish | Restoration-tagged (overlap) | True CO learning crashes | Violations |",
    );
    put(out, "|---|---|---|---|---|---|---|---|---|---|---|");
    for seed in &group.seeds {
        let violations = if seed.learning.reported {
            seed.learning.violations.len().to_string()
        } else {
            "unreported".into()
        };
        let line = match &seed.ledger {
            Some(ledger) => format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {violations} |",
                seed.seed,
                ledger.limit,
                ledger.consumed,
                ledger.remaining,
                ledger.overspend,
                day(ledger.exhausted_day).replace('∞', "none"),
                ledger.charged_learning,
                ledger.charged_post_finish,
                ledger.charged_restoration_tagged,
                seed.risk.learning_k,
            ),
            None => format!(
                "| {} | unreported | unreported | unreported | unreported | unreported | unreported | unreported | unreported | {} | {violations} |",
                seed.seed, seed.risk.learning_k
            ),
        };
        put(out, line);
    }
    put(out, "");
}

fn seed_section(out: &mut String, group: &Group, seed: &SeedMetrics) {
    put(
        out,
        format!(
            "### {} — {} — slope {} — seed {}\n",
            cell(&group.scenario),
            mode(group.unit_mode),
            num(group.slope),
            seed.seed
        ),
    );
    put(
        out,
        format!(
            "Outcome {:?}; first finish {}; settled {}; unit-total depth {} of reference M = {} (physical maximum {}, required {}); physical-core mean depth {}; final depths [{}]{}.\n",
            seed.outcome,
            day(seed.first_finish_day),
            day(seed.settled_at),
            seed.depth.final_total,
            seed.depth.reference,
            seed.depth.physical_max,
            seed.depth.required_total,
            seed.depth.final_core_mean.map_or_else(|| "n/a".into(), num),
            depths(&seed.depth.final_depths),
            seed.depth
                .finish_depths
                .as_ref()
                .map_or_else(String::new, |finish| format!(
                    "; at first finish [{}]",
                    depths(finish)
                ))
        ),
    );
    put(
        out,
        format!(
            "Events (unique): daily {} total / {} CO / {} background / {} non-CPU / {} unattributed; night {} total / {} CO. Learning-phase daily CO {}; post-finish daily CO {}.\n",
            seed.events.day().total,
            seed.events.day().co,
            seed.events.day().background,
            seed.events.day().non_cpu,
            seed.events.day().unattributed,
            seed.events.night.total,
            seed.events.night.co,
            seed.events.day_learning.co,
            seed.events.day_post_finish.co
        ),
    );
    put(
        out,
        "| Unit | Finish | Finish day | Settled | Finish depth | Final depth | Promotions | Retreat steps | Learning H | Post-finish H |",
    );
    put(out, "|---|---|---|---|---|---|---|---|---|---|");
    for unit in &seed.units {
        put(
            out,
            format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                unit.unit.get(),
                unit.finish
                    .map_or_else(|| "none".into(), |(reason, _)| format!("{reason:?}")),
                day(unit.finish.map(|(_, day)| day)),
                day(unit.settled_at),
                unit.finish_depth
                    .map_or_else(|| "n/a".into(), |depth| depth.to_string()),
                unit.final_depth,
                unit.promotions,
                unit.retreat_steps,
                num(unit.learning_h),
                num(unit.post_finish_h)
            ),
        );
    }
    put(out, "");
    let evidence = &seed.policy_evidence;
    let stops = format!(
        "stopped units [{}]; newest step {}",
        evidence
            .stopped_units
            .iter()
            .map(|unit| unit.get().to_string())
            .collect::<Vec<_>>()
            .join(","),
        evidence.newest_step.as_deref().unwrap_or("none")
    );
    match &seed.ledger {
        Some(ledger) => put(
            out,
            format!(
                "Allowance: limit {}, consumed {}, remaining {}, overspend {}, exhausted {}{}; provisional {}, reclassified {}; {stops}.\n",
                ledger.limit,
                ledger.consumed,
                ledger.remaining,
                ledger.overspend,
                ledger.exhausted,
                ledger
                    .exhausted_day
                    .map_or_else(String::new, |day| format!(" at day {}", num(day))),
                ledger.provisional,
                ledger.reclassified,
            ),
        ),
        None => put(
            out,
            format!(
                "Allowance: unreported (the policy kept no allowance ledger; the learning contract is not judged); {stops}.\n"
            ),
        ),
    }
    for finish in &evidence.inherited_stops {
        put(
            out,
            format!(
                "Inherited coarse stop: unit {}, {:?}, day {}, witnesses [{}].",
                finish.unit.get(),
                finish.reason,
                num(finish.day),
                cell(&finish.witnesses.join(", "))
            ),
        );
    }
    if !evidence.inherited_stops.is_empty() {
        put(out, "");
    }
    if !seed.event_views.is_empty() {
        put(
            out,
            "| Event | Day | Period | Phase | Restoration tag | Classification | Provisional | Reclassified from | Charged | True cause | Implicated units | Newest step |",
        );
        put(out, "|---|---|---|---|---|---|---|---|---|---|---|---|");
        for event in &seed.event_views {
            put(
                out,
                format!(
                    "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                    cell(&event.id),
                    num(event.day),
                    if event.period == Period::Day {
                        "day"
                    } else {
                        "night"
                    },
                    if event.phase == Phase::Learning {
                        "learning"
                    } else {
                        "post-finish"
                    },
                    event.restoration_learning.as_deref().unwrap_or("-"),
                    classification(event.classification),
                    event.provisional,
                    event
                        .reclassified_from
                        .map_or_else(|| "-".into(), classification),
                    event.charged,
                    cell(&event.truth),
                    event
                        .implicated
                        .iter()
                        .map(|unit| unit.get().to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                    cell(event.newest_step.as_deref().unwrap_or("-"))
                ),
            );
        }
        put(out, "");
    }
    if !seed.risk.sources.is_empty() {
        put(
            out,
            "| Source | Learning K | Learning H | Post-finish K | Post-finish H |",
        );
        put(out, "|---|---|---|---|---|");
        for row in &seed.risk.sources {
            put(
                out,
                format!(
                    "| {} | {} | {} | {} | {} |",
                    cell(&row.source),
                    row.learning_k,
                    num(row.learning_h),
                    row.post_finish_k,
                    num(row.post_finish_h)
                ),
            );
        }
        put(
            out,
            format!(
                "\nPost-finish T = {} h (shared), K = {}, H = {}; restoration-tagged overlap {} h, H {}, K {} (a subset of the post-finish figures, not additional).\n",
                num(seed.risk.t),
                seed.risk.k,
                num(seed.risk.h),
                num(seed.risk.tagged.hours),
                num(seed.risk.tagged.h),
                seed.risk.tagged.k
            ),
        );
    }
    if !seed.learning.promotions.is_empty() {
        put(
            out,
            "| Promotion | Day | Unit | Before | After | Ceiling | Charged events before |",
        );
        put(out, "|---|---|---|---|---|---|---|");
        for promotion in &seed.learning.promotions {
            put(
                out,
                format!(
                    "| {} | {} | {} | {} | {} | {} | {} |",
                    cell(&promotion.id),
                    num(promotion.day),
                    promotion.unit.get(),
                    promotion.before,
                    promotion.after,
                    promotion.ceiling,
                    promotion.charged_before
                ),
            );
        }
        put(out, "");
    }
    if !seed.learning.restorations.is_empty() {
        put(
            out,
            "| Restoration | Epoch | Transition day | Stock reset day | Applied day | Clean stock h (recorded / recomputed) | Previous vector | Result | Stopped units | Qualification | Violations |",
        );
        put(out, "|---|---|---|---|---|---|---|---|---|---|---|");
        for item in &seed.learning.restorations {
            let result = match &item.status {
                RestorationStatus::Blocked { reason } => format!("blocked: {}", cell(reason)),
                RestorationStatus::Retained => "retained".into(),
                RestorationStatus::Crashed { event } => format!(
                    "crashed ({}): stock step-back and permanent stop",
                    cell(event)
                ),
            };
            put(
                out,
                format!(
                    "| {} | {} | {} | {} | {} | {} / {} | [{}] | {result} | [{}] | {} | {} |",
                    cell(&item.id),
                    cell(&item.epoch),
                    num(item.transition_day),
                    num(item.stock_reset_day),
                    item.applied_day.map_or_else(|| "none".into(), num),
                    item.clean_stock_hours.map_or_else(|| "-".into(), num),
                    item.stock_hours_recomputed.map_or_else(|| "-".into(), num),
                    depths(&item.previous_depths),
                    item.stopped_units
                        .iter()
                        .map(|unit| unit.get().to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                    item.qualification.as_deref().unwrap_or("-"),
                    item.violations
                ),
            );
        }
        put(out, "");
    }
    put(
        out,
        "| Epoch | Context | Days | Operational M | Physical max | Per-unit feasible | Certificate | Hours in A / total | In A at end | Recovered to A at |",
    );
    put(out, "|---|---|---|---|---|---|---|---|---|---|");
    for epoch in &seed.epochs {
        put(
            out,
            format!(
                "| {} | {} | {}–{} | {} | {} | [{}] | {} | {} / {} | {} | {} |",
                cell(&epoch.id),
                cell(&epoch.context),
                num(epoch.start_day),
                num(epoch.end_day),
                epoch.operational_max,
                epoch.physical_max,
                depths(&epoch.per_unit_max_feasible),
                cell(&epoch.certificate),
                num(epoch.member_hours),
                num(epoch.hours),
                epoch.in_a_at_end,
                epoch.recovered_to_a_at.map_or_else(|| "never".into(), num)
            ),
        );
    }
    put(out, "");
    for violation in &seed.learning.violations {
        put(
            out,
            format!(
                "- Violation `{}` ({}): {}",
                violation.rule,
                cell(&violation.subject),
                cell(&violation.detail)
            ),
        );
    }
    put(out, "");
}

fn faults(out: &mut String, score: &Score) {
    if score.faults.is_empty() {
        return;
    }
    put(out, "## Incomplete cells\n");
    put(
        out,
        "Partial-run counts are lower bounds and never zeros; none of these cells counts toward a fraction's denominator.\n",
    );
    put(out, "| Scenario | Mode | Slope | Seed | Fault | Detail |");
    put(out, "|---|---|---|---|---|---|");
    for fault in &score.faults {
        let detail = match &fault.kind {
            FaultKind::Missing | FaultKind::Unexpected => String::new(),
            FaultKind::Duplicate { copies } => format!("{copies} records"),
            FaultKind::Invalid(error) => error.clone(),
            FaultKind::Incompatible(reasons) => reasons.join("; "),
            FaultKind::Inconsistent(error) => error.clone(),
            FaultKind::Truncated {
                observed_until_day,
                reason,
                lower_bound,
            } => format!(
                "observed until day {}: {reason}; at least {} daily interruptions, {} daily CO crashes over {} active hours",
                num(*observed_until_day),
                lower_bound.daily_interruptions,
                lower_bound.daily_co_crashes,
                num(lower_bound.active_hours)
            ),
            FaultKind::HarnessError {
                observed_until_day,
                error,
            } => format!("observed until day {}: {error}", num(*observed_until_day)),
        };
        put(
            out,
            format!(
                "| {} | {} | {} | {} | {} | {} |",
                cell(&fault.scenario),
                mode(fault.unit_mode),
                num(fault.slope),
                fault.seed,
                fault.kind.label(),
                cell(&detail)
            ),
        );
    }
    put(out, "");
}

fn runtime(out: &mut String, score: &Score) {
    let runtime = &score.runtime;
    put(out, "## Runtime\n");
    put(
        out,
        format!(
            "- Total invocation wall {} s (excluding compilation).",
            runtime
                .invocation_wall_seconds
                .map_or_else(|| "not recorded".into(), num)
        ),
    );
    for invocation in &runtime.invocations {
        put(
            out,
            format!(
                "  - {}: {} s.",
                cell(&invocation.id),
                num(invocation.elapsed_seconds)
            ),
        );
    }
    put(
        out,
        format!(
            "- Records {} ({} complete); summed run wall {} s; maximum run wall {} s; compilation {} s (excluded from the budget); trial launches {}.",
            runtime.records,
            runtime.complete,
            num(runtime.summed_run_wall_seconds),
            num(runtime.max_run_wall_seconds),
            num(runtime.compilation_seconds),
            runtime.trial_launches
        ),
    );
    put(
        out,
        format!(
            "- Jobs {:?}; resource class {:?}; registered budget {} s per policy/slope invocation. Summed run wall is not elapsed time and no completion time is forecast.",
            runtime.jobs, runtime.resource_classes, runtime.budget_seconds
        ),
    );
    put(
        out,
        format!(
            "- Exposure seconds: intended {}, observed {}, eligible {}, onset-active {}.",
            num(runtime.intended_seconds),
            num(runtime.observed_seconds),
            num(runtime.eligible_seconds),
            num(runtime.onset_active_seconds)
        ),
    );
}

#[cfg(test)]
#[path = "render_review_tests.rs"]
mod review_tests;
