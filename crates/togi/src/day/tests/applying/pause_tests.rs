use super::*;
use crate::day::{pause::Pause, socket::Control};
use togi_journal::{DayPause, IntervalState, PauseSource, PauseState};

fn paused(journal: &mut Journal, state: PauseState) {
    if !journal.events().iter().any(|event| {
        matches!(&event.payload, Payload::DayBootStart(_)) && event.boot == *journal.boot()
    }) {
        append(
            journal,
            &ActiveClock::one(),
            DayBootStart {
                build: current_build(),
                bios: bios(),
                offsets: offsets(0),
                watchdog: Some(armed()),
            },
        )
        .unwrap();
    }
    append(
        journal,
        &ActiveClock::one(),
        DayPause {
            state,
            source: PauseSource::Command { uid: 1000 },
        },
    )
    .unwrap();
}

fn deadline() -> Timestamp {
    Timestamp::UNIX_EPOCH
        .checked_add(Duration::from_hours(1))
        .unwrap()
}

#[test]
fn untimed_pause_survives_restart_and_resume_but_ends_at_next_cold_boot() {
    let mut journal = seed();
    paused(&mut journal, PauseState::UntilBoot {});
    let mut hardware = Hardware::stock();
    let mut clock = ActiveClock::one();
    serve(
        &mut journal,
        &mut hardware,
        &mut clock,
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    let mut state = Pause::of(&journal);
    assert_eq!(state.status().pause, PauseState::UntilBoot {});
    let mut live = crate::day::apply::Live::preflight(&journal, &mut hardware).unwrap();
    state
        .control(
            &mut journal,
            &mut hardware,
            &clock,
            &mut live,
            Control::Reapply,
        )
        .unwrap();
    assert_eq!(Pause::of(&journal).status().pause, PauseState::UntilBoot {});
    journal.reboot("day-b".to_owned().into()).unwrap();
    serve(
        &mut journal,
        &mut hardware,
        &mut ActiveClock::one(),
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    assert_eq!(Pause::of(&journal).status().pause, PauseState::Running {});
    assert!(journal.events().iter().any(|event| matches!(
        &event.payload,
        Payload::DayPause(DayPause {
            source: PauseSource::ColdBoot {},
            ..
        })
    )));
}

#[test]
fn timed_pause_survives_cold_boot_restart_and_resume() {
    let mut journal = seed();
    paused(
        &mut journal,
        PauseState::Until {
            deadline: deadline(),
        },
    );
    let mut hardware = Hardware::stock();
    serve(
        &mut journal,
        &mut hardware,
        &mut ActiveClock::one(),
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    journal.reboot("day-b".to_owned().into()).unwrap();
    let mut clock = ActiveClock::one();
    serve(
        &mut journal,
        &mut hardware,
        &mut clock,
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    let mut state = Pause::of(&journal);
    let mut live = crate::day::apply::Live::preflight(&journal, &mut hardware).unwrap();
    state
        .control(
            &mut journal,
            &mut hardware,
            &clock,
            &mut live,
            Control::Reapply,
        )
        .unwrap();
    assert_eq!(
        Pause::of(&journal).status().pause,
        PauseState::Until {
            deadline: deadline()
        }
    );
}

#[test]
fn resume_early_acknowledges_only_after_the_stock_transaction_and_keeps_learning() {
    let mut journal = seed();
    paused(
        &mut journal,
        PauseState::Until {
            deadline: deadline(),
        },
    );
    let before = journal.events().len();
    let mut hardware = Hardware::stock();
    let mut live = crate::day::apply::Live::preflight(&journal, &mut hardware).unwrap();
    let mut state = Pause::of(&journal);
    let status = state
        .control(
            &mut journal,
            &mut hardware,
            &ActiveClock::one(),
            &mut live,
            Control::Resume { uid: 1000 },
        )
        .unwrap();
    assert_eq!(status.pause, PauseState::Running {});
    let events = &journal.events()[before..];
    let application = events
        .iter()
        .position(|event| matches!(event.payload, Payload::DayApplied(_)))
        .unwrap();
    let ended = events
        .iter()
        .position(|event| matches!(event.payload, Payload::DayPause(_)))
        .unwrap();
    assert!(application < ended);
    assert_eq!(hardware.writes, [TargetOffset::ZERO]);
    assert_eq!(applications(&journal).last().unwrap().readbacks, offsets(0));
    assert!(
        journal
            .events()
            .iter()
            .any(|event| matches!(event.payload, Payload::SessionBaseline(_)))
    );
}

#[test]
fn failed_resume_retains_the_durable_pause_and_never_claims_success() {
    let mut journal = seed();
    paused(&mut journal, PauseState::UntilBoot {});
    let mut hardware = Hardware::stock();
    let mut live = crate::day::apply::Live::preflight(&journal, &mut hardware).unwrap();
    hardware.failure = Failure::Write(1);
    assert!(
        Pause::of(&journal)
            .control(
                &mut journal,
                &mut hardware,
                &ActiveClock::one(),
                &mut live,
                Control::Resume { uid: 1000 }
            )
            .is_err()
    );
    assert_eq!(Pause::of(&journal).status().pause, PauseState::UntilBoot {});
    assert_eq!(applications(&journal).len(), 0);
}

#[test]
fn extending_replaces_the_deadline_and_zeroes_with_readback_again() {
    let mut journal = seed();
    paused(
        &mut journal,
        PauseState::Until {
            deadline: deadline(),
        },
    );
    let extended = deadline().checked_add(Duration::from_hours(1)).unwrap();
    let mut hardware = Hardware::stock();
    let mut live = crate::day::apply::Live::preflight(&journal, &mut hardware).unwrap();
    Pause::of(&journal)
        .control(
            &mut journal,
            &mut hardware,
            &ActiveClock::one(),
            &mut live,
            Control::Pause {
                state: PauseState::Until { deadline: extended },
                uid: 1000,
            },
        )
        .unwrap();
    assert_eq!(
        Pause::of(&journal).status().pause,
        PauseState::Until { deadline: extended }
    );
    assert_eq!(hardware.writes, [TargetOffset::ZERO]);
    assert!(
        journal
            .events()
            .iter()
            .any(|event| matches!(event.payload, Payload::SmuReadback(_)))
    );
}

#[test]
fn deadline_after_suspended_wall_time_reapplies_before_ending_pause() {
    let mut journal = seed();
    paused(
        &mut journal,
        PauseState::Until {
            deadline: deadline(),
        },
    );
    let mut clock = ActiveClock {
        active: Duration::ZERO,
        wall: Duration::ZERO,
        sleeps: [
            (Duration::from_secs(5), Duration::from_hours(1)),
            (Duration::from_secs(5), Duration::from_secs(5)),
        ]
        .into(),
    };
    let mut hardware = Hardware::stock();
    serve(
        &mut journal,
        &mut hardware,
        &mut clock,
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    assert_eq!(Pause::of(&journal).status().pause, PauseState::Running {});
    let ended = journal
        .events()
        .iter()
        .position(|event| {
            matches!(
                &event.payload,
                Payload::DayPause(DayPause {
                    source: PauseSource::Deadline {},
                    ..
                })
            )
        })
        .unwrap();
    assert!(matches!(
        journal.events()[ended - 1].payload,
        Payload::DayApplied(_)
    ));
    assert_eq!(
        hardware.writes,
        [TargetOffset::ZERO, TargetOffset::ZERO, TargetOffset::ZERO]
    );
    assert_eq!(
        Ledger::of(journal.events())
            .profiles
            .values()
            .copied()
            .sum::<u64>(),
        5
    );
}

#[test]
fn paused_stock_hours_feed_the_existing_exclusion_rule() {
    let mut journal = seed();
    paused(&mut journal, PauseState::UntilBoot {});
    let mut hardware = Hardware::stock();
    let mut clock = ActiveClock::one();
    serve(
        &mut journal,
        &mut hardware,
        &mut clock,
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    let mut live = crate::day::apply::Live::preflight(&journal, &mut hardware).unwrap();
    live.reapply(&mut journal, &mut hardware, &clock).unwrap();
    assert!(journal.events().iter().any(|event| matches!(&event.payload, Payload::DayInterval(span) if span.state == IntervalState::Paused && span.end_ns - span.start_ns == 120_000_000_000)));
    assert!(!journal.events().iter().any(|event| matches!(&event.payload, Payload::DayInterval(span) if span.state == IntervalState::Applied)));
    assert_eq!(
        Ledger::of(journal.events())
            .profiles
            .values()
            .copied()
            .sum::<u64>(),
        0
    );
}

#[test]
fn freeze_and_unfreeze_do_not_write_or_discard_the_committed_profile() {
    let mut journal = seed();
    let mut hardware = Hardware::stock();
    let clock = ActiveClock::one();
    let mut live = crate::day::apply::Live::preflight(&journal, &mut hardware).unwrap();
    live.reapply(&mut journal, &mut hardware, &clock).unwrap();
    let before = applications(&journal).last().unwrap().target.clone();
    let writes = hardware.writes.len();
    let mut state = Pause::of(&journal);
    assert!(
        state
            .control(
                &mut journal,
                &mut hardware,
                &clock,
                &mut live,
                Control::Freeze {
                    frozen: true,
                    uid: 1000
                }
            )
            .unwrap()
            .frozen
    );
    assert_eq!(hardware.writes.len(), writes);
    assert_eq!(applications(&journal).last().unwrap().target, before);
    assert!(
        !state
            .control(
                &mut journal,
                &mut hardware,
                &clock,
                &mut live,
                Control::Freeze {
                    frozen: false,
                    uid: 1000
                }
            )
            .unwrap()
            .frozen
    );
}

#[test]
fn paused_cold_boot_takes_the_required_retreat_and_resume_never_promotes() {
    let mut journal = seed();
    let proposal = proposed(&mut journal);
    let historical = target(&journal, -5);
    let previous_boot = journal.boot().clone();
    journal
        .append(
            Stamp {
                time: Timestamp::UNIX_EPOCH,
                mono: Some(Duration::ZERO),
            },
            DayApplied {
                application: ApplicationId(0),
                cause: ApplyCause::Step {
                    decision: DecisionId(0),
                },
                target: historical,
                readbacks: offsets(-5),
                transactions: Vec::new(),
                boot: previous_boot.clone(),
                mono_ns: 0,
            }
            .into(),
            &[proposal],
        )
        .unwrap();
    append(
        &mut journal,
        &ActiveClock::one(),
        DayUncleanEnd {
            boot: previous_boot.clone(),
            last_checkpoint: None,
            offsets: offsets(-5),
            reset_reason: ResetReason {
                kind: None,
                raw: Vec::new(),
                supported: false,
            },
            evidence: togi_machine::EndEvidence::unknown(),
        },
    )
    .unwrap();
    append(
        &mut journal,
        &ActiveClock::one(),
        togi_journal::DayAnswer {
            boot: previous_boot,
            offsets: offsets(-5),
            answer: togi_journal::DayAnswerKind::Frozen,
            uid: 1000,
        },
    )
    .unwrap();
    paused(
        &mut journal,
        PauseState::Until {
            deadline: deadline(),
        },
    );
    journal.reboot("day-b".to_owned().into()).unwrap();
    let mut hardware = Hardware::stock();
    let mut clock = ActiveClock::one();
    serve(
        &mut journal,
        &mut hardware,
        &mut clock,
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    assert_eq!(
        Pause::of(&journal).status().pause,
        PauseState::Until {
            deadline: deadline()
        }
    );
    let boot_steps: Vec<_> = journal
        .events()
        .iter()
        .filter_map(|event| {
            if let Payload::DayStep(step) = &event.payload {
                (step.cold_boot == *journal.boot()).then_some(&step.step)
            } else {
                None
            }
        })
        .collect();
    assert!(
        boot_steps
            .iter()
            .any(|step| matches!(step, StepKind::Retreat { .. }))
    );
    assert!(
        !boot_steps
            .iter()
            .any(|step| matches!(step, StepKind::Promote { .. }))
    );
    let steps = boot_steps.len();
    let mut live = crate::day::apply::Live::preflight(&journal, &mut hardware).unwrap();
    Pause::of(&journal)
        .control(
            &mut journal,
            &mut hardware,
            &clock,
            &mut live,
            Control::Resume { uid: 1000 },
        )
        .unwrap();
    assert_eq!(applications(&journal).last().unwrap().readbacks, offsets(0));
    assert_eq!(journal.events().iter().filter(|event| matches!(&event.payload, Payload::DayStep(step) if step.cold_boot == *journal.boot())).count(), steps);
}
