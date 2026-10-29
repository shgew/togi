use jiff::tz::TimeZone;
use test_case::test_case;
use togi_journal::{
    Activity, Budget, DayEventId, DeadEndCondition, NightId, Queue, ReproductionState, Seq, TaskId,
    UnavailableBackend,
};
use togi_machine::{Backend, CcdId, CcdProfile, CoreId, PerCore, TargetOffset};
use togi_tuner::policy::{
    DayProjection, FirmwareValidity, MappingProjection, NightProjection, Projection,
    QualificationProgress, ReproductionProjection,
};
use togi_tuner::unit::{UnitId, UnitProfile};

use super::dead_end_label;
use super::policy::{Where, activity, sections};

fn core(id: i64) -> CoreId {
    CoreId::new(id).unwrap()
}

#[test]
fn every_dead_end_condition_has_its_own_display_label_and_fits_eighty_columns() {
    let labels: Vec<(DeadEndCondition, &str)> = DeadEndCondition::ALL
        .iter()
        .map(|&condition| (condition, dead_end_label(condition)))
        .collect();
    assert_eq!(
        labels,
        [
            (DeadEndCondition::FailureAtZero, "failure at offset 0"),
            (DeadEndCondition::Smu, "SMU error"),
            (DeadEndCondition::NoEvidence, "no conclusive evidence"),
            (DeadEndCondition::BootLoop, "boot loop"),
            (DeadEndCondition::Containment, "workload containment failed"),
            (DeadEndCondition::Preflight, "preflight failed"),
            (
                DeadEndCondition::Defect,
                "recorded decisions affected by a defect"
            ),
            (DeadEndCondition::ThermalTrip, "thermal trip"),
        ]
    );
    assert_eq!(labels.len(), 8);
    for (condition, label) in labels {
        let seq = Seq::new(99_999_999).unwrap();
        let line = format!("dead end: {label} [#{seq}]");
        assert!(line.chars().count() <= 80, "{condition}: {line}");
        assert!(!label.contains('_'), "{condition}: wire token in {label}");
    }
}

fn armed() -> togi_machine::WatchdogState {
    togi_machine::WatchdogState::Armed {
        detail: "watchdog0 (SP5100 TCO timer) active, held by PID 1".to_owned(),
    }
}

fn day_events(
    extra: impl IntoIterator<Item = (&'static str, togi_journal::Payload)>,
) -> Vec<togi_journal::Event> {
    use jiff::Timestamp;
    use togi_journal::{
        DayBootStart, DayCheckpoint, DayCleanStop, DayUncleanEnd, Event, Payload, Stamp,
    };
    use togi_machine::{BiosContext, EndEvidence, ObservedOffset, ResetKind, ResetReason};

    let offsets = [(core(0), ObservedOffset(-10))]
        .into_iter()
        .collect::<PerCore<_>>();
    let start = || {
        Payload::from(DayBootStart {
            build: crate::report::current_build(),
            bios: BiosContext {
                bios_version: "test".into(),
                board: "board".into(),
                cpu_model: "CPU".into(),
                microcode: "test".into(),
                boost_limit_mhz: 5000,
            },
            offsets,
            watchdog: Some(armed()),
        })
    };
    [
        ("day-a", start()),
        (
            "day-a",
            DayCheckpoint {
                applied_secs: 3600,
                load_mix: togi_journal::LoadMix::default(),
                watchdog: None,
            }
            .into(),
        ),
        ("day-a", DayCleanStop {}.into()),
        ("day-b", start()),
        (
            "day-b",
            DayCheckpoint {
                applied_secs: 1800,
                load_mix: togi_journal::LoadMix::default(),
                watchdog: None,
            }
            .into(),
        ),
        (
            "day-c",
            DayUncleanEnd {
                boot: "day-b".to_owned().into(),
                last_checkpoint: Some(Seq::new(5).unwrap()),
                offsets,
                reset_reason: ResetReason {
                    kind: Some(ResetKind::Watchdog),
                    raw: Vec::new(),
                    supported: true,
                },
                evidence: EndEvidence::unknown(),
            }
            .into(),
        ),
    ]
    .into_iter()
    .chain(extra)
    .enumerate()
    .map(|(index, (boot, payload))| {
        Event::new(
            Seq::new(u64::try_from(index).unwrap() + 1).unwrap(),
            Stamp {
                time: Timestamp::UNIX_EPOCH,
                mono: None,
            },
            boot.to_owned().into(),
            Vec::new(),
            payload,
        )
    })
    .collect()
}

fn answer(kind: togi_journal::DayAnswerKind) -> (&'static str, togi_journal::Payload) {
    let offsets = [(core(0), togi_machine::ObservedOffset(-10))]
        .into_iter()
        .collect::<PerCore<_>>();
    (
        "day-c",
        togi_journal::DayAnswer {
            boot: "day-b".to_owned().into(),
            offsets,
            answer: kind,
            uid: 1000,
        }
        .into(),
    )
}

#[test]
fn day_section_snapshot() {
    let mut out = String::new();
    super::render_day(&mut out, &day_events([]));
    assert_eq!(out, include_str!("day-status.txt"));
}

#[test]
fn the_day_totals_sit_beside_the_ledger_hours_and_say_they_are_not_a_bound() {
    let events = day_events([]);
    let mut out = String::new();
    super::render_day(&mut out, &events);
    for line in [
        "all profiles: 1.5h observed, 1 crash (0 unknown intervals)\n",
        "stock (CO 0): 0.0h observed, 0 crashes\n",
        "totals are observations, not a bound\n",
    ] {
        assert!(out.contains(line), "{line:?} missing from\n{out}");
    }
    assert!(out.lines().all(|line| line.chars().count() <= 80), "{out}");
    let ledger = crate::day::Ledger::of(&events);
    let hours: u64 = ledger.profiles.values().sum();
    let totals = togi_watch::day_totals::DayTotals::of(&events);
    assert_eq!(totals.all().secs, hours);
}

#[test_case::test_case(togi_journal::DayAnswerKind::PowerOff, "0 crashes"; "a power-off answer is not a crash")]
#[test_case::test_case(togi_journal::DayAnswerKind::Frozen, "1 crash"; "a frozen answer is a crash")]
fn an_answer_changes_the_crash_total(kind: togi_journal::DayAnswerKind, crashes: &str) {
    let mut out = String::new();
    super::render_day(&mut out, &day_events([answer(kind)]));
    let want = format!("all profiles: 1.5h observed, {crashes} (0 unknown intervals)\n");
    assert!(out.contains(&want), "{want:?} missing from\n{out}");
}

#[test]
fn no_day_boots_print_no_day_section() {
    let mut out = String::new();
    super::render_day(&mut out, &[]);
    assert_eq!(out, "");
}

fn restart_with(
    watchdog: Option<togi_machine::WatchdogState>,
) -> (&'static str, togi_journal::Payload) {
    (
        "day-d",
        togi_journal::DayBootStart {
            build: crate::report::current_build(),
            bios: togi_machine::BiosContext::default(),
            offsets: PerCore::default(),
            watchdog,
        }
        .into(),
    )
}

#[test]
fn the_latest_observer_start_decides_the_reported_watchdog() {
    let unarmed = togi_machine::WatchdogState::Unarmed {
        detail: "watchdog0 state \"inactive\", want active".to_owned(),
    };
    let mut out = String::new();
    super::render_day(&mut out, &day_events([restart_with(Some(unarmed))]));
    assert!(
        out.contains("hardware watchdog at the last observer check: not armed (watchdog0 state"),
        "{out}"
    );
}

#[test]
fn a_start_recorded_before_the_check_reports_the_watchdog_as_not_recorded() {
    let mut out = String::new();
    super::render_day(&mut out, &day_events([restart_with(None)]));
    assert!(
        out.contains("hardware watchdog at the last observer check: not recorded\n"),
        "{out}"
    );
}

#[test_case(true; "counting")]
#[test_case(false; "open but not counting")]
fn a_checkpoint_refreshes_the_watchdog_report(counting: bool) {
    let watchdog = if counting {
        armed()
    } else {
        togi_machine::WatchdogState::Unarmed {
            detail: "watchdog0 open but not counting: timeleft 4294967295 s exceeds timeout 60 s"
                .into(),
        }
    };
    let mut out = String::new();
    super::render_day(
        &mut out,
        &day_events([(
            "day-b",
            togi_journal::DayCheckpoint {
                applied_secs: 1800,
                load_mix: togi_journal::LoadMix::default(),
                watchdog: Some(watchdog),
            }
            .into(),
        )]),
    );
    let expected = if counting {
        "hardware watchdog at the last observer check: armed"
    } else {
        "hardware watchdog at the last observer check: not armed (watchdog0 open"
    };
    assert!(out.contains(expected), "{out}");
}

#[test]
fn an_unanswered_unclean_end_is_a_question_with_four_answers() {
    let mut out = String::new();
    super::render_questions(&mut out, &day_events([]));
    assert_eq!(out, include_str!("day-question-pending.txt"));
}

#[test]
fn an_answered_end_shows_the_answer_and_how_to_change_it() {
    let mut out = String::new();
    let events = day_events([answer(togi_journal::DayAnswerKind::Frozen)]);
    super::render_questions(&mut out, &events);
    assert_eq!(out, include_str!("day-question-answered.txt"));
}

#[test]
fn a_changed_answer_replaces_the_earlier_one() {
    let mut out = String::new();
    let events = day_events([
        answer(togi_journal::DayAnswerKind::Frozen),
        answer(togi_journal::DayAnswerKind::Graphics),
    ]);
    super::render_questions(&mut out, &events);
    assert_eq!(out, include_str!("day-question-changed.txt"));
}

fn projection(night: Option<NightProjection>) -> Projection {
    Projection {
        climb: togi_tuner::ClimbProjection::default(),
        applied: None,
        units: Vec::new(),
        day: DayProjection {
            hold: None,
            last_step: None,
            freeze: false,
            firmware: FirmwareValidity::Unknown,
            pending_questions: Vec::new(),
        },
        events: Vec::new(),
        vetoes: Vec::new(),
        qualifications: Vec::new(),
        night,
        stopped: false,
    }
}

fn budget(queue: Queue, share_pct: u8, consumed_s: u64) -> Budget {
    Budget {
        queue,
        share_pct,
        consumed_s,
    }
}

fn night() -> NightProjection {
    NightProjection {
        night: NightId(Seq::new(40).unwrap()),
        started: "2026-01-01T00:00:00Z".parse().unwrap(),
        closed_by: None,
        budgets: vec![
            budget(Queue::Reproduction, 25, 1_800),
            budget(Queue::Qualification, 75, 5_400),
            budget(Queue::Mapping, 10, 0),
            budget(Queue::Control, 0, 0),
            budget(Queue::Filler, 0, 1_800),
        ],
        tasks: Vec::new(),
        current: None,
        mapping: vec![MappingProjection {
            unit: UnitId::Ccd(CcdId::new(1).unwrap()),
            cursor: Some(TargetOffset::new(-12).unwrap()),
            bracket: Some((
                TargetOffset::new(-20).unwrap(),
                TargetOffset::new(-10).unwrap(),
            )),
        }],
        qualification: Some(QualificationProgress {
            profile: UnitProfile::of_ccd(&CcdProfile::zero([CcdId::new(0).unwrap()]).unwrap()),
            outstanding: Vec::new(),
            witnessed: 3,
            eligible_s: 900,
        }),
        reproductions: vec![ReproductionProjection {
            source: DayEventId::Boot {
                boot: "day-a".to_owned().into(),
            },
            state: ReproductionState::Pending,
            consumed_s: 5_400,
            remaining: Vec::new(),
        }],
        unavailable: vec![UnavailableBackend {
            backend: Backend::Ycruncher,
            causes: vec![Seq::new(31).unwrap(), Seq::new(35).unwrap()],
        }],
        filler: 4,
    }
}

static ZONE: TimeZone = TimeZone::UTC;

const UTC: Where<'static> = Where {
    zone: &ZONE,
    closed: None,
};

fn shown(policy: &Projection, at: Where<'_>) -> String {
    let mut out = String::new();
    sections(&mut out, policy, at);
    out
}

#[test]
fn the_night_part_shows_its_start_elapsed_time_shares_and_progress() {
    let out = shown(&projection(Some(night())), UTC);
    let night = out.split_once("\nNight since 00:00\n").unwrap().1;
    assert!(night.starts_with("elapsed: 2h30m\n"), "{out}");
    let table: Vec<&str> = night.lines().skip(1).take(5).collect();
    assert_eq!(
        table,
        [
            "QUEUE          SHARE  USED  TIME",
            "reproduction   25%    20%   30m00s",
            "qualification  75%    60%   1h30m",
            "mapping        10%    0%    0s",
            "filler         -      20%   30m00s",
        ],
        "{out}"
    );
    assert!(!out.contains("control"), "{out}");
    assert!(
        out.contains("\nunavailable: ycruncher, excluded by #31 #35\n"),
        "{out}"
    );
    assert!(out.contains("current task: none"), "{out}");
    assert!(
        out.contains("mapping CCD 1: next offset -12, bracketing the failure at -20"),
        "{out}"
    );
    assert!(
        out.contains("0.2 h eligible witnessed exposure, 0 outstanding"),
        "{out}"
    );
    assert!(
        out.contains("reproduction: boot day-a: reproduction pending, 1.5 h consumed"),
        "{out}"
    );
}

#[test]
fn a_closed_night_is_the_last_night_and_its_end_is_the_boot_that_closed_it() {
    let mut closed = night();
    closed.closed_by = Some(Seq::new(90).unwrap());
    let policy = projection(Some(closed));
    let ended = Where {
        closed: Some("2026-01-01T07:15:00Z".parse().unwrap()),
        ..UTC
    };
    assert!(
        shown(&policy, ended).contains("\nLast night 00:00\u{2013}07:15\n"),
        "{}",
        shown(&policy, ended)
    );
    assert!(shown(&policy, UTC).contains("\nLast night 00:00\n"));
    assert_eq!(activity(&policy, None, ended), "day");
}

#[test]
fn an_open_night_leads_the_header_with_its_start_elapsed_time_and_task() {
    let mut open = night();
    assert_eq!(
        activity(&projection(Some(open.clone())), None, UTC),
        "night since 00:00, 2h30m elapsed: between tasks"
    );
    open.current = Some((
        TaskId {
            queue: Queue::Qualification,
            ordinal: 2,
        },
        Some(Activity::Qualification),
    ));
    assert_eq!(
        activity(&projection(Some(open.clone())), None, UTC),
        "night since 00:00, 2h30m elapsed: qualification (task qualification #2)"
    );
    open.current = Some((
        TaskId {
            queue: Queue::Filler,
            ordinal: 1,
        },
        None,
    ));
    assert_eq!(
        activity(&projection(Some(open)), None, UTC),
        "night since 00:00, 2h30m elapsed: filler (task filler #1)"
    );
}

