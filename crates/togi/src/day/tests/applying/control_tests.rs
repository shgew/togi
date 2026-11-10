use super::*;
use crate::day::socket::{self, ControlSource, PauseRequest, Reply, Request};
use std::cell::RefCell;
use std::sync::mpsc::{self, Receiver, SyncSender};
use test_case::test_case;

enum Message {
    Request(Box<Request>, SyncSender<Reply>),
    Stop,
}

struct Source {
    inbox: Receiver<Message>,
    cancel: CancellationToken,
    transactions: RefCell<Vec<Vec<Event>>>,
}

impl ControlSource for Source {
    fn poll(
        &self,
        journal: &mut Journal,
        clock: &dyn Clock,
        controller: socket::Controller<'_>,
    ) -> Result<(), Error> {
        match self.inbox.recv_timeout(Duration::from_secs(5)).unwrap() {
            Message::Request(request, reply) => {
                let before = journal.events().len();
                let response = socket::respond(journal, clock, 0, *request, controller)?;
                self.transactions
                    .borrow_mut()
                    .push(journal.events()[before..].to_vec());
                reply.send(response).unwrap();
            }
            Message::Stop => self.cancel.cancel(),
        }
        Ok(())
    }
}

struct ControlClock(Duration);

impl Clock for ControlClock {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH + self.0
    }
    fn monotonic(&self) -> Result<Duration, MachineError> {
        Ok(self.0)
    }
    fn sleep(
        &mut self,
        cancel: &CancellationToken,
        duration: Duration,
    ) -> Result<(), MachineError> {
        if cancel.is_cancelled() {
            return Err(MachineError::Cancelled);
        }
        self.0 += duration;
        Ok(())
    }
}

#[test_case(false, Request::Reapply, -5; "sleep resume")]
#[test_case(true, Request::Resume, -5; "early pause resume")]
#[test_case(true, Request::Reapply, 0; "sleep while paused")]
fn threaded_controls_reapply_committed_d_without_promotion(
    paused: bool,
    request: Request,
    expected: i64,
) {
    let mut journal = seed();
    proposed_climb(&mut journal);
    let mut hardware = Hardware::stock();
    serve(
        &mut journal,
        &mut hardware,
        &mut ActiveClock::one(),
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    let decisions = journal
        .events()
        .iter()
        .filter(|event| matches!(event.payload, Payload::ClimbDecision(_)))
        .count();
    let (sender, inbox) = mpsc::sync_channel(1);
    let cancel = CancellationToken::new();
    let source = Source {
        inbox,
        cancel: cancel.clone(),
        transactions: RefCell::new(Vec::new()),
    };
    std::thread::scope(|scope| {
        let client = scope.spawn(move || {
            let send = |request| {
                let (reply, response) = mpsc::sync_channel(1);
                sender
                    .send(Message::Request(Box::new(request), reply))
                    .unwrap();
                response.recv_timeout(Duration::from_secs(5)).unwrap()
            };
            if paused {
                assert!(matches!(
                    send(Request::Pause(PauseRequest {
                        state: togi_journal::PauseState::UntilBoot {},
                    })),
                    Reply::Controlled { .. }
                ));
            }
            assert!(matches!(
                send(request),
                Reply::Controlled { .. } | Reply::Reapplied
            ));
            sender.send(Message::Stop).unwrap();
        });
        crate::day::serve_with_controls(
            &mut journal,
            &mut hardware,
            &mut ControlClock(Duration::from_secs(120)),
            &cancel,
            &current_build(),
            Some(&source),
        )
        .unwrap();
        client.join().unwrap();
    });
    let transactions = source.transactions.borrow();
    let last = transactions.last().unwrap();
    let readback = last
        .iter()
        .rev()
        .find_map(|event| {
            if let Payload::SmuReadback(value) = &event.payload {
                Some(value.offset.0)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(readback, expected);
    assert_eq!(
        journal
            .events()
            .iter()
            .filter(|event| matches!(event.payload, Payload::ClimbDecision(_)))
            .count(),
        decisions
    );
    if expected == -5 {
        let applied = last
            .iter()
            .position(|event| matches!(event.payload, Payload::DayApplied(_)))
            .unwrap();
        let read = last
            .iter()
            .rposition(|event| matches!(event.payload, Payload::SmuReadback(_)))
            .unwrap();
        assert!(read < applied);
        assert_eq!(
            applications(&journal).last().unwrap().cause,
            ApplyCause::Reapplication {}
        );
    } else {
        assert!(
            !last
                .iter()
                .any(|event| matches!(event.payload, Payload::DayApplied(_)))
        );
    }
    assert_eq!(hardware.current, 0);
    assert!(matches!(
        journal.events().last().unwrap().payload,
        Payload::DayCleanStop(_)
    ));
}

/// Hands the service one request and then stops it. A service that ends with an error never answers, so the
/// reply is not kept.
struct OneRequest {
    request: RefCell<Option<Request>>,
    cancel: CancellationToken,
}

impl ControlSource for OneRequest {
    fn poll(
        &self,
        journal: &mut Journal,
        clock: &dyn Clock,
        controller: socket::Controller<'_>,
    ) -> Result<(), Error> {
        let request = self.request.borrow_mut().take();
        if let Some(request) = request {
            socket::respond(journal, clock, 0, request, controller)?;
        } else {
            self.cancel.cancel();
        }
        Ok(())
    }
}

/// Serves a nonzero startup profile, then a pause request while the hardware drifts at read `drift_at`. Reads
/// number 1 the baseline check, 2 the boot-start read, 3 to 5 the startup transaction (stock readback, -5
/// readback, confirmation), 6 the startup guard, 7 the control's first read, 8 the pause's stock readback and 9
/// the control's second read.
fn pause_with_drift_at(drift_at: usize) -> (Journal, Hardware, Result<(), Error>) {
    let mut journal = seed();
    proposed_climb(&mut journal);
    let mut hardware = Hardware::stock();
    hardware.drift_at = Some(drift_at);
    let cancel = CancellationToken::new();
    let source = OneRequest {
        request: RefCell::new(Some(Request::Pause(PauseRequest {
            state: togi_journal::PauseState::UntilBoot {},
        }))),
        cancel: cancel.clone(),
    };
    let result = crate::day::serve_with_controls(
        &mut journal,
        &mut hardware,
        &mut ControlClock(Duration::from_secs(120)),
        &cancel,
        &current_build(),
        Some(&source),
    );
    (journal, hardware, result)
}

fn recorded_offsets(journal: &Journal) -> Vec<PerCore<ObservedOffset>> {
    journal
        .events()
        .iter()
        .filter_map(|event| {
            if let Payload::DayOffsetsChanged(change) = &event.payload {
                Some(change.offsets)
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn drift_after_a_pauses_verified_stock_records_that_profile_before_the_drift() {
    let (journal, hardware, result) = pause_with_drift_at(9);
    assert!(matches!(result, Err(Error::OffsetDrift)));
    assert_eq!(
        recorded_offsets(&journal),
        [offsets(-5), offsets(0), offsets(-12), offsets(0)]
    );
    let drift = journal
        .events()
        .iter()
        .position(|event| {
            matches!(&event.payload, Payload::DayOffsetsChanged(change) if change.offsets == offsets(-12))
        })
        .unwrap();
    assert_eq!(journal.events()[drift + 1].kind(), EventKind::DayCheckpoint);
    assert_eq!(journal.events()[drift + 2].kind(), EventKind::SmuIntent);
    assert_eq!(
        hardware.writes,
        [
            TargetOffset::ZERO,
            TargetOffset::new(-5).unwrap(),
            TargetOffset::ZERO,
            TargetOffset::ZERO,
        ]
    );
    assert_eq!(applications(&journal).len(), 1);
    assert!(
        !journal
            .events()
            .iter()
            .any(|event| matches!(event.payload, Payload::DayCleanStop(_)))
    );
}

#[test]
fn drift_before_a_control_acts_stops_the_service_without_running_it() {
    let (journal, hardware, result) = pause_with_drift_at(7);
    assert!(matches!(result, Err(Error::OffsetDrift)));
    assert!(
        !journal
            .events()
            .iter()
            .any(|event| matches!(event.payload, Payload::DayPause(_)))
    );
    assert_eq!(
        recorded_offsets(&journal),
        [offsets(-5), offsets(-12), offsets(0)]
    );
    assert_eq!(
        hardware.writes,
        [
            TargetOffset::ZERO,
            TargetOffset::new(-5).unwrap(),
            TargetOffset::ZERO,
        ]
    );
    assert_eq!(applications(&journal).len(), 1);
    assert!(
        !journal
            .events()
            .iter()
            .any(|event| matches!(event.payload, Payload::DayCleanStop(_)))
    );
}
