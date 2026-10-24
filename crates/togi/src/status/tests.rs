use togi_journal::{DeadEndCondition, HuntStage, Member, Seq, TrialId};
use togi_machine::{CoreId, CoreSet, PerCore, Regime, TargetOffset};
use togi_tuner::{GroupState, HuntGroup};

use super::{
    GROUP_CORES_MAX, Parked, dead_end_label, group_cores, hunt_failure, missing_coverage,
    parked_source, rounds, search_left, self_sufficiency, unconfirmed_note,
};

fn core(id: i64) -> CoreId {
    CoreId::new(id).unwrap()
}

fn group(cores: &[i64], stage: HuntStage) -> HuntGroup {
    HuntGroup {
        number: 1,
        cores: cores.iter().map(|&id| core(id)).collect(),
        profile: PerCore::default(),
        stage,
        probe: None,
        held: Vec::new(),
        outcome: GroupState::Running,
        passed: 0,
        needed: 5,
        carried: false,
        running: true,
    }
}

#[test]
fn a_full_cycle_lacks_nothing() {
    let steps = [
        Regime::R1,
        Regime::R1,
        Regime::R1,
        Regime::R2,
        Regime::R2,
        Regime::R2,
        Regime::R3,
        Regime::R4,
        Regime::R5,
        Regime::R6,
        Regime::R7,
        Regime::R7,
        Regime::R7,
    ];
    assert!(missing_coverage(steps).is_empty());
}

#[test]
fn a_short_cycle_names_each_regime_it_lacks() {
    let steps = [Regime::R1, Regime::R2, Regime::R2, Regime::R2, Regime::R3];
    assert_eq!(
        missing_coverage(steps),
        [
            "R1: 2 more steps",
            "R4: 1 more step",
            "R5: 1 more step",
            "R6: 1 more step",
            "R7: 3 more steps",
        ]
    );
}

#[test]
fn rounds_agree_with_their_count() {
    assert_eq!(rounds(1), "1 round");
    assert_eq!(rounds(2), "2 rounds");
    assert_eq!(rounds(3), "3 rounds");
}

#[test]
fn the_search_header_agrees_with_the_cores_left() {
    assert_eq!(search_left(1), "search: 1 core left");
    assert_eq!(search_left(2), "search: 2 cores left");
    assert_eq!(search_left(16), "search: 16 cores left");
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

#[test]
fn the_unconfirmed_note_agrees_with_the_cores_and_scopes_the_confirmed_profile() {
    let confirmed = Seq::new(7).unwrap();
    let one = unconfirmed_note(9, [core(1)], confirmed);
    assert!(
        one.starts_with("* unconfirmed since [#9]: core 01 shows a stepped-back offset; other BIOS offsets come from the profile confirmed by the passed full cycle [#7]. "),
        "{one}"
    );
    let many = unconfirmed_note(9, [core(1), core(2)], confirmed);
    assert!(
        many.starts_with("* unconfirmed since [#9]: cores 01 02 show stepped-back offsets; other BIOS offsets come from"),
        "{many}"
    );
    assert!(
        many.ends_with("which stays marked until the hold ends"),
        "{many}"
    );
}

#[test]
fn the_self_sufficiency_count_agrees_with_its_noun() {
    assert_eq!(self_sufficiency(0), "not yet demonstrated");
    assert_eq!(self_sufficiency(1), "observed (1 passing trial)");
    assert_eq!(self_sufficiency(2), "observed (2 passing trials)");
}

#[test]
fn parked_offsets_say_where_they_come_from() {
    let seq = Seq::new(12).unwrap();
    assert_eq!(parked_source(&Parked::Zero), "parked offsets all-zero");
    assert_eq!(
        parked_source(&Parked::CycleEnd(seq)),
        "parked offsets from cycle end #12"
    );
    let held: CoreSet = [core(0), core(1)].into_iter().collect();
    assert_eq!(
        parked_source(&Parked::Held(held)),
        "loaded cores 00 01 held at their failing offsets, the rest parked at 0"
    );
}

#[test]
fn a_hunt_failure_without_a_trial_says_no_trial_was_in_flight() {
    let trial = TrialId::from("0412".to_owned());
    assert_eq!(
        hunt_failure("crash", Regime::R6, Some(&trial)),
        "unattributed crash in R6 trial 0412"
    );
    assert_eq!(
        hunt_failure("crash", Regime::R6, None),
        "unattributed crash with no trial in flight"
    );
}

#[test]
fn a_locate_group_is_identified_and_explained_unlike_an_ordinary_group() {
    let (cell, note) = group_cores(&group(&[], HuntStage::Locate));
    assert_eq!(cell, "locate");
    let (label, text) = note.expect("a locate group carries a note");
    assert_eq!(label, "locate");
    assert!(text.contains("every unloaded core at 0"), "{text}");

    let (cell, note) = group_cores(&group(&[0, 1], HuntStage::Part));
    assert_eq!((cell.as_str(), note), ("00 01", None));

    let wide: Vec<i64> = (0..16).collect();
    let (cell, note) = group_cores(&group(&wide, HuntStage::Full));
    assert_eq!(cell, "16 cores");
    assert!(note.is_some_and(|(label, text)| label == "cores" && text.len() > GROUP_CORES_MAX));

    let mut probe = group(&[0], HuntStage::Probe);
    probe.probe = Some(Member {
        core: core(0),
        offset: TargetOffset::new(-5).unwrap(),
    });
    let (cell, note) = group_cores(&probe);
    assert_eq!(cell, "probe");
    assert_eq!(note.map(|(label, _)| label), Some("probe"));
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
        })
    };
    [
        ("day-a", start()),
        (
            "day-a",
            DayCheckpoint {
                applied_secs: 3600,
                load_mix: Default::default(),
            }
            .into(),
        ),
        ("day-a", DayCleanStop {}.into()),
        ("day-b", start()),
        (
            "day-b",
            DayCheckpoint {
                applied_secs: 1800,
                load_mix: Default::default(),
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
