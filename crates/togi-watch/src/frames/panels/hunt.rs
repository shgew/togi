//! A hunt: its cause, its parts or groups, the member probes and what follows it.

use std::time::Duration;

use ratatui::text::Line;
use togi_journal::HuntStage;
use togi_machine::{CoreId, Signal};
use togi_tuner::{GroupState, HuntGroup, MemberProbe, RerunPlan};

use super::super::colours::Paint;
use super::super::fit::{Clause, Form, display_width, fit_clauses, rule};
use super::super::layout::SizeClass;
use super::super::measure::Tables;
use super::super::notes::offset_list;
use super::super::scene::{Scene, between, clock, short_duration};
use super::super::story::{crash_evidence, known_words};
use super::super::terms::{cycle_place, recut_words, trial_name};
use super::table::{cell, field_row, overlay, stage_marker, table_row};
use crate::model::HuntView;
use crate::words::{age, core_ids, group_word, kind_words, plural, signal_words};

fn seconds(n: u32) -> String {
    short_duration(Duration::from_secs(u64::from(n)))
}

/// A hunt in `width` cells: its rule and cause, its parts, groups and probes, and what comes after.
pub(super) fn hunt_lines(
    scene: &Scene<'_>,
    hunt: &HuntView<'_>,
    t: &Tables,
    width: usize,
    class: SizeClass,
) -> Vec<Line<'static>> {
    let compact = class == SizeClass::Compact;
    let right = hunt.facts.started.map_or_else(Line::default, |started| {
        let since = age(between(started, scene.now));
        let minute = scene.wall_minute(started);
        if compact {
            cell(scene, Paint::Grey, format!("{minute} · {since}"))
        } else {
            cell(
                scene,
                Paint::Grey,
                format!("started {minute} · {since} so far"),
            )
        }
    });
    let room = width.saturating_sub(t.field);
    let mut out = vec![
        rule(
            width,
            &cell(scene, Paint::Grey, format!("HUNT {}", hunt.plan.number)),
            &right,
            scene.style(Paint::Track),
        ),
        Line::default(),
        field_row(
            scene,
            t,
            "cause",
            cause_line(scene, hunt, compact, room),
            width,
        ),
    ];
    if !compact {
        if let Some(line) = evidence_line(scene, hunt, room) {
            out.push(field_row(scene, t, "evidence", line, width));
        }
        let explain = |text: &'static str| Form::new(cell(scene, Paint::Text, text), 1);
        let candidates = fit_clauses(
            room,
            &[
                Clause::whole(
                    "",
                    cell(scene, Paint::Text, core_ids(hunt.plan.candidates.iter())),
                ),
                Clause::new(
                    ", ",
                    vec![
                        explain("deeper at failure than at their parked offsets"),
                        explain("deeper at failure than parked"),
                        Form::empty(),
                    ],
                ),
            ],
        );
        out.push(field_row(scene, t, "candidates", candidates, width));
    }
    if !hunt.plan.probes.is_empty() {
        out.push(Line::default());
        out.extend(group_lines(scene, hunt, t, width));
        out.push(Line::default());
        out.extend(probe_lines(scene, hunt, t, class));
    } else if !hunt.plan.parts.is_empty() {
        if !compact {
            out.push(Line::default());
        }
        out.extend(part_lines(scene, hunt, t, width, class));
    } else if !hunt.plan.groups.is_empty() {
        out.push(Line::default());
        out.extend(group_lines(scene, hunt, t, width));
    }
    out.push(Line::default());
    out.extend(then_lines(scene, hunt, t, width, class));
    #[cfg(test)]
    crate::corpus::lines("panel.hunt", &format!("{class:?}"), &out);
    out
}

/// What failed and where, with the attribution kept whole: the subject and the cycle place shorten first, so
/// `no core named` or the named core is never what a narrow panel cuts.
fn cause_line(scene: &Scene<'_>, hunt: &HuntView<'_>, compact: bool, room: usize) -> Line<'static> {
    let cause = &hunt.facts.cause;
    let say = |text: String| cell(scene, Paint::Text, text);
    if cause.known {
        return fit_clauses(room, &[Clause::whole("", say(known_words(cause)))]);
    }
    let Some(trial) = &cause.trial else {
        let text = format!(
            "{} while idle, with no trial running",
            cause.signal.map_or("crash", signal_words)
        );
        return fit_clauses(room, &[Clause::whole("", say(text))]);
    };
    let signal = cause.signal.map_or("failure", signal_words);
    let rerun = if cause.rerun_of { "the rerun of " } else { "" };
    let subject = Clause::new(
        "",
        vec![
            Form::new(say(format!("{signal} in {rerun}{}", trial_name(trial))), 3),
            Form::new(
                say(format!(
                    "{signal} in {rerun}{} {}",
                    trial.regime,
                    kind_words(trial.regime)
                )),
                3,
            ),
            Form::new(
                say(if cause.rerun_of {
                    format!("{signal} in the rerun")
                } else {
                    signal.to_owned()
                }),
                3,
            ),
        ],
    );
    let mut place = vec![];
    if !compact {
        let full = cycle_place(trial);
        if !full.is_empty() {
            let mut bare = trial.clone();
            bare.count.of = 0;
            let bare = cycle_place(&bare);
            place.push(Form::new(say(full.clone()), 5));
            if bare != full {
                place.push(Form::new(say(bare), 5));
            }
            place.push(Form::empty());
        }
    }
    let named = cause.core.map_or_else(
        || "no core named".to_owned(),
        |core| format!("core {:02} named", core.get()),
    );
    let mut clauses = vec![subject];
    if !place.is_empty() {
        clauses.push(Clause::new(" · ", place));
    }
    clauses.push(Clause::whole(
        if compact { ", " } else { " · " },
        say(named),
    ));
    fit_clauses(room, &clauses)
}

/// What the journal holds about a crash that falls short of naming a core, then that it does not attribute the
/// failure; the uncertainty is kept whole and the sample's wording shortens first.
fn evidence_line(scene: &Scene<'_>, hunt: &HuntView<'_>, room: usize) -> Option<Line<'static>> {
    let end = hunt.facts.cause.end.as_ref();
    let whole = crash_evidence(end);
    if whole.is_empty() {
        return None;
    }
    let say = |text: String| Form::new(cell(scene, Paint::Grey, text), 3);
    let mut forms = vec![say(whole)];
    if let Some(end) = end
        && let Some(sample) = end.last_sample
    {
        let at = clock(sample);
        match end.stalled {
            Some(core) => {
                let core = core.get();
                forms.push(say(format!(
                    "core {core:02} stalled by the last sample, {at} in"
                )));
                forms.push(say(format!("core {core:02} stalled, {at} in")));
            }
            None => forms.push(say(format!("last sample {at} in"))),
        }
    }
    Some(fit_clauses(
        room,
        &[
            Clause::new("", forms),
            Clause::whole(" · ", cell(scene, Paint::Grey, "not enough to attribute")),
        ],
    ))
}

fn part_lines(
    scene: &Scene<'_>,
    hunt: &HuntView<'_>,
    t: &Tables,
    width: usize,
    class: SizeClass,
) -> Vec<Line<'static>> {
    let compact = class == SizeClass::Compact;
    let plan = hunt.plan;
    let mut out = Vec::new();
    let parked = if hunt.facts.parked_zero && t.parts[3].w >= "parked at 0".len() {
        "parked at 0"
    } else {
        "parked"
    };
    let failing = if t.parts[2].w < "at failing offsets".len() {
        "failing"
    } else {
        "at failing offsets"
    };
    if !compact {
        out.push(overlay(
            scene.paint(Paint::Grey, "parts"),
            table_row(
                &t.parts,
                &[
                    Line::default(),
                    cell(scene, Paint::Grey, "part"),
                    cell(scene, Paint::Grey, failing),
                    cell(scene, Paint::Grey, parked),
                    cell(scene, Paint::Grey, "schedule"),
                    cell(scene, Paint::Grey, "state"),
                ],
            ),
        ));
    }
    let trial = scene.s.trial.filter(|trial| trial.count.of > 0);
    for (i, part) in plan.parts.iter().enumerate() {
        let mut marker = Line::default();
        let (mut style, mut faint) = (Paint::Text, Paint::Grey);
        let mut state = cell(
            scene,
            Paint::Grey,
            part.outcome
                .map_or("", |outcome| outcome_words(outcome, None)),
        );
        if part.running && scene.trial_runs() {
            marker = Line::from(stage_marker(scene, "►"));
            (style, faint) = (Paint::White, Paint::Text);
            if let Some(trial) = trial {
                let (index, of) = (trial.count.index, trial.count.of);
                state = cell(
                    scene,
                    Paint::Running,
                    if compact {
                        format!("trial {index}/{of}")
                    } else {
                        format!("trial {index} of {of}")
                    },
                );
            }
        } else if part.group.is_none() && i > 0 {
            state = cell(scene, Paint::Grey, format!("if part {i} passes"));
        } else if part.group.is_none() {
            state = cell(scene, Paint::Grey, "next");
        } else if part.outcome == Some(GroupState::Passed) {
            state = cell(scene, Paint::Good, group_word(GroupState::Passed));
        } else if part.outcome == Some(GroupState::Failed) {
            state = cell(scene, Paint::Bad, group_word(GroupState::Failed));
        }
        let mut name = format!("{} of {}", i + 1, plan.parts.len());
        let mut cores = core_ids(part.failing.iter());
        if compact {
            name = format!("part {}/{}", i + 1, plan.parts.len());
            cores.push_str(" at failing offsets");
        }
        out.push(table_row(
            &t.parts,
            &[
                marker,
                cell(scene, style, name),
                cell(scene, style, cores),
                cell(scene, Paint::Text, core_ids(part.parked.iter())),
                cell(
                    scene,
                    faint,
                    format!("{} x {}", part.trials, seconds(part.duration_s)),
                ),
                state,
            ],
        ));
    }
    if !compact && plan.parts.len() > 1 {
        out.push(field_row(
            scene,
            t,
            "",
            cell(
                scene,
                Paint::Grey,
                "failed parts are split again; complements test what remains",
            ),
            width,
        ));
    }
    out
}

/// A stretch of consecutive groups that ended the same way.
struct Run<'a> {
    first: &'a HuntGroup,
    last: &'a HuntGroup,
    cores: Vec<CoreId>,
    stages: Vec<HuntStage>,
}

fn runs<'a>(groups: &'a [HuntGroup]) -> Vec<Run<'a>> {
    let mut runs: Vec<Run<'a>> = Vec::new();
    for group in groups.iter().filter(|group| group.probe.is_none()) {
        if let Some(run) = runs.last_mut()
            && group.stage != HuntStage::Locate
            && run.last.stage != HuntStage::Locate
            && run.last.outcome == group.outcome
            && run.last.carried == group.carried
            && !matches!(group.outcome, GroupState::Running | GroupState::Failed)
        {
            run.last = group;
            for core in group.cores {
                if !run.cores.contains(&core) {
                    run.cores.push(core);
                }
            }
            if !run.stages.contains(&group.stage) {
                run.stages.push(group.stage);
            }
            continue;
        }
        runs.push(Run {
            first: group,
            last: group,
            cores: group.cores.iter().collect(),
            stages: vec![group.stage],
        });
    }
    runs
}

fn group_lines(
    scene: &Scene<'_>,
    hunt: &HuntView<'_>,
    t: &Tables,
    width: usize,
) -> Vec<Line<'static>> {
    let plan = hunt.plan;
    let mut out = Vec::new();
    for (i, run) in runs(&plan.groups).iter().enumerate() {
        let label = if i == 0 { "parts" } else { "" };
        let mut which = format!("group {}", run.first.number);
        let mut what = format!("{} at failing offsets", core_ids(run.first.cores.iter()));
        let rest: Vec<CoreId> = plan
            .candidates
            .iter()
            .filter(|core| !run.first.cores.contains(*core))
            .collect();
        if !rest.is_empty() {
            let with_rest = format!("{what}, {} parked", core_ids(rest.iter().copied()));
            if display_width(&with_rest) <= t.groups[1].w {
                what = with_rest;
            }
        }
        if run.first.stage == HuntStage::Locate {
            what = format!("locate: {} at 0", core_ids(plan.candidates.iter()));
        }
        let many = run.first.number != run.last.number;
        if many {
            which = format!("groups {}-{}", run.first.number, run.last.number);
            what = format!("parts of {}", core_ids(run.cores.iter().copied()));
            if run.stages.contains(&HuntStage::Complement) {
                what.push_str(" and their complements");
            }
        }
        let mut outcome =
            outcome_words(run.last.outcome, hunt.group_signal(run.last.number)).to_owned();
        let paint = match run.last.outcome {
            GroupState::Passed => {
                if many {
                    "all passed".clone_into(&mut outcome);
                }
                Paint::Good
            }
            GroupState::Failed => Paint::Bad,
            GroupState::Running => {
                outcome = format!("running · {}/{} passed", run.last.passed, run.last.needed);
                Paint::Running
            }
            GroupState::Skipped => Paint::Text,
        };
        let mut result = vec![scene.paint(paint, outcome)];
        if run.last.carried {
            result.push(scene.paint(Paint::Grey, " · answered by existing evidence"));
        }
        out.push(overlay(
            scene.paint(Paint::Grey, label),
            table_row(
                &t.groups,
                &[
                    cell(scene, Paint::Text, which),
                    cell(scene, Paint::Grey, what),
                    Line::from(result),
                ],
            ),
        ));
    }
    if !plan.probes.is_empty() {
        let members = plan.probes.iter().map(|probe| probe.member);
        out.push(field_row(
            scene,
            t,
            "",
            cell(
                scene,
                Paint::White,
                format!("→ {} kept together; probing members", core_ids(members)),
            ),
            width,
        ));
    }
    out
}

/// How a group's outcome reads; a failed group says it crashed when its trial did.
fn outcome_words(outcome: GroupState, signal: Option<Signal>) -> &'static str {
    match outcome {
        GroupState::Failed if signal == Some(Signal::Crash) => "crashed",
        GroupState::Passed | GroupState::Failed | GroupState::Running | GroupState::Skipped => {
            group_word(outcome)
        }
    }
}

fn probe_lines(
    scene: &Scene<'_>,
    hunt: &HuntView<'_>,
    t: &Tables,
    class: SizeClass,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    if class != SizeClass::Compact {
        out.push(overlay(
            scene.paint(Paint::Grey, "probes"),
            table_row(
                &t.probes,
                &[
                    cell(scene, Paint::Grey, "member"),
                    cell(scene, Paint::Grey, "now"),
                    cell(scene, Paint::Grey, "group failed at"),
                    cell(scene, Paint::Grey, "passed at"),
                    cell(scene, Paint::Grey, "state"),
                ],
            ),
        ));
    }
    let trial = scene.s.trial.filter(|trial| trial.count.of > 0);
    let mut next = true;
    let mut waiting: Vec<CoreId> = Vec::new();
    let flush = |waiting: &mut Vec<CoreId>, out: &mut Vec<Line<'static>>| {
        if !waiting.is_empty() {
            out.push(table_row(
                &t.probes,
                &[
                    cell(scene, Paint::Text, core_ids(waiting.iter().copied())),
                    Line::default(),
                    Line::default(),
                    Line::default(),
                    cell(scene, Paint::Grey, "waiting"),
                ],
            ));
            waiting.clear();
        }
    };
    for probe in &hunt.plan.probes {
        let probed =
            probe.running || probe.done || !probe.passed_at.is_empty() || probe.failed_at.len() > 1;
        if !probed && !next {
            waiting.push(probe.member);
            continue;
        }
        flush(&mut waiting, &mut out);
        let mut style = Paint::Text;
        let mut state = cell(scene, Paint::Grey, "next");
        if probe.done {
            state = cell(scene, Paint::Good, "done");
        } else if probe.running {
            style = Paint::White;
            state = if !scene.trial_runs() {
                Line::default()
            } else if let Some(trial) = trial {
                cell(
                    scene,
                    Paint::Running,
                    format!("► trial {} of {}", trial.count.index, trial.count.of),
                )
            } else {
                cell(scene, Paint::Running, "► running")
            };
            next = false;
        } else {
            next = false;
        }
        let (mut failed, mut passed) = (Line::default(), Line::default());
        if probed {
            if let Some(later) = probe.failed_at.get(1..).filter(|later| !later.is_empty()) {
                failed = cell(scene, Paint::Bad, offset_list(later));
            }
            passed = cell(scene, Paint::Good, passed_words(hunt, probe));
        }
        out.push(table_row(
            &t.probes,
            &[
                cell(scene, style, format!("{:02}", probe.member.get())),
                cell(scene, style, probe.offset.get().to_string()),
                failed,
                passed,
                state,
            ],
        ));
    }
    flush(&mut waiting, &mut out);
    out
}

/// The offsets at which the group passed with this member there, each with how it passed.
fn passed_words(hunt: &HuntView<'_>, probe: &MemberProbe) -> String {
    probe
        .passed_at
        .iter()
        .map(|&offset| {
            if probe.carried.contains(&offset) {
                return format!("{} carried", offset.get());
            }
            hunt.plan
                .groups
                .iter()
                .rfind(|group| {
                    group
                        .probe
                        .is_some_and(|at| at.core == probe.member && at.offset == offset)
                })
                .map_or_else(
                    || offset.get().to_string(),
                    |group| format!("{} {}/{}", offset.get(), group.passed, group.needed),
                )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn then_lines(
    scene: &Scene<'_>,
    hunt: &HuntView<'_>,
    t: &Tables,
    width: usize,
    class: SizeClass,
) -> Vec<Line<'static>> {
    let plan = hunt.plan;
    let compact = class == SizeClass::Compact;
    let mut steps: Vec<(String, Option<String>)> = if compact {
        vec![
            ("record a failure point or a combination".to_owned(), None),
            ("back off past it".to_owned(), None),
        ]
    } else if plan.probes.is_empty() {
        vec![
            (
                "record one core's failure point or a combination".to_owned(),
                None,
            ),
            ("back off past it".to_owned(), None),
        ]
    } else {
        let members = core_ids(plan.probes.iter().map(|probe| probe.member));
        vec![
            (
                format!("record the combination: {members} at the offsets that still fail"),
                None,
            ),
            (
                "back off one member to a tested passing probe that keeps the profile clear"
                    .to_owned(),
                None,
            ),
        ]
    };
    let rerun = &plan.rerun;
    if rerun.short > 0 || rerun.long > 0 {
        let (place, length) = rerun_words(rerun, class);
        steps.push((format!("rerun {place}{length}"), Some(place)));
    }
    if let Some(cycle) = scene.s.cycle
        && cycle.plan.current < cycle.plan.steps.len()
        && !compact
    {
        let number = cycle.plan.number.map_or(0, std::num::NonZeroU32::get);
        steps.push((
            format!("if the rerun passes, resume cycle {number}"),
            Some(format!("cycle {number}")),
        ));
    }
    let lines: Vec<Line<'static>> = steps
        .iter()
        .map(|(text, emphasis)| step_line(scene, text, emphasis.as_deref()))
        .collect();
    let mut out = Vec::new();
    if compact {
        out.push(overlay(
            scene.paint(Paint::Grey, "then"),
            table_row(&t.steps, &[cell(scene, Paint::Grey, "1"), lines[0].clone()]),
        ));
    } else {
        out.push(field_row(
            scene,
            t,
            "then",
            cell(
                scene,
                Paint::Grey,
                "after the hunt:",
            ),
            width,
        ));
        out.push(table_row(
            &t.steps,
            &[cell(scene, Paint::Grey, "1"), lines[0].clone()],
        ));
    }
    for (i, line) in lines.into_iter().enumerate().skip(1) {
        out.push(table_row(
            &t.steps,
            &[cell(scene, Paint::Grey, (i + 1).to_string()), line],
        ));
    }
    out
}

/// A step in text, with the words `emphasis` in white where it names the rerun's load or the cycle it resumes.
fn step_line(scene: &Scene<'_>, text: &str, emphasis: Option<&str>) -> Line<'static> {
    let Some(emphasis) = emphasis else {
        return cell(scene, Paint::Text, text.to_owned());
    };
    match text.split_once(emphasis) {
        Some((before, after)) => Line::from(vec![
            scene.paint(Paint::Text, before.to_owned()),
            scene.paint(Paint::White, emphasis.to_owned()),
            scene.paint(Paint::Text, after.to_owned()),
        ]),
        None => cell(scene, Paint::Text, text.to_owned()),
    }
}

/// Where the rerun after a hunt loads, and how long it runs.
fn rerun_words(rerun: &RerunPlan, class: SizeClass) -> (String, String) {
    let place = format!("{} {}", rerun.regime, kind_words(rerun.regime));
    if class != SizeClass::Wide {
        let mut length = format!(": {} x {}", rerun.short, seconds(rerun.short_s));
        if rerun.long > 0 {
            length = format!("{length}, then {} x {}", rerun.long, seconds(rerun.long_s));
        } else if class == SizeClass::Medium {
            length.push_str(", original length");
        }
        let place = if class == SizeClass::Compact {
            place
        } else {
            format!("{place} on {}", core_ids(rerun.cores.iter()))
        };
        return (place, length);
    }
    let mut length = format!(
        ": {} of {}",
        plural(rerun.short as usize, "trial"),
        seconds(rerun.short_s)
    );
    if rerun.long > 0 {
        length = format!("{length}, then {} of {}", rerun.long, seconds(rerun.long_s));
    } else {
        length.push_str(", its original length");
    }
    (
        format!("{place} on {}", core_ids(rerun.cores.iter())),
        length,
    )
}

/// Where the hunt is: its parts, its member probes, or starting before its first group.
pub(super) fn stage(hunt: &HuntView<'_>, short: bool) -> String {
    let plan = hunt.plan;
    if !plan.probes.is_empty() {
        return probe_stage(hunt, short);
    }
    let Some(last) = plan.groups.last() else {
        return "starting".to_owned();
    };
    if hunt.locating() {
        return "locate".to_owned();
    }
    if plan.parts.is_empty() {
        return format!("group {}", last.number);
    }
    let parts = &plan.parts;
    let at = parts
        .iter()
        .position(|part| part.running)
        .or_else(|| {
            parts
                .iter()
                .position(|part| part.group.is_some() && part.outcome == Some(GroupState::Running))
        })
        .or_else(|| parts.iter().position(|part| part.group.is_none()));
    let Some(at) = at else {
        return format!("group {}", last.number);
    };
    let recut = recut_words(plan.cut, false);
    let (at, n) = (at + 1, parts.len());
    if short {
        format!("{recut}part {at} of {n}")
    } else {
        format!("parts · {recut}part {at} of {n}")
    }
}

fn probe_stage(hunt: &HuntView<'_>, short: bool) -> String {
    let probes = &hunt.plan.probes;
    let at = probes
        .iter()
        .position(|probe| !probe.done)
        .map_or(probes.len(), |index| index + 1);
    if short {
        format!("member {at} of {}", probes.len())
    } else {
        format!("member probes · member {at} of {}", probes.len())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use jiff::tz::TimeZone;
    use togi_journal::{DeadEndCondition, HuntStage, ShutdownReason};
    use togi_machine::{Regime, Signal};
    use togi_tuner::GroupState;

    use super::super::super::fit::{CUT_MARKER, cut_words, plain};
    use super::super::super::layout::SizeClass;
    use super::super::super::measure::Tables;
    use super::super::fixtures::{
        at, cycle_part, cycle_plan, cycle_step, group, hunt_facts, hunt_plan, member, part, probe,
        scene, texts, trial_view,
    };
    use super::{
        cause_line, group_lines, hunt_lines, outcome_words, part_lines, probe_lines, then_lines,
    };
    use crate::model::{
        CyclePlace, CycleView, DeadEndView, HuntFacts, HuntView, Model, SessionView, StopView,
        TrialEndView,
    };
    use crate::tests::kit::{off, set};

    fn stage_of(plan: &togi_tuner::HuntPlan, short: bool) -> String {
        let facts = hunt_facts();
        let model = Model::new();
        let mut s = model.snapshot();
        s.hunt = Some(HuntView {
            plan,
            facts: &facts,
        });
        let zone = TimeZone::UTC;
        let scene = scene(&s, &zone);
        super::super::hunt_stage(&scene, short)
    }

    #[test]
    fn the_stage_stays_on_an_unfinished_part_between_trials() {
        let mut plan = hunt_plan(
            vec![group(4, &[0], GroupState::Running)],
            vec![
                part(&[0], &[1], Some(4), Some(GroupState::Running), false),
                part(&[1], &[0], None, None, false),
            ],
        );
        assert_eq!(stage_of(&plan, true), "part 1 of 2");
        plan.groups.push(group(5, &[1], GroupState::Running));
        plan.parts[0].outcome = Some(GroupState::Passed);
        plan.parts[1] = part(&[1], &[0], Some(5), Some(GroupState::Running), true);
        assert_eq!(stage_of(&plan, true), "part 2 of 2");
        plan.parts[1].outcome = Some(GroupState::Passed);
        plan.parts[1].running = false;
        assert_eq!(stage_of(&plan, true), "group 5");
        assert_eq!(stage_of(&plan, false), "group 5");
    }

    #[test]
    fn the_stage_says_when_parts_were_recut() {
        let mut plan = hunt_plan(
            vec![group(3, &[0], GroupState::Running)],
            vec![
                part(&[0], &[1], Some(3), Some(GroupState::Running), true),
                part(&[1], &[0], None, None, false),
            ],
        );
        for (cut, short, long) in [
            (1, "part 1 of 2", "parts · part 1 of 2"),
            (
                2,
                "re-cut 1 · part 1 of 2",
                "parts · re-cut 1 · part 1 of 2",
            ),
            (
                3,
                "re-cut 2 · part 1 of 2",
                "parts · re-cut 2 · part 1 of 2",
            ),
        ] {
            plan.cut = cut;
            assert_eq!(stage_of(&plan, true), short, "cut {cut} short");
            assert_eq!(stage_of(&plan, false), long, "cut {cut}");
        }
    }

    #[test]
    fn the_stage_before_parts_names_the_hunt_s_progress() {
        let mut plan = hunt_plan(Vec::new(), Vec::new());
        assert_eq!(stage_of(&plan, true), "starting");
        plan.groups.push(group(2, &[0], GroupState::Passed));
        assert_eq!(stage_of(&plan, true), "group 2");
        let mut locating = group(3, &[0], GroupState::Running);
        locating.stage = HuntStage::Locate;
        plan.groups.push(locating);
        assert_eq!(stage_of(&plan, true), "locate");
        plan.groups.pop();
        plan.probes = vec![probe(0, -10, false, true), probe(1, -10, true, false)];
        assert_eq!(stage_of(&plan, true), "member 2 of 2");
        assert_eq!(stage_of(&plan, false), "member probes · member 2 of 2");
        plan.probes[1].done = true;
        assert_eq!(stage_of(&plan, true), "member 2 of 2");
    }

    #[test]
    fn no_hunt_says_nothing_of_a_stage() {
        let model = Model::new();
        let zone = TimeZone::UTC;
        let scene = scene(&model.snapshot(), &zone);
        assert_eq!(super::super::hunt_stage(&scene, false), "");
    }

    fn with_hunt<R>(
        plan: &togi_tuner::HuntPlan,
        facts: &crate::model::HuntFacts,
        draw: impl FnOnce(&crate::frames::scene::Scene<'_>, &HuntView<'_>) -> R,
    ) -> R {
        let model = Model::new();
        let mut s = model.snapshot();
        let hunt = HuntView { plan, facts };
        s.hunt = Some(hunt);
        let zone = TimeZone::UTC;
        let scene = scene(&s, &zone);
        draw(&scene, &hunt)
    }

    #[test]
    fn a_probe_without_a_failing_offset_renders() {
        let mut plan = hunt_plan(Vec::new(), Vec::new());
        plan.probes = vec![probe(0, -9, true, false)];
        let facts = hunt_facts();
        let rows = with_hunt(&plan, &facts, |scene, hunt| {
            texts(&probe_lines(
                scene,
                hunt,
                &Tables::new(SizeClass::Medium, 80),
                SizeClass::Medium,
            ))
        });
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(rows[0].starts_with("probes"));
        assert!(rows[1].contains("00") && rows[1].contains("-9") && rows[1].contains("► running"));
    }

    #[test]
    fn probes_show_how_each_member_stands() {
        let mut plan = hunt_plan(Vec::new(), Vec::new());
        let mut done = probe(0, -15, false, true);
        done.failed_at = vec![off(-20), off(-18)];
        done.passed_at = vec![off(-15), off(-12)];
        done.carried = vec![off(-12)];
        plan.probes = vec![
            done,
            probe(1, -10, true, false),
            probe(2, -10, false, false),
            probe(3, -10, false, false),
        ];
        let mut passed = group(7, &[0, 1, 2, 3], GroupState::Passed);
        passed.probe = Some(member(0, -15));
        passed.passed = 5;
        plan.groups.push(passed);
        let facts = hunt_facts();
        let model = Model::new();
        let trial = trial_view(2, 5);
        let mut s = model.snapshot();
        s.trial = Some(&trial);
        s.hunt = Some(HuntView {
            plan: &plan,
            facts: &facts,
        });
        let zone = TimeZone::UTC;
        let scene = scene(&s, &zone);
        let hunt = scene.s.hunt.expect("a hunt");
        let rows = texts(&probe_lines(
            &scene,
            &hunt,
            &Tables::new(SizeClass::Wide, 100),
            SizeClass::Wide,
        ));
        assert_eq!(rows.len(), 4, "{rows:?}");
        assert!(rows[1].contains("-18") && rows[1].contains("-15 5/5, -12 carried"));
        assert!(rows[1].ends_with("done"));
        assert!(rows[2].contains("► trial 2 of 5"));
        assert!(rows[3].contains("02 03") && rows[3].ends_with("waiting"));
    }

    #[test]
    fn a_probe_shows_nothing_running_once_the_session_is_over() {
        let mut plan = hunt_plan(Vec::new(), Vec::new());
        plan.probes = vec![probe(0, -10, true, false), probe(1, -10, false, false)];
        let facts = hunt_facts();
        let model = Model::new();
        let trial = trial_view(2, 5);
        let session = SessionView {
            start: at(0),
            activity: None,
        };
        let stopped = StopView {
            at: at(1),
            reason: ShutdownReason::Signal,
            saved: false,
        };
        let dead_end = DeadEndView {
            at: at(1),
            condition: DeadEndCondition::NoEvidence,
            detail: String::new(),
        };
        let rows = |stop: Option<&StopView>, end: Option<&DeadEndView>| {
            let mut s = model.snapshot();
            s.session = Some(&session);
            s.trial = Some(&trial);
            s.stopped = stop;
            s.dead_end = end;
            s.hunt = Some(HuntView {
                plan: &plan,
                facts: &facts,
            });
            let zone = TimeZone::UTC;
            let scene = scene(&s, &zone);
            let hunt = scene.s.hunt.expect("a hunt");
            texts(&probe_lines(
                &scene,
                &hunt,
                &Tables::new(SizeClass::Wide, 100),
                SizeClass::Wide,
            ))
        };
        assert!(rows(None, None)[1].contains("► trial 2 of 5"));
        for over in [rows(Some(&stopped), None), rows(None, Some(&dead_end))] {
            assert!(over[1].contains("00"), "{over:?}");
            for row in &over {
                assert!(
                    !row.contains('►') && !row.contains("trial 2 of 5"),
                    "{over:?}"
                );
            }
        }
    }

    #[test]
    fn the_hunt_s_parts_say_what_runs_and_what_waits() {
        let plan = hunt_plan(
            Vec::new(),
            vec![
                part(&[0, 1], &[2], Some(4), Some(GroupState::Running), true),
                part(&[2], &[0, 1], None, None, false),
            ],
        );
        let mut facts = hunt_facts();
        facts.parked_zero = true;
        let model = Model::new();
        let trial = trial_view(2, 5);
        let mut s = model.snapshot();
        s.trial = Some(&trial);
        let hunt = HuntView {
            plan: &plan,
            facts: &facts,
        };
        s.hunt = Some(hunt);
        let zone = TimeZone::UTC;
        let scene = scene(&s, &zone);
        let wide = texts(&part_lines(
            &scene,
            &hunt,
            &Tables::new(SizeClass::Wide, 100),
            100,
            SizeClass::Wide,
        ));
        assert!(wide[0].starts_with("parts") && wide[0].contains("parked at 0"));
        assert!(
            wide[1].contains("1 of 2")
                && wide[1].contains("5 x 10m")
                && wide[1].contains("trial 2 of 5")
        );
        assert!(wide[2].contains("2 of 2") && wide[2].contains("if part 1 passes"));
        assert!(wide[3].contains("failed parts are split again; complements test what remains"));
        assert!(!wide[3].contains("until one fails"));
        let compact = texts(&part_lines(
            &scene,
            &hunt,
            &Tables::new(SizeClass::Compact, 80),
            80,
            SizeClass::Compact,
        ));
        assert_eq!(compact.len(), 2, "{compact:?}");
        assert!(compact[0].contains("part 1/2") && compact[0].contains("trial 2/5"));
    }

    #[test]
    fn parts_that_have_not_run_or_have_concluded_say_so() {
        let facts = hunt_facts();
        let state_rows = |plan: &togi_tuner::HuntPlan| {
            with_hunt(plan, &facts, |scene, hunt| {
                texts(&part_lines(
                    scene,
                    hunt,
                    &Tables::new(SizeClass::Wide, 100),
                    100,
                    SizeClass::Wide,
                ))
            })
        };
        let waiting = hunt_plan(
            Vec::new(),
            vec![
                part(&[0], &[1], None, None, false),
                part(&[1], &[0], None, None, false),
            ],
        );
        let rows = state_rows(&waiting);
        assert!(
            rows[1].contains("1 of 2") && rows[1].ends_with("next"),
            "{rows:?}"
        );
        assert!(rows[2].ends_with("if part 1 passes"), "{rows:?}");
        let concluded = hunt_plan(
            vec![
                group(1, &[0], GroupState::Passed),
                group(2, &[1], GroupState::Failed),
            ],
            vec![
                part(&[0], &[1], Some(1), Some(GroupState::Passed), false),
                part(&[1], &[0], Some(2), Some(GroupState::Failed), false),
            ],
        );
        let rows = state_rows(&concluded);
        assert!(
            rows[1].contains("1 of 2") && rows[1].ends_with("passed"),
            "{rows:?}"
        );
        assert!(
            rows[2].contains("2 of 2") && rows[2].ends_with("failed"),
            "{rows:?}"
        );
        let painted = with_hunt(&concluded, &facts, |scene, hunt| {
            let colourful = crate::frames::scene::Scene {
                colours: crate::screen::Colours::Full,
                ..*scene
            };
            part_lines(
                &colourful,
                hunt,
                &Tables::new(SizeClass::Wide, 100),
                100,
                SizeClass::Wide,
            )
        });
        let state_style = |row: usize, word: &str| {
            painted[row]
                .spans
                .iter()
                .find(|span| span.content.trim() == word)
                .unwrap_or_else(|| panic!("row {row} has no {word} span: {:?}", painted[row]))
                .style
        };
        let full = crate::screen::Colours::Full;
        assert_eq!(
            state_style(1, "passed"),
            crate::frames::Paint::Good.style(full)
        );
        assert_eq!(
            state_style(2, "failed"),
            crate::frames::Paint::Bad.style(full)
        );
    }

    #[test]
    fn the_probe_list_between_trials_names_the_next_member_and_folds_the_rest() {
        let mut plan = hunt_plan(Vec::new(), Vec::new());
        plan.probes = vec![
            probe(0, -15, false, true),
            probe(1, -10, false, false),
            probe(2, -10, false, false),
            probe(3, -10, false, false),
        ];
        let facts = hunt_facts();
        let rows = with_hunt(&plan, &facts, |scene, hunt| {
            texts(&probe_lines(
                scene,
                hunt,
                &Tables::new(SizeClass::Wide, 100),
                SizeClass::Wide,
            ))
        });
        assert_eq!(rows.len(), 4, "{rows:?}");
        assert!(
            rows[1].trim_start().starts_with("00") && rows[1].ends_with("done"),
            "{rows:?}"
        );
        assert!(
            rows[2].trim_start().starts_with("01") && rows[2].ends_with("next"),
            "{rows:?}"
        );
        assert!(
            rows[3].contains("02 03") && rows[3].ends_with("waiting"),
            "{rows:?}"
        );
    }

    #[test]
    fn a_hunt_with_groups_and_no_parts_draws_its_locate_row() {
        let mut locating = group(1, &[0, 1, 2], GroupState::Running);
        locating.stage = HuntStage::Locate;
        let plan = hunt_plan(vec![locating], Vec::new());
        let facts = hunt_facts();
        let rows = with_hunt(&plan, &facts, |scene, hunt| {
            texts(&hunt_lines(
                scene,
                hunt,
                &Tables::new(SizeClass::Wide, 120),
                120,
                SizeClass::Wide,
            ))
        });
        let at = rows
            .iter()
            .position(|row| row.starts_with("parts"))
            .unwrap_or_else(|| panic!("no parts row in {rows:?}"));
        assert!(rows[at - 1].trim().is_empty(), "{rows:?}");
        assert!(rows[at].contains("locate: 00-02 at 0"), "{rows:?}");
        assert!(rows[at].ends_with("running · 0/5 passed"), "{rows:?}");
    }

    #[test]
    fn groups_that_ended_alike_read_as_one_row() {
        let mut plan = hunt_plan(
            vec![
                group(1, &[0], GroupState::Passed),
                group(2, &[1], GroupState::Passed),
                group(3, &[2], GroupState::Passed),
                group(4, &[1], GroupState::Failed),
                group(5, &[0, 1], GroupState::Running),
            ],
            Vec::new(),
        );
        plan.groups[2].stage = HuntStage::Complement;
        plan.groups[4].passed = 2;
        let mut facts = hunt_facts();
        facts.group_signals = vec![(4, Signal::Crash)];
        let rows = with_hunt(&plan, &facts, |scene, hunt| {
            texts(&group_lines(
                scene,
                hunt,
                &Tables::new(SizeClass::Wide, 140),
                140,
            ))
        });
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert!(rows[0].starts_with("parts"));
        assert!(
            rows[0].contains("groups 1-3")
                && rows[0].contains("parts of 00-02 and their complements")
        );
        assert!(rows[0].ends_with("all passed"));
        assert!(rows[1].contains("group 4") && rows[1].ends_with("crashed"));
        assert!(rows[2].contains("group 5") && rows[2].ends_with("running · 2/5 passed"));
    }

    #[test]
    fn answers_by_existing_evidence_are_said_so() {
        let mut carried = group(1, &[0], GroupState::Passed);
        carried.carried = true;
        let plan = hunt_plan(vec![carried], Vec::new());
        let facts = hunt_facts();
        let rows = with_hunt(&plan, &facts, |scene, hunt| {
            texts(&group_lines(
                scene,
                hunt,
                &Tables::new(SizeClass::Wide, 140),
                140,
            ))
        });
        assert!(
            rows[0].ends_with("passed · answered by existing evidence"),
            "{rows:?}"
        );
    }

    #[test]
    fn what_a_failed_group_did_is_worded_by_its_signal() {
        assert_eq!(
            outcome_words(GroupState::Failed, Some(Signal::Crash)),
            "crashed"
        );
        assert_eq!(
            outcome_words(GroupState::Failed, Some(Signal::Stall)),
            "failed"
        );
        assert_eq!(outcome_words(GroupState::Passed, None), "passed");
        assert_eq!(outcome_words(GroupState::Skipped, None), "skipped");
    }

    #[test]
    fn the_cause_names_what_failed_and_where() {
        let plan = hunt_plan(Vec::new(), Vec::new());
        let mut facts = hunt_facts();
        let words = |facts: &crate::model::HuntFacts, compact| {
            with_hunt(&plan, facts, |scene, hunt| {
                plain(&cause_line(scene, hunt, compact, usize::MAX))
            })
        };
        assert_eq!(
            words(&facts, false),
            "crash while idle, with no trial running"
        );
        facts.cause.signal = Some(Signal::CorrectedMce);
        assert_eq!(
            words(&facts, false),
            "corrected hardware error while idle, with no trial running"
        );
        facts.cause.signal = Some(Signal::Crash);
        assert_eq!(
            words(&facts, true),
            "crash while idle, with no trial running"
        );
        facts.cause.trial = Some(trial_view(1, 5));
        facts.cause.signal = Some(Signal::ComputationError);
        facts.cause.rerun_of = true;
        assert_eq!(
            words(&facts, false),
            "wrong result in the rerun of R7 all-core on 00 01 · no core named"
        );
        assert_eq!(
            words(&facts, true),
            "wrong result in the rerun of R7 all-core on 00 01, no core named"
        );
        facts.cause.core = Some(crate::tests::kit::core(1));
        assert_eq!(
            words(&facts, true),
            "wrong result in the rerun of R7 all-core on 00 01, core 01 named"
        );
        assert_eq!(
            words(&facts, false),
            "wrong result in the rerun of R7 all-core on 00 01 · core 01 named"
        );
        facts.cause.trial = None;
        facts.cause.known = true;
        facts.cause.regime = Some(Regime::R7);
        assert_eq!(
            words(&facts, false),
            "R7 all-core already failed at these offsets"
        );
        assert_eq!(words(&facts, false), super::known_words(&facts.cause));
    }

    fn failure_facts(signal: Signal, named: Option<u8>, cores: &[u8]) -> HuntFacts {
        let mut trial = trial_view(2, 5);
        trial.cores = set(cores);
        trial.cycle = Some(CyclePlace {
            cycle: 1,
            step: Some(4),
            part: Some(11),
            parts: 12,
        });
        let mut facts = hunt_facts();
        facts.cause.signal = Some(signal);
        facts.cause.rerun_of = true;
        facts.cause.core = named.map(crate::tests::kit::core);
        if signal == Signal::Crash {
            facts.cause.end = Some(TrialEndView {
                id: "t1".to_owned().into(),
                at: at(1500),
                regime: Some(Regime::R7),
                cores: trial.cores,
                outcome: togi_journal::Outcome::Failure,
                signal: Some(signal),
                core: None,
                duration: Duration::from_secs(1),
                planned: Some(Duration::from_secs(600)),
                last_sample: Some(Duration::from_secs(1)),
                stalled: None,
                tctl_max_c: None,
                voltage_v: None,
            });
        }
        facts.cause.trial = Some(trial);
        facts
    }

    fn medium_rows(facts: &HuntFacts, width: usize) -> Vec<String> {
        let plan = hunt_plan(
            Vec::new(),
            vec![
                part(&[0, 1], &[2], Some(4), Some(GroupState::Running), true),
                part(&[2], &[0, 1], None, None, false),
            ],
        );
        with_hunt(&plan, facts, |scene, hunt| {
            texts(&hunt_lines(
                scene,
                hunt,
                &Tables::new(SizeClass::Medium, width),
                width,
                SizeClass::Medium,
            ))
        })
    }

    fn row_of<'a>(rows: &'a [String], label: &str) -> &'a str {
        rows.iter()
            .find(|row| row.starts_with(label))
            .unwrap_or_else(|| panic!("no {label} row in {rows:?}"))
    }

    /// The 160x45 hunt panel: 75 cells, of which the label column takes 12.
    const MEDIUM_PANEL: usize = 75;

    #[test]
    fn the_legacy_cut_hid_what_the_medium_rows_now_keep() {
        let room = MEDIUM_PANEL - 12;
        let legacy_cause = "wrong result in the rerun of R7 all-core on 00 01 · cycle 1 step 4, part 11, trial 2 of 5 · no core named";
        let legacy_evidence = "the last sample came 0:01 into the trial · not enough to attribute";
        assert!(!cut_words(legacy_cause, room).contains("no core named"));
        assert!(!cut_words(legacy_evidence, room).contains("attribute"));
        let crash = medium_rows(&failure_facts(Signal::Crash, None, &[0, 1]), MEDIUM_PANEL);
        let cause = row_of(&crash, "cause");
        assert!(
            cause.ends_with("no core named") && !cause.contains(CUT_MARKER),
            "{cause}"
        );
        assert!(cause.contains("crash in the rerun of R7 all-core on 00 01"));
        let evidence = row_of(&crash, "evidence");
        assert!(
            evidence.ends_with("not enough to attribute") && !evidence.contains(CUT_MARKER),
            "{evidence}"
        );
        assert!(evidence.contains("0:01"), "{evidence}");
    }

    #[test]
    fn a_named_wrong_result_keeps_its_core_at_medium() {
        let facts = failure_facts(Signal::ComputationError, Some(4), &[0, 1]);
        let rows = medium_rows(&facts, MEDIUM_PANEL);
        let cause = row_of(&rows, "cause");
        assert!(
            cause.starts_with("cause       wrong result in the rerun of R7 all-core")
                && cause.ends_with("core 04 named")
                && !cause.contains(CUT_MARKER),
            "{cause}"
        );
        assert!(!rows.iter().any(|row| row.starts_with("evidence")));
    }

    #[test]
    fn a_long_subject_gives_way_before_the_attribution_and_the_uncertainty() {
        let cores = [0, 2, 4, 6, 8, 10, 12, 14];
        for width in [MEDIUM_PANEL, 60] {
            let unnamed = medium_rows(&failure_facts(Signal::Crash, None, &cores), width);
            let cause = row_of(&unnamed, "cause");
            assert!(
                cause.ends_with("no core named") && !cause.contains(CUT_MARKER),
                "{width}: {cause}"
            );
            let evidence = row_of(&unnamed, "evidence");
            assert!(
                evidence.ends_with("not enough to attribute") && !evidence.contains(CUT_MARKER),
                "{width}: {evidence}"
            );
            let named = medium_rows(
                &failure_facts(Signal::ComputationError, Some(8), &cores),
                width,
            );
            assert!(
                row_of(&named, "cause").ends_with("core 08 named"),
                "{width}: {named:?}"
            );
        }
    }

    #[test]
    fn a_stalled_worker_is_named_in_the_medium_evidence() {
        let mut facts = failure_facts(Signal::Crash, None, &[0, 1]);
        let end = facts.cause.end.as_mut().expect("an end");
        end.last_sample = Some(Duration::from_secs(125));
        end.stalled = Some(crate::tests::kit::core(4));
        let rows = medium_rows(&facts, MEDIUM_PANEL);
        let evidence = row_of(&rows, "evidence");
        assert!(
            evidence.contains("core 04 stalled")
                && evidence.contains("2:05")
                && evidence.ends_with("not enough to attribute"),
            "{evidence}"
        );
    }

    #[test]
    fn the_medium_hunt_prose_fits_without_a_cut() {
        let rows = medium_rows(&failure_facts(Signal::Crash, None, &[0, 1]), MEDIUM_PANEL);
        for row in &rows {
            assert!(!row.contains(CUT_MARKER), "{row}\n{}", rows.join("\n"));
        }
        let all = rows.join("\n");
        for kept in [
            "then finer parts and complements until one fails or none does",
            "record one core's failure point or a combination",
            "original length",
        ] {
            assert!(all.contains(kept), "{kept}:\n{all}");
        }
        let (_, expected) = include_str!("snapshots/hunt-medium-attribution.snap")
            .strip_prefix("---\n")
            .and_then(|snapshot| snapshot.split_once("\n---\n"))
            .expect("a committed snapshot has an insta metadata header");
        assert_eq!(all, expected.trim_end_matches('\n'));
    }

    #[test]
    fn the_compact_hunt_names_its_cause_parts_and_what_follows() {
        let plan = hunt_plan(
            Vec::new(),
            vec![
                part(&[0, 1], &[2], Some(4), Some(GroupState::Running), true),
                part(&[2], &[0, 1], None, None, false),
            ],
        );
        let facts = hunt_facts();
        let rows = with_hunt(&plan, &facts, |scene, hunt| {
            texts(&hunt_lines(
                scene,
                hunt,
                &Tables::new(SizeClass::Compact, 80),
                80,
                SizeClass::Compact,
            ))
        });
        assert!(
            rows[0].starts_with("HUNT 1") && rows[0].ends_with("00:16 · 16m40s"),
            "{rows:?}"
        );
        assert!(rows[2].starts_with("cause") && rows[2].contains("crash while idle"));
        assert!(rows.iter().any(|row| row.contains("part 1/2")));
        assert!(
            rows.iter()
                .any(|row| row.starts_with("then") && row.contains("record a failure point"))
        );
        assert!(
            rows.iter()
                .any(|row| row.contains("rerun R7 all-core: 5 x 10m"))
        );
    }

    #[test]
    fn what_follows_a_hunt_names_the_rerun_and_the_cycle_it_resumes() {
        let plan = hunt_plan(Vec::new(), Vec::new());
        let facts = hunt_facts();
        let cycle = cycle_plan(
            vec![cycle_step(
                Regime::R1,
                vec![cycle_part(&[0], None, true, true, false)],
                false,
                true,
            )],
            0,
            false,
        );
        let model = Model::new();
        let mut s = model.snapshot();
        let hunt = HuntView {
            plan: &plan,
            facts: &facts,
        };
        s.hunt = Some(hunt);
        s.cycle = Some(CycleView {
            plan: &cycle,
            step_hunts: &[],
            resume: None,
        });
        let zone = TimeZone::UTC;
        let scene = scene(&s, &zone);
        let rows = texts(&then_lines(
            &scene,
            &hunt,
            &Tables::new(SizeClass::Wide, 100),
            100,
            SizeClass::Wide,
        ));
        assert_eq!(rows.len(), 5, "{rows:?}");
        assert!(
            rows[0].starts_with("then")
                && rows[0].contains("after the hunt:")
                && !rows[0].contains("if the hunt resolves")
        );
        assert!(rows[1].contains('1') && rows[1].contains("record one core's failure point"));
        assert!(
            rows[3].contains("rerun R7 all-core on 00-02: 5 trials of 10m, its original length")
        );
        assert!(rows[4].contains("if the rerun passes, resume cycle 2"));
        let compact = texts(&then_lines(
            &scene,
            &hunt,
            &Tables::new(SizeClass::Compact, 80),
            80,
            SizeClass::Compact,
        ));
        assert_eq!(compact.len(), 3, "{compact:?}");
    }
}
