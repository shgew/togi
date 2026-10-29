//! The context panel: the night's task, progress and shares of its elapsed time, and the day's applied profile, hold, step and
//! events, as the tuner's projection reports them. Nothing here decides what the tuner would do.

use std::time::Duration;

use ratatui::text::{Line, Span};
use togi_journal::{Classification, DayEventId, DayStep, HoldReason, Queue, Resolution, StepKind};
use togi_tuner::policy::{
    DayProjection, EventProjection, FirmwareValidity, NightProjection, Projection,
    QualificationProgress, ReproductionProjection,
};
use togi_tuner::unit::UnitProfile;

use super::super::colours::Paint;
use super::super::draw::bounded_rows;
use super::super::fit::rule;
use super::super::layout::SizeClass;
use super::super::measure::Measure;
use super::super::scene::{Scene, hm};
use super::table::{cell, field_row};
use crate::day_totals::unknown_intervals;

/// The panel's rows, at most `measure.context.height` of them: the open night first while one runs, then the
/// day; the finished night comes after the day.
pub(super) fn context(scene: &Scene<'_>, measure: &Measure) -> Vec<Line<'static>> {
    let width = usize::from(measure.context.width);
    let height = usize::from(measure.context.height);
    let policy = scene.s.policy;
    let night = policy
        .night
        .as_ref()
        .map(|night| (night, night_lines(scene, night, policy, width)));
    let day = day_lines(scene, policy, width);
    let open = night
        .as_ref()
        .is_some_and(|(night, _)| night.closed_by.is_none());
    let mut sections = Vec::new();
    match night {
        Some((_, lines)) if open => {
            sections.push(lines);
            sections.push(day);
        }
        Some((_, lines)) => {
            sections.push(day);
            sections.push(lines);
        }
        None => sections.push(day),
    }
    let blank = measure.layout.class != SizeClass::Compact;
    let mut out = Vec::new();
    for section in sections {
        if blank && !out.is_empty() {
            out.push(Line::default());
        }
        out.extend(section);
    }
    bounded_rows(out, height, scene.colours)
}

/// Seconds as `12m` or `3h05m`.
fn secs(seconds: u64) -> String {
    hm(Duration::from_secs(seconds))
}

fn profile_text(profile: &UnitProfile) -> String {
    profile
        .iter()
        .map(|(_, offset)| offset.get().to_string())
        .collect::<Vec<_>>()
        .join("/")
}

fn value(scene: &Scene<'_>, paint: Paint, text: String) -> Line<'static> {
    cell(scene, paint, text)
}

fn night_lines(
    scene: &Scene<'_>,
    night: &NightProjection,
    policy: &Projection,
    width: usize,
) -> Vec<Line<'static>> {
    let since = scene.wall_minute(night.started);
    let title = match night.closed_by {
        None => Line::from(vec![
            scene.paint(Paint::White, format!("NIGHT since {since}")),
            scene.paint(Paint::Grey, format!(" · {} elapsed", secs(elapsed(night)))),
        ]),
        Some(closed_by) => {
            let end = scene
                .s
                .log
                .iter()
                .find(|entry| entry.seq == closed_by)
                .map_or_else(String::new, |entry| {
                    format!("-{}", scene.wall_minute(entry.at))
                });
            cell(scene, Paint::White, format!("LAST NIGHT {since}{end}"))
        }
    };
    let mut out = vec![rule(
        width,
        &title,
        &Line::default(),
        scene.style(Paint::Track),
        scene.colours,
    )];
    out.push(field_row(scene, "task", task_value(scene, night), width));
    let mut label = "mapping";
    for step in &night.mapping {
        let text = match (step.cursor, step.bracket) {
            (_, Some((failing, bracket))) => format!(
                "CCD {}: failed at {}, bracket at {}",
                step.unit,
                failing.get(),
                bracket.get()
            ),
            (Some(cursor), None) => format!("CCD {} at {}", step.unit, cursor.get()),
            (None, None) => continue,
        };
        out.push(field_row(
            scene,
            label,
            value(scene, Paint::Text, text),
            width,
        ));
        label = "";
    }
    if let Some(progress) = &night.qualification {
        out.push(field_row(
            scene,
            "qualifying",
            qualification_value(scene, progress, policy),
            width,
        ));
    }
    let mut label = "reproducing";
    for reproduction in &night.reproductions {
        out.push(field_row(
            scene,
            label,
            reproduction_value(scene, reproduction),
            width,
        ));
        label = "";
    }
    if let Some(shares) = shares_value(scene, night) {
        out.push(field_row(scene, "shares", shares, width));
    }
    if !night.unavailable.is_empty() {
        let backends = night
            .unavailable
            .iter()
            .map(|unavailable| unavailable.backend.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        out.push(field_row(
            scene,
            "unavailable",
            value(scene, Paint::Warn, backends),
            width,
        ));
    }
    out
}

/// The night's elapsed observed time E, in seconds: what its queues consumed, together.
fn elapsed(night: &NightProjection) -> u64 {
    night.budgets.iter().map(|budget| budget.consumed_s).sum()
}

/// Each queue's part of E against its soft share: `mapping 8%/10%`, the filler as `filler 3%`; a queue with no
/// share that consumed nothing is left out. `None` when every queue is left out.
fn shares_value(scene: &Scene<'_>, night: &NightProjection) -> Option<Line<'static>> {
    let total = elapsed(night);
    let mut spans = Vec::new();
    for budget in &night.budgets {
        if budget.share_pct == 0 && budget.consumed_s == 0 {
            continue;
        }
        let used = budget
            .consumed_s
            .saturating_mul(100)
            .checked_div(total)
            .unwrap_or(0);
        let text = match budget.queue {
            Queue::Filler => format!("{} {used}%", budget.queue.as_str()),
            Queue::Reproduction
            | Queue::Qualification
            | Queue::Mapping
            | Queue::Control
            | Queue::Requalification => {
                format!("{} {used}%/{}%", budget.queue.as_str(), budget.share_pct)
            }
        };
        if !spans.is_empty() {
            spans.push(scene.paint(Paint::Grey, " · "));
        }
        spans.push(scene.paint(Paint::Text, text));
    }
    (!spans.is_empty()).then(|| Line::from(spans))
}

fn task_value(scene: &Scene<'_>, night: &NightProjection) -> Line<'static> {
    let done = night
        .tasks
        .iter()
        .filter(|planned| planned.status == togi_journal::TaskStatus::Complete)
        .count();
    let progress = format!("{done} of {} planned tasks done", night.tasks.len());
    match &night.current {
        Some((task, _)) => Line::from(vec![
            scene.paint(
                Paint::Running,
                format!("{} task {}", task.queue.as_str(), task.ordinal),
            ),
            scene.paint(Paint::Grey, format!(" · {progress}")),
        ]),
        None => Line::from(vec![
            scene.paint(Paint::Grey, "none running · ".to_owned()),
            scene.paint(Paint::Grey, progress),
        ]),
    }
}

fn qualification_value(
    scene: &Scene<'_>,
    progress: &QualificationProgress,
    policy: &Projection,
) -> Line<'static> {
    let purpose = policy
        .qualifications
        .iter()
        .rev()
        .find(|record| record.profile == progress.profile)
        .map(|record| record.purpose.as_str().replace('_', " "));
    let purpose = purpose.map_or_else(String::new, |purpose| format!(" · {purpose}"));
    let text = format!(
        "{}{purpose} · {} eligible exposure · {} outstanding",
        profile_text(&progress.profile),
        secs(progress.eligible_s),
        progress.outstanding.len()
    );
    value(scene, Paint::Text, text)
}

fn event_name(event: &DayEventId) -> String {
    match event {
        DayEventId::Boot { boot } => {
            let short: String = boot.as_str().chars().take(8).collect();
            format!("boot {short}")
        }
        DayEventId::Record { seq, .. } => format!("record #{seq}"),
        DayEventId::Requalification { night } => format!("requalification {night}"),
    }
}

fn reproduction_value(scene: &Scene<'_>, reproduction: &ReproductionProjection) -> Line<'static> {
    let text = format!(
        "{} · {} · {} used · {} classes left",
        event_name(&reproduction.source),
        reproduction.state.as_str(),
        secs(reproduction.consumed_s),
        reproduction.remaining.len()
    );
    value(scene, Paint::Text, text)
}

fn day_lines(scene: &Scene<'_>, policy: &Projection, width: usize) -> Vec<Line<'static>> {
    let applied = policy.applied.as_ref().map_or_else(
        || "applied CO 0".to_owned(),
        |profile| format!("applied {}", profile_text(profile)),
    );
    let day = &policy.day;
    let mut out = vec![rule(
        width,
        &cell(scene, Paint::White, "DAY".to_owned()),
        &cell(scene, Paint::Grey, applied),
        scene.style(Paint::Track),
        scene.colours,
    )];
    out.push(field_row(
        scene,
        "Ruleset 13",
        value(
            scene,
            Paint::Grey,
            "mapping-only cap; CO 0 is not hazard-goal proof".to_owned(),
        ),
        width,
    ));
    out.push(field_row(scene, "hold", hold_value(scene, day), width));
    if let Some((_, step)) = &day.last_step {
        out.push(field_row(
            scene,
            "last step",
            step_value(scene, step),
            width,
        ));
    }
    if day.freeze {
        out.push(field_row(
            scene,
            "freeze",
            value(
                scene,
                Paint::Warn,
                "on: D kept, no promotion; retreats and firmware resets still apply".to_owned(),
            ),
            width,
        ));
    }
    out.push(field_row(
        scene,
        "firmware",
        firmware_value(scene, &day.firmware),
        width,
    ));
    if !day.pending_questions.is_empty() {
        out.push(field_row(
            scene,
            "questions",
            value(
                scene,
                Paint::Warn,
                format!("{} pending", day.pending_questions.len()),
            ),
            width,
        ));
    }
    let totals = scene.s.day_totals;
    if !totals.is_empty() {
        let unknown = unknown_intervals(totals.unknown_intervals());
        out.push(field_row(
            scene,
            "all days",
            value(
                scene,
                Paint::Text,
                format!("{} · {unknown}", totals.all().describe()),
            ),
            width,
        ));
        out.push(field_row(
            scene,
            "stock CO 0",
            value(
                scene,
                Paint::Text,
                format!("{} · observations, not a bound", totals.stock().describe()),
            ),
            width,
        ));
    }
    let mut label = "events";
    for event in &policy.events {
        out.push(field_row(scene, label, event_value(scene, event), width));
        label = "";
    }
    let mut label = "vetoes";
    for veto in &policy.vetoes {
        out.push(field_row(
            scene,
            label,
            value(
                scene,
                Paint::Text,
                format!("{} · {}", event_name(&veto.event), veto.state.as_str()),
            ),
            width,
        ));
        label = "";
    }
    out
}

fn hold_value(scene: &Scene<'_>, day: &DayProjection) -> Line<'static> {
    match &day.hold {
        Some(hold) => Line::from(vec![
            scene.paint(
                Paint::White,
                format!("{} of {}", secs(hold.eligible_s), secs(hold.required_s)),
            ),
            scene.paint(
                Paint::Grey,
                format!(" · holding {}", profile_text(&hold.profile)),
            ),
        ]),
        None => value(scene, Paint::Grey, "no hold started".to_owned()),
    }
}

fn reason_words(reason: &HoldReason) -> String {
    match reason {
        HoldReason::Incomplete {
            credited_s,
            required_s,
        } => format!("incomplete, {} of {}", secs(*credited_s), secs(*required_s)),
        HoldReason::AtCap {} => "at the cap".to_owned(),
        HoldReason::NotQualified {} => "not qualified".to_owned(),
        HoldReason::Vetoed { veto } => format!("vetoed by {}", event_name(veto)),
        HoldReason::QuestionPending { .. } => "a question is pending".to_owned(),
        HoldReason::EventUnresolved { event } => format!("{} is unresolved", event_name(event)),
        HoldReason::Frozen {} => "frozen".to_owned(),
        HoldReason::Paused {} => "paused".to_owned(),
        HoldReason::Expired {} => "expired".to_owned(),
        HoldReason::QualificationFailed { profile, .. } => {
            format!(
                "{} failed qualification",
                profile_text(&UnitProfile::of_ccd(profile.ccd()))
            )
        }
    }
}

fn ccds_text(changed: &[togi_machine::CcdId]) -> String {
    changed
        .iter()
        .map(|ccd| ccd.get().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn step_value(scene: &Scene<'_>, step: &DayStep) -> Line<'static> {
    let applied = scene.s.step_applied(step);
    let (paint, text) = match &step.step {
        StepKind::Hold { blocked_by } if blocked_by.is_empty() => {
            (Paint::Text, "hold, nothing blocks it".to_owned())
        }
        StepKind::Hold { blocked_by } => (
            Paint::Warn,
            format!(
                "hold: {}",
                blocked_by
                    .iter()
                    .map(reason_words)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
        StepKind::Promote { changed } if applied => {
            (Paint::Good, format!("promoted CCD {}", changed.get()))
        }
        StepKind::Promote { changed } => (
            Paint::Grey,
            format!("proposed promoting CCD {}, not applied", changed.get()),
        ),
        StepKind::Retreat { changed, .. } if applied => {
            (Paint::Bad, format!("retreated CCD {}", ccds_text(changed)))
        }
        StepKind::Retreat { changed, .. } => (
            Paint::Grey,
            format!(
                "proposed retreating CCD {}, not applied",
                ccds_text(changed)
            ),
        ),
        StepKind::FirmwareZero { changed } if applied => (
            Paint::Warn,
            format!("reset CCD {} after firmware change", ccds_text(changed)),
        ),
        StepKind::FirmwareZero { changed } => (
            Paint::Grey,
            format!(
                "proposed resetting CCD {} after firmware change, not applied",
                ccds_text(changed)
            ),
        ),
    };
    value(scene, paint, text)
}

fn firmware_value(scene: &Scene<'_>, firmware: &FirmwareValidity) -> Line<'static> {
    match firmware {
        FirmwareValidity::Valid => value(scene, Paint::Good, "valid".to_owned()),
        FirmwareValidity::Invalidated { since } => value(
            scene,
            Paint::Warn,
            format!("changed at #{since}; D awaits a reset"),
        ),
        FirmwareValidity::Unknown => value(scene, Paint::Grey, "not reported yet".to_owned()),
    }
}

fn event_value(scene: &Scene<'_>, event: &EventProjection) -> Line<'static> {
    let class = match &event.classification {
        Classification::CpuSuspect { certainty } => format!("cpu suspect ({certainty})"),
        Classification::NonCpu {} => "not the cpu".to_owned(),
        Classification::Unexplained {} => "unexplained".to_owned(),
    };
    let (paint, resolution) = match event.resolution {
        Resolution::Unresolved {} => (Paint::Warn, "unresolved"),
        Resolution::Resolved { .. } => (Paint::Grey, "resolved"),
    };
    let spans: Vec<Span<'static>> = vec![
        scene.paint(
            Paint::Text,
            format!("{} · {class} · ", event_name(&event.event)),
        ),
        scene.paint(paint, resolution.to_owned()),
        scene.paint(
            Paint::Grey,
            format!(" · reproduction {}", event.reproduction.as_str()),
        ),
    ];
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use jiff::SignedDuration;
    use jiff::tz::TimeZone;
    use togi_journal::{Activity, Queue, StepKind, UnavailableBackend};
    use togi_machine::Backend;

    use super::context;
    use crate::frames::fit::plain;
    use crate::frames::hand::{Hand, bare, ccd, ccd_row, core_row, hold, night, profile_of, step};
    use crate::frames::measure::measure;
    use crate::frames::scene::Scene;
    use crate::frames::testing::{FIXED_NOW, screen};
    use crate::history::LogEntry;
    use crate::screen::{Colours, View};

    fn rows(hand: &Hand, after_s: i64, width: u16, height: u16) -> Vec<String> {
        let zone = TimeZone::UTC;
        let now = FIXED_NOW + SignedDuration::from_secs(after_s);
        let scene = Scene::new(hand.snapshot(), now, &zone, Colours::None);
        let measured = measure(
            &scene,
            &screen(View::Main, width, height, Colours::None, true),
        );
        context(&scene, &measured)
            .iter()
            .map(|line| plain(line, Colours::None))
            .collect()
    }

    fn hand() -> Hand {
        let mut hand = Hand::new(
            vec![core_row(0, 0, Some(0), false)],
            bare(vec![ccd_row(0, &[0], 0)]),
        );
        hand.policy.night = Some(night(Some((Queue::Mapping, 2, Activity::Mapping))));
        hand.policy.day.hold = Some(hold(&[(0, -10)], 3600, 7200));
        hand
    }

    /// The hand's night, closed by a boot six hours after it started.
    fn closed(mut hand: Hand) -> Hand {
        let boot = crate::tests::kit::seq(2);
        if let Some(night) = hand.policy.night.as_mut() {
            night.closed_by = Some(boot);
        }
        hand.log.push(LogEntry {
            seq: boot,
            at: FIXED_NOW + SignedDuration::from_hours(6),
            tag: "day boot".into(),
            msg: "a later boot".to_owned(),
        });
        hand
    }

    #[test]
    fn the_open_night_comes_first_and_the_day_after_it() {
        let rows = rows(&hand(), 60, 240, 67);
        assert!(
            rows[0].starts_with("NIGHT since 22:13 · 0h30m elapsed ─"),
            "{:?}",
            rows[0]
        );
        assert!(
            rows[1].starts_with("task        mapping task 2"),
            "{:?}",
            rows[1]
        );
        let day = rows
            .iter()
            .position(|row| row.starts_with("DAY"))
            .expect("the day section");
        assert!(
            rows[day - 1].trim().is_empty(),
            "a blank row separates the sections"
        );
        assert!(
            rows[day].trim_end().ends_with("applied CO 0"),
            "{:?}",
            rows[day]
        );
        assert_eq!(
            rows[day + 1],
            "Ruleset 13  mapping-only cap; CO 0 is not hazard-goal proof"
        );
        assert!(rows[day + 2].contains("holding -10"), "{:?}", rows[day + 2]);
        assert!(
            rows.iter()
                .any(|row| row
                    == "shares      mapping 66%/10% · qualification 33%/75% · control 0%/5%"),
            "{rows:#?}"
        );
        assert!(
            rows.iter().any(|row| row.starts_with("firmware")),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_closed_night_follows_the_day_and_says_when_it_ran() {
        let rows = rows(&closed(hand()), 7 * 3600, 240, 67);
        assert!(rows[0].starts_with("DAY"), "{:?}", rows[0]);
        assert!(
            rows.iter()
                .any(|row| row.starts_with("LAST NIGHT 22:13-04:13 ─")),
            "{rows:#?}"
        );
        assert!(!rows.iter().any(|row| row.contains("until")), "{rows:#?}");
    }

    #[test]
    fn a_closed_night_whose_boot_left_the_log_names_only_its_start() {
        let mut hand = closed(hand());
        hand.log.clear();
        let rows = rows(&hand, 7 * 3600, 240, 67);
        assert!(
            rows.iter().any(|row| row.starts_with("LAST NIGHT 22:13 ─")),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_filler_shows_its_part_without_a_share() {
        let mut hand = hand();
        if let Some(night) = hand.policy.night.as_mut() {
            for budget in &mut night.budgets {
                if budget.queue == Queue::Filler {
                    budget.consumed_s = 200;
                }
            }
        }
        let rows = rows(&hand, 60, 240, 67);
        assert!(
            rows.iter().any(|row| row.starts_with("shares")
                && row.trim_end().ends_with("control 0%/5% · filler 10%")),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_backends_the_night_cannot_use_are_named() {
        let mut hand = hand();
        if let Some(night) = hand.policy.night.as_mut() {
            night.unavailable = vec![UnavailableBackend {
                backend: Backend::Mprime,
                causes: vec![crate::tests::kit::seq(3)],
            }];
        }
        let rows = rows(&hand, 60, 240, 67);
        assert!(
            rows.iter()
                .any(|row| row.trim_end() == "unavailable mprime"),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_machine_with_no_night_shows_only_the_day() {
        let mut hand = hand();
        hand.policy.night = None;
        let rows = rows(&hand, 60, 240, 67);
        assert!(rows[0].starts_with("DAY"), "{:?}", rows[0]);
        assert!(!rows.iter().any(|row| row.contains("NIGHT")), "{rows:#?}");
    }

    #[test]
    fn the_last_step_the_freeze_and_the_applied_profile_are_named() {
        let mut hand = closed(hand());
        hand.policy.applied = Some(profile_of(&[(0, -10)]));
        hand.policy.day.freeze = true;
        let promoted = step(StepKind::Promote { changed: ccd(0) });
        hand.applied_steps.insert(promoted.decision);
        hand.policy.day.last_step = Some((crate::tests::kit::seq(5), promoted));
        let rows = rows(&hand, 7 * 3600, 240, 67);
        assert!(rows[0].trim_end().ends_with("applied -10"), "{:?}", rows[0]);
        assert!(
            rows.iter()
                .any(|row| row.starts_with("last step   promoted CCD 0")),
            "{rows:#?}"
        );
        assert!(
            rows.iter().any(|row| row.starts_with("freeze")),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_compact_panel_keeps_no_blank_row_between_sections() {
        let rows = rows(&hand(), 60, 120, 33);
        assert!(rows.iter().all(|row| !row.trim().is_empty()), "{rows:#?}");
    }

    #[test]
    fn the_day_shows_observed_totals_beside_stock_and_no_bound() {
        use jiff::Timestamp;
        use togi_journal::{DayBootStart, DayCheckpoint, DayUncleanEnd, Event, Stamp};
        use togi_machine::{
            BiosContext, EndEvidence, ObservedOffset, PerCore, ResetKind, ResetReason,
        };

        use crate::day_totals::DayTotals;
        use crate::tests::kit::{core, seq};

        let at = |offset: i64| -> PerCore<ObservedOffset> {
            [(core(0), ObservedOffset(offset))].into_iter().collect()
        };
        let start = |offset: i64| DayBootStart {
            build: crate::tests::kit::build(),
            bios: BiosContext::default(),
            offsets: at(offset),
            watchdog: None,
        };
        let checkpoint = |applied_secs: u64| DayCheckpoint {
            applied_secs,
            load_mix: togi_journal::LoadMix::default(),
            watchdog: None,
        };
        let end = DayUncleanEnd {
            boot: "b".to_owned().into(),
            last_checkpoint: None,
            offsets: at(-10),
            reset_reason: ResetReason {
                kind: Some(ResetKind::Watchdog),
                raw: Vec::new(),
                supported: true,
            },
            evidence: EndEvidence::unknown(),
        };
        let events: Vec<Event> = [
            ("a", start(0).into()),
            ("a", checkpoint(7200).into()),
            ("b", start(-10).into()),
            ("b", checkpoint(3600).into()),
            ("c", end.into()),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (boot, payload))| {
            Event::new(
                seq(u64::try_from(index).unwrap() + 1),
                Stamp {
                    time: Timestamp::UNIX_EPOCH,
                    mono: None,
                },
                boot.to_owned().into(),
                Vec::new(),
                payload,
            )
        })
        .collect();
        let mut hand = hand();
        hand.totals = DayTotals::of(&events);
        let rows = rows(&hand, 60, 240, 67);
        assert!(
            rows.iter()
                .any(|row| row.trim_end()
                    == "all days    3.0h observed, 1 crash · 0 unknown intervals"),
            "{rows:#?}"
        );
        assert!(
            rows.iter().any(|row| row.trim_end()
                == "stock CO 0  2.0h observed, 0 crashes · observations, not a bound"),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_day_with_no_observations_shows_no_totals() {
        let rows = rows(&hand(), 60, 240, 67);
        assert!(
            !rows.iter().any(|row| row.starts_with("all days")),
            "{rows:#?}"
        );
    }
}
