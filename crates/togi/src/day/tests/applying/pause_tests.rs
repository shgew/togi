use super::*;
use crate::day::socket::{self, FreezeRequest, PauseRequest, Reply, Request};
use crate::day::{pause::Pause, socket::Control};
use std::cell::Cell;
use test_case::test_case;
use togi_journal::{DayPause, IntervalState, PauseSource, PauseState};

fn started(journal: &mut Journal) {
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

fn paused(journal: &mut Journal, state: PauseState) {
    if !journal.events().iter().any(|event| {
        matches!(&event.payload, Payload::DayBootStart(_)) && event.boot == *journal.boot()
    }) {
        started(journal);
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

/// A clock the test moves by hand, shared with the closures that run during a control.
struct Dial {
    active: Cell<Duration>,
    wall: Cell<Duration>,
}

impl Dial {
    fn at(seconds: u64) -> Self {
        Self {
            active: Cell::new(Duration::from_secs(seconds)),
            wall: Cell::new(Duration::from_secs(seconds)),
        }
    }

    fn set(&self, seconds: u64) {
        self.active.set(Duration::from_secs(seconds));
        self.wall.set(Duration::from_secs(seconds));
    }
}

impl Clock for Dial {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH.checked_add(self.wall.get()).unwrap()
    }
    fn monotonic(&self) -> Result<Duration, MachineError> {
        Ok(self.active.get())
    }
    fn sleep(&mut self, _: &CancellationToken, _: Duration) -> Result<(), MachineError> {
        panic!("a control must not wait")
    }
}

#[test]
fn a_deadline_that_passes_during_the_checkpoint_is_refused_without_a_pause_or_write() {
    let mut journal = seed();
    started(&mut journal);
    let mut hardware = Hardware::stock();
    let mut live = crate::day::apply::Live::preflight(&journal, &mut hardware).unwrap();
    let mut state = Pause::of(&journal);
    let dial = Dial::at(0);
    let before = journal.events().len();
    let reply = socket::respond(
        &mut journal,
        &dial,
        1000,
        Request::Pause(PauseRequest {
            state: PauseState::Until {
                deadline: Timestamp::UNIX_EPOCH + Duration::from_secs(60),
            },
        }),
        &mut |journal, clock, control| {
            // The socket saw a future deadline; the clock reaches it before the pause is recorded.
            dial.set(60);
            state.control(journal, &mut hardware, clock, &mut live, control)
        },
    )
    .unwrap();
    assert!(matches!(reply, Reply::Refused { .. }), "{reply:?}");
    assert_eq!(journal.events().len(), before);
    assert!(hardware.writes.is_empty());
    assert_eq!(Pause::of(&journal).status().pause, PauseState::Running {});
}

#[test]
fn an_extension_adds_one_hour_to_the_pause_its_revision_names() {
    let mut journal = seed();
    paused(
        &mut journal,
        PauseState::Until {
            deadline: deadline(),
        },
    );
    let mut hardware = Hardware::stock();
    let mut live = crate::day::apply::Live::preflight(&journal, &mut hardware).unwrap();
    let mut state = Pause::of(&journal);
    let revision = state.status().revision.unwrap();
    let status = state
        .control(
            &mut journal,
            &mut hardware,
            &ActiveClock::one(),
            &mut live,
            Control::Extend {
                revision,
                uid: 1000,
            },
        )
        .unwrap();
    let extended = deadline().checked_add(Duration::from_hours(1)).unwrap();
    assert_eq!(status.pause, PauseState::Until { deadline: extended });
    assert_ne!(status.revision, Some(revision));
    assert_eq!(hardware.writes, [TargetOffset::ZERO]);
}

#[test_case(Control::Resume { uid: 1001 }, PauseState::Running {}; "resumed meanwhile")]
#[test_case(Control::Pause { state: PauseState::UntilBoot {}, uid: 1001 }, PauseState::UntilBoot {}; "untimed pause meanwhile")]
#[test_case(
    Control::Pause { state: PauseState::Until { deadline: Timestamp::UNIX_EPOCH + Duration::from_hours(5) }, uid: 1001 },
    PauseState::Until { deadline: Timestamp::UNIX_EPOCH + Duration::from_hours(5) };
    "replaced meanwhile"
)]
fn an_extension_for_a_superseded_pause_is_refused_and_leaves_the_newer_state(
    meanwhile: Control,
    expected: PauseState,
) {
    let mut journal = seed();
    paused(
        &mut journal,
        PauseState::Until {
            deadline: deadline(),
        },
    );
    let mut hardware = Hardware::stock();
    let mut live = crate::day::apply::Live::preflight(&journal, &mut hardware).unwrap();
    let mut state = Pause::of(&journal);
    let revision = state.status().revision.unwrap();
    let clock = ActiveClock::one();
    state
        .control(&mut journal, &mut hardware, &clock, &mut live, meanwhile)
        .unwrap();
    let events = journal.events().len();
    let writes = hardware.writes.len();
    let error = state
        .control(
            &mut journal,
            &mut hardware,
            &clock,
            &mut live,
            Control::Extend {
                revision,
                uid: 1000,
            },
        )
        .unwrap_err();
    assert!(matches!(error, Error::PauseChanged), "{error:?}");
    assert_eq!(journal.events().len(), events);
    assert_eq!(hardware.writes.len(), writes);
    assert_eq!(state.status().pause, expected);
    assert_eq!(Pause::of(&journal).status().pause, expected);
}

#[test]
fn an_extension_after_the_deadline_never_resurrects_the_pause() {
    let mut journal = seed();
    paused(
        &mut journal,
        PauseState::Until {
            deadline: deadline(),
        },
    );
    let mut hardware = Hardware::stock();
    let mut live = crate::day::apply::Live::preflight(&journal, &mut hardware).unwrap();
    let mut state = Pause::of(&journal);
    let revision = state.status().revision.unwrap();
    let events = journal.events().len();
    let error = state
        .control(
            &mut journal,
            &mut hardware,
            &Dial::at(3601),
            &mut live,
            Control::Extend {
                revision,
                uid: 1000,
            },
        )
        .unwrap_err();
    assert!(matches!(error, Error::PauseChanged), "{error:?}");
    assert_eq!(journal.events().len(), events);
    assert!(hardware.writes.is_empty());
}

#[test]
fn an_untimed_pause_cannot_be_extended() {
    let mut journal = seed();
    paused(&mut journal, PauseState::UntilBoot {});
    let mut hardware = Hardware::stock();
    let mut live = crate::day::apply::Live::preflight(&journal, &mut hardware).unwrap();
    let mut state = Pause::of(&journal);
    let revision = state.status().revision.unwrap();
    let error = state
        .control(
            &mut journal,
            &mut hardware,
            &ActiveClock::one(),
            &mut live,
            Control::Extend {
                revision,
                uid: 1000,
            },
        )
        .unwrap_err();
    assert!(matches!(error, Error::PauseNotTimed), "{error:?}");
    assert!(hardware.writes.is_empty());
}

fn position(events: &[Event], found: fn(&Payload) -> bool) -> usize {
    events
        .iter()
        .position(|event| found(&event.payload))
        .unwrap()
}

#[test]
fn the_live_control_seam_brackets_a_pause_with_checkpoints_and_acknowledges_after_the_transaction()
{
    let mut journal = seed();
    started(&mut journal);
    let mut hardware = Hardware::stock();
    let mut live = crate::day::apply::Live::preflight(&journal, &mut hardware).unwrap();
    let mut state = Pause::of(&journal);
    let sampler = Sampler::new(session_cores(journal.events()).unwrap(), LoadMix::default());
    let mut progress = Progress {
        started: Duration::ZERO,
        accumulated: Duration::ZERO,
        checkpointed: Duration::ZERO,
        offsets: offsets(0),
        watchdog: armed(),
    };
    let dial = Dial::at(0);
    let mut send = |journal: &mut Journal, hardware: &mut Hardware, seconds: u64, request| {
        dial.set(seconds);
        let mut controls = Controls {
            progress: &mut progress,
            sampler: &sampler,
            observer: hardware,
            pause: &mut state,
            live: Some(&mut live),
        };
        let before = journal.events().len();
        let reply = socket::respond(
            journal,
            &dial,
            1000,
            request,
            &mut |journal, clock, control| controls.apply(journal, clock, control),
        )
        .unwrap();
        assert!(matches!(reply, Reply::Controlled { .. }), "{reply:?}");
        journal.events()[before..].to_vec()
    };

    let paused = send(
        &mut journal,
        &mut hardware,
        600,
        Request::Pause(PauseRequest {
            state: PauseState::UntilBoot {},
        }),
    );
    let checkpoint = position(&paused, |payload| {
        matches!(payload, Payload::DayCheckpoint(_))
    });
    let pause = position(&paused, |payload| matches!(payload, Payload::DayPause(_)));
    let readback = paused
        .iter()
        .rposition(|event| matches!(event.payload, Payload::SmuReadback(_)))
        .unwrap();
    assert!(checkpoint < pause && pause < readback, "{paused:?}");
    assert_eq!(hardware.writes, [TargetOffset::ZERO]);

    let resumed = send(&mut journal, &mut hardware, 1200, Request::Resume);
    let before = position(&resumed, |payload| {
        matches!(payload, Payload::DayCheckpoint(_))
    });
    let applied = position(&resumed, |payload| {
        matches!(payload, Payload::DayApplied(_))
    });
    let ended = position(&resumed, |payload| {
        matches!(
            payload,
            Payload::DayPause(DayPause {
                state: PauseState::Running {},
                ..
            })
        )
    });
    let after = resumed
        .iter()
        .rposition(|event| matches!(event.payload, Payload::DayCheckpoint(_)))
        .unwrap();
    assert!(
        before < applied && applied < ended && ended < after,
        "{resumed:?}"
    );
    assert_eq!(hardware.writes, [TargetOffset::ZERO, TargetOffset::ZERO]);

    send(
        &mut journal,
        &mut hardware,
        1800,
        Request::Freeze(FreezeRequest { frozen: true }),
    );

    let mut intervals = DayIntervals::default();
    let closed: Vec<(bool, u64)> = journal
        .events()
        .iter()
        .filter_map(|event| {
            intervals.apply(event).map(|credit| match credit {
                DayIntervalCredit::Credited {
                    offsets: credited,
                    seconds,
                    ..
                } => {
                    assert_eq!(*credited, offsets(0));
                    (true, seconds)
                }
                DayIntervalCredit::Excluded { seconds, .. } => (false, seconds),
            })
        })
        .collect();
    // Running before the pause is credited; the paused span and the resume's closing checkpoint are
    // excluded; running after the resume is credited again.
    assert_eq!(closed, [(true, 600), (false, 600), (false, 0), (true, 600)]);
    assert_eq!(
        Ledger::of(journal.events()).profiles.values().sum::<u64>(),
        1200
    );
}
