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
        climb: Default::default(),
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
    assert!(out.contains("3 cells witnessed, 0 outstanding"), "{out}");
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

fn ccd_vector(offsets: &[(i64, i64)]) -> togi_journal::CcdVector {
    let profile = CcdProfile::try_from(
        offsets
            .iter()
            .map(|&(ccd, offset)| (CcdId::new(ccd).unwrap(), TargetOffset::new(offset).unwrap()))
            .collect::<std::collections::BTreeMap<_, _>>(),
    )
    .unwrap();
    let topology: Vec<_> = offsets
        .iter()
        .map(|&(ccd, _)| togi_machine::CoreInfo {
            core: core(ccd),
            ccd: CcdId::new(ccd).unwrap(),
            cpus: Vec::new(),
        })
        .collect();
    togi_journal::CcdVector::expand(profile, &topology).unwrap()
}

#[test]
fn a_fallback_retreat_names_the_profile_it_wants_and_why_it_holds_co_0() {
    let mut policy = projection(None);
    policy.day.last_step = Some((
        Seq::new(12).unwrap(),
        togi_journal::DayStep {
            decision: togi_journal::DecisionId(3),
            cold_boot: "boot-9".to_owned().into(),
            old: ccd_vector(&[(0, -10), (1, -10)]),
            proposed: ccd_vector(&[(0, 0), (1, 0)]),
            step: togi_journal::StepKind::Retreat {
                changed: vec![CcdId::new(0).unwrap(), CcdId::new(1).unwrap()],
                cause: togi_journal::RetreatCause::ShallowerCap {},
                wanted: Some(ccd_vector(&[(0, -5), (1, -5)])),
            },
            predecessor: None,
            hours: Vec::new(),
            hold_s: 0,
            causes: togi_journal::StepCauses::default(),
            reason: String::new(),
        },
    ));
    let out = shown(&policy, UTC);
    let step = out.split_once("latest step").unwrap().1;
    let step = step.split_once("\nfreeze").unwrap().0;
    let step = step.split_whitespace().collect::<Vec<_>>().join(" ");
    assert_eq!(
        step,
        "[#12] at cold boot boot-9: retreats CCD 0, 1 to CCD 0 0, CCD 1 0: a cap became shallower; the wanted CCD 0 -5, CCD 1 -5 is not qualified, so it falls back to CO 0 until it is",
        "{out}"
    );
}

fn ccd_projection(
    ccd: i64,
    cores: &[i64],
    edge: Option<togi_journal::Edge>,
) -> togi_tuner::policy::UnitProjection {
    togi_tuner::policy::UnitProjection {
        unit: UnitId::Ccd(CcdId::new(ccd).unwrap()),
        cores: cores.iter().map(|&id| core(id)).collect(),
        edge,
        cap: Some(togi_tuner::policy::CapProjection {
            offset: TargetOffset::new(-1).unwrap(),
            binding: togi_journal::Binding::Baseline {},
            slope: togi_journal::Slope::Fallback {
                beta: togi_journal::Finite::new(1.0).unwrap(),
            },
            adequate: Vec::new(),
            uncovered: Vec::new(),
            witness: Seq::new(7).unwrap(),
        }),
        day: TargetOffset::new(-10).unwrap(),
        remaining: Some(0),
    }
}

/// Where each header of the CCD table starts on `line`, and where each value of a row starts.
fn starts(line: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut previous = ' ';
    for (at, c) in line.char_indices() {
        if c != ' ' && previous == ' ' {
            out.push(at);
        }
        previous = c;
    }
    out
}

#[test_case(None; "an unknown edge")]
#[test_case(Some(togi_journal::Edge::Censored { limit: TargetOffset::new(-50).unwrap(), witnesses: Vec::new() }); "a censored edge")]
#[test_case(Some(togi_journal::Edge::Observed { offset: TargetOffset::new(-35).unwrap(), witness: Seq::new(4).unwrap() }); "an observed edge")]
fn the_ccd_table_keeps_every_value_under_its_header_within_eighty_columns(
    edge: Option<togi_journal::Edge>,
) {
    let mut policy = projection(None);
    policy.units = vec![
        ccd_projection(0, &[0, 1, 2, 3, 4, 5, 6, 7], edge.clone()),
        ccd_projection(1, &[8, 9, 10, 11, 12, 13, 14, 15], edge),
    ];
    let out = shown(&policy, UTC);
    let lines: Vec<&str> = out.lines().take(3).collect();
    let columns = [0, 4, 10, 14, 18, 27, 36, 49, 59, 67];
    assert_eq!(starts(lines[0]), columns, "{out}");
    assert_eq!(
        lines[0],
        "CCD CORES D   S   SLOPE    BINDING  U            REMAINING COVERED UNCOVERED"
    );
    for row in &lines[1..] {
        let at = starts(row);
        // U is the seventh column; `censored -50` is two words that start at its column and inside it.
        let at: Vec<usize> = at
            .into_iter()
            .filter(|&at| !(columns[6] < at && at < columns[7]))
            .collect();
        assert_eq!(at, columns, "{out}");
        assert!(row.len() <= 80, "{row:?} is wider than 80 columns");
    }
}

#[test]
fn noncontiguous_cores_are_counted_in_the_ccd_table_and_listed_under_their_row() {
    let mut policy = projection(None);
    policy.units = vec![
        ccd_projection(0, &[0, 2, 4, 6, 8, 10, 12, 14], None),
        ccd_projection(1, &[1, 3, 5, 7, 9, 11, 13, 15], None),
    ];
    let out = shown(&policy, UTC);
    let lines: Vec<&str> = out.lines().take(5).collect();
    let columns = starts(lines[0]);
    assert_eq!(columns.len(), 10, "{out}");
    // `8 cores` is two words that start at the CORES column and inside it.
    let cells = |row: &str| -> Vec<usize> {
        starts(row)
            .into_iter()
            .filter(|&at| !(columns[1] < at && at < columns[2]))
            .collect()
    };
    assert_eq!(cells(lines[1]), columns, "{out}");
    assert_eq!(cells(lines[3]), columns, "{out}");
    assert!(lines[1].starts_with("0   8 cores "), "{out}");
    assert_eq!(lines[2], "  cores 0 2 4 6 8 10 12 14");
    assert_eq!(lines[4], "  cores 1 3 5 7 9 11 13 15");
    assert!(out.lines().all(|line| line.chars().count() <= 80), "{out}");
}
