use super::*;
use jiff::Timestamp;
use std::io::Cursor;
use test_case::test_case;
use togi_journal::{CrashedBoot, OffsetSource, Running};
use togi_machine::{CancellationToken, CoreId, EndEvidence, ObservedOffset, PerCore, ResetReason};
use togi_session::{Crash, Draft, Entered, OffsetSpec, draft_record};

struct FixedClock;
impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }
    fn monotonic(&self) -> Result<Duration, togi_machine::Error> {
        Ok(Duration::ZERO)
    }
    fn sleep(&mut self, _: &CancellationToken, _: Duration) -> Result<(), togi_machine::Error> {
        panic!("answering must not wait")
    }
}

fn pending(journal: &mut Journal) -> BootId {
    let boot = BootId::from("crashed-boot".to_owned());
    super::super::append(
        journal,
        &FixedClock,
        DayUncleanEnd {
            boot: boot.clone(),
            last_checkpoint: None,
            offsets: [(0, -12), (7, -3), (15, -30)].into_iter().fold(
                PerCore::default(),
                |mut offsets, (core, offset)| {
                    offsets.set(CoreId::new(core).unwrap(), ObservedOffset(offset));
                    offsets
                },
            ),
            reset_reason: ResetReason {
                kind: None,
                raw: Vec::new(),
                supported: false,
            },
            evidence: EndEvidence::unknown(),
        },
    )
    .unwrap();
    boot
}

fn serve(journal: &mut Journal, uid: u32, request: Request) -> Result<Reply, super::super::Error> {
    respond(journal, &FixedClock, uid, request, &mut |_, _, _| {
        panic!("this request must not control the day")
    })
}

fn recordable(journal: &mut Journal) -> DayRecord {
    let boot = pending(journal);
    draft_record(
        journal.events(),
        &Draft {
            crash: Crash::Observed {
                boot,
                entered: Some(Entered {
                    at: Timestamp::from_second(500).unwrap(),
                    specs: vec![OffsetSpec::All(-20)],
                }),
            },
            running: Running::Build,
            note: Some("nix build".into()),
        },
    )
    .unwrap()
}

fn records(journal: &Journal) -> usize {
    journal
        .events()
        .iter()
        .filter(|event| event.kind() == togi_journal::EventKind::DayRecord)
        .count()
}

/// A record request for the session [`crate::day::tests::journal`] starts.
fn ours(record: DayRecord) -> Request {
    Request::Record {
        session: SessionId::new(Timestamp::UNIX_EPOCH, None).unwrap(),
        record,
    }
}

#[test_case(0, 100, true; "root")]
#[test_case(1000, 42, true; "primary group")]
#[test_case(1001, 100, true; "supplementary group")]
#[test_case(1002, 100, false; "unrelated peer refused")]
fn peer_credentials_must_authorize_the_kernel_identity(uid: u32, gid: u32, allowed: bool) {
    assert_eq!(
        authorized(
            uid,
            gid,
            42,
            "togi:x:42:alice\n",
            "alice:x:1001:100::/:/bin/sh\n"
        ),
        allowed
    );
}

#[test_case(1000, true)]
#[test_case(1001, true)]
#[test_case(1002, false)]
#[test_case(1003, false)]
fn supplementary_membership_resolves_to_uids_not_client_claims(uid: u32, allowed: bool) {
    assert_eq!(
        authorized(
            uid,
            100,
            42,
            "togi:x:42:alice,bob\nother:x:43:mallory\n",
            "alice:x:1000:100::/:/bin/sh\nbob:x:1001:100::/:/bin/sh\nmallory:x:1002:100::/:/bin/sh\n"
        ),
        allowed
    );
}

#[test]
fn changed_group_membership_is_used_for_the_next_connection() {
    let accounts = "alice:x:1000:100::/:/bin/sh\n";
    assert!(!authorized(1000, 100, 42, "togi:x:42:\n", accounts));
    assert!(authorized(1000, 100, 42, "togi:x:42:alice\n", accounts));
    assert!(!authorized(1000, 100, 42, "togi:x:42:\n", accounts));
}

#[test_case(b"{\"answer\":{\"boot\":\"crashed\",\"answer\":\"frozen\",\"uid\":0}}\n"; "claimed identity refused")]
#[test_case(b"{\"answer\":{\"boot\":\"crashed\",\"answer\":\"other\"}}\n"; "unknown answer refused")]
#[test_case(b"\"questions\""; "unterminated request refused")]
#[test_case(b"[]\n"; "nonobject refused")]
fn malformed_requests_are_refused(bytes: &[u8]) {
    assert!(read_request(&mut Cursor::new(bytes)).is_err());
}

#[test]
fn oversized_request_is_refused() {
    let bytes = vec![b' '; usize::try_from(LIMIT).unwrap() + 1];
    assert!(read_request(&mut Cursor::new(bytes)).is_err());
}

struct Trickle {
    reads: usize,
}
impl io::Read for Trickle {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reads += 1;
        buf[0] = b' ';
        Ok(1)
    }
}
impl Timeouts for Trickle {
    fn set_read_timeout(&self, _: Duration) -> io::Result<()> {
        Ok(())
    }
    fn set_write_timeout(&self, _: Duration) -> io::Result<()> {
        Ok(())
    }
}

/// Every look at the clock costs 300 ms, as if each byte of the trickle took that long.
struct SlowClock(std::cell::Cell<Duration>);
impl Clock for SlowClock {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }
    fn monotonic(&self) -> Result<Duration, togi_machine::Error> {
        let now = self.0.get();
        self.0.set(now + Duration::from_millis(300));
        Ok(now)
    }
    fn sleep(&mut self, _: &CancellationToken, _: Duration) -> Result<(), togi_machine::Error> {
        panic!("answering must not wait")
    }
}

#[test]
fn a_trickling_request_is_refused_within_the_deadline() {
    let clock = SlowClock(std::cell::Cell::default());
    let mut exchange = Deadline::new(Trickle { reads: 0 }, &clock, EXCHANGE).unwrap();
    let error = read_request(&mut exchange).unwrap_err();
    assert!(
        matches!(&error, Error::Io(io) if io.kind() == io::ErrorKind::TimedOut),
        "{error:?}"
    );
    assert_eq!(exchange.stream.reads, 3);
}

#[test]
fn a_client_decodes_the_pending_ends_the_service_lists() {
    let mut journal = crate::day::tests::journal();
    pending(&mut journal);
    let expected: Vec<_> = DayQuestions::of(journal.events())
        .pending()
        .cloned()
        .collect();
    let reply = serve(&mut journal, 1000, Request::Questions).unwrap();
    let mut wire = Vec::new();
    write_reply(&mut wire, &reply).unwrap();
    let Reply::Questions { pending, .. } = read_reply(&mut Cursor::new(wire)).unwrap() else {
        panic!("questions request must be answered with questions")
    };
    assert_eq!(pending, expected);
    assert_eq!(pending[0].offsets.len(), 3);
}

#[test]
fn the_morning_report_is_read_only_socket_data() {
    let mut journal = crate::day::tests::journal();
    let before = journal.events().len();
    let reply = serve(&mut journal, 1000, Request::Questions).unwrap();
    let Reply::Questions { morning, .. } = reply else {
        panic!("questions must return the read-only report")
    };
    assert_eq!(morning, None);
    assert_eq!(journal.events().len(), before);

    let report = crate::morning::Report {
        boot: "morning".to_owned().into(),
        text: "Day application is restricted to stock (CO 0).".into(),
    };
    let mut wire = Vec::new();
    write_reply(
        &mut wire,
        &Reply::Questions {
            pending: Vec::new(),
            morning: Some(report.clone()),
        },
    )
    .unwrap();
    let Reply::Questions { morning, .. } = read_reply(&mut Cursor::new(wire)).unwrap() else {
        panic!("questions reply must retain its report")
    };
    assert_eq!(morning, Some(report));
}

#[test_case(br#"{"questions":{"pending":[],"morning":["boot","text"]}}"#; "positional morning refused")]
#[test_case(br#"{"questions":{"pending":[],"morning":{"boot":"boot","text":"text","extra":true}}}"#; "unknown morning field refused")]
fn malformed_morning_reports_are_refused(bytes: &[u8]) {
    assert!(read_reply(&mut Cursor::new(bytes)).is_err());
}

#[test]
fn a_reply_without_a_morning_report_remains_compatible() {
    let reply = read_reply(&mut Cursor::new(br#"{"questions":{"pending":[]}}"#)).unwrap();
    let Reply::Questions { pending, morning } = reply else {
        panic!("questions reply must decode without a morning field")
    };
    assert_eq!(pending, Vec::new());
    assert_eq!(morning, None);
}

#[test]
fn answers_are_durable_associated_with_profile_and_can_be_changed() {
    let mut journal = crate::day::tests::journal();
    let boot = pending(&mut journal);
    assert_eq!(DayQuestions::of(journal.events()).pending().count(), 1);
    let reply = serve(
        &mut journal,
        1000,
        Request::Answer {
            boot: boot.clone(),
            answer: DayAnswerKind::Frozen,
        },
    )
    .unwrap();
    assert!(matches!(reply, Reply::Answered));
    let questions = DayQuestions::of(journal.events());
    assert_eq!(questions.pending().count(), 0);
    let answer = questions.answer(&boot).unwrap();
    assert_eq!(answer.answer, DayAnswerKind::Frozen);
    assert_eq!(answer.uid, 1000);
    assert_eq!(answer.offsets, questions.get(&boot).unwrap().offsets);
    serve(
        &mut journal,
        1001,
        Request::Answer {
            boot: boot.clone(),
            answer: DayAnswerKind::Graphics,
        },
    )
    .unwrap();
    let questions = DayQuestions::of(journal.events());
    assert_eq!(
        questions.answer(&boot).unwrap().answer,
        DayAnswerKind::Graphics
    );
    assert_eq!(questions.answer(&boot).unwrap().uid, 1001);
    assert_eq!(
        journal
            .events()
            .iter()
            .filter(|e| e.kind() == togi_journal::EventKind::DayAnswer)
            .count(),
        2
    );
}

#[test]
fn a_well_formed_answer_for_an_unknown_boot_is_refused_without_appending() {
    let mut journal = crate::day::tests::journal();
    let before = journal.events().len();
    let reply = serve(
        &mut journal,
        1000,
        Request::Answer {
            boot: "unknown".to_owned().into(),
            answer: DayAnswerKind::Unsure,
        },
    )
    .unwrap();
    assert!(matches!(reply, Reply::Refused { .. }));
    assert_eq!(journal.events().len(), before);
}

#[cfg(target_os = "linux")]
#[test]
fn real_fs_socket_credentials_come_from_the_kernel() {
    let (client, server) = UnixStream::pair().unwrap();
    let peer = std::thread::spawn(move || credentials(&server).unwrap());
    assert_eq!(
        peer.join().unwrap(),
        (
            rustix::process::geteuid().as_raw(),
            rustix::process::getegid().as_raw()
        )
    );
    drop(client);
}

#[test]
fn a_record_from_root_is_appended_to_the_live_journal() {
    let mut journal = crate::day::tests::journal();
    let record = recordable(&mut journal);
    let reply = serve(&mut journal, 0, ours(record.clone())).unwrap();
    assert!(matches!(reply, Reply::Recorded { .. }));
    assert_eq!(records(&journal), 1);
    let Some(togi_journal::Payload::DayRecord(appended)) =
        journal.events().last().map(|event| event.payload.clone())
    else {
        panic!("the record must be the last event")
    };
    assert_eq!(appended, record);
}

#[test]
fn a_second_record_of_the_same_boot_is_refused_by_the_service() {
    let mut journal = crate::day::tests::journal();
    let record = recordable(&mut journal);
    serve(&mut journal, 0, ours(record.clone())).unwrap();
    let reply = serve(&mut journal, 0, ours(record)).unwrap();
    let Reply::Refused { reason } = reply else {
        panic!("a duplicate record must be refused")
    };
    assert!(reason.contains("already has a crash record"), "{reason}");
    assert_eq!(records(&journal), 1);
}

#[test]
fn a_record_for_a_boot_without_an_unclean_end_is_refused() {
    let mut journal = crate::day::tests::journal();
    let record = DayRecord {
        boot: CrashedBoot::UncleanEnd {
            boot: "elsewhere".to_owned().into(),
        },
        ..recordable(&mut journal)
    };
    let reply = serve(&mut journal, 0, ours(record)).unwrap();
    let Reply::Refused { reason } = reply else {
        panic!("an unknown boot must be refused")
    };
    assert!(reason.contains("no recorded unclean end"), "{reason}");
    assert_eq!(records(&journal), 0);
}

#[test]
fn a_record_claiming_a_read_back_the_journal_lacks_is_refused() {
    let mut journal = crate::day::tests::journal();
    let record = DayRecord {
        source: OffsetSource::ReadBack {
            at: Timestamp::from_second(700).unwrap(),
        },
        ..recordable(&mut journal)
    };
    let reply = serve(&mut journal, 0, ours(record)).unwrap();
    assert!(matches!(reply, Reply::Refused { .. }));
    assert_eq!(records(&journal), 0);
}

#[test]
fn a_record_drafted_from_another_session_is_refused_without_appending() {
    let mut journal = crate::day::tests::journal();
    let record = recordable(&mut journal);
    let elsewhere = SessionId::new(Timestamp::from_second(86_400).unwrap(), None).unwrap();
    let reply = serve(
        &mut journal,
        0,
        Request::Record {
            session: elsewhere,
            record,
        },
    )
    .unwrap();
    let Reply::Refused { reason } = reply else {
        panic!("a record for another session must be refused")
    };
    assert!(
        reason.contains(&format!("not session {elsewhere}")),
        "{reason}"
    );
    assert_eq!(records(&journal), 0);
}

#[test]
fn a_record_from_the_answer_group_is_refused_without_appending() {
    let mut journal = crate::day::tests::journal();
    let record = recordable(&mut journal);
    let reply = serve(&mut journal, 1000, ours(record)).unwrap();
    let Reply::Refused { reason } = reply else {
        panic!("recording needs root")
    };
    assert!(reason.contains("needs root"), "{reason}");
    assert_eq!(records(&journal), 0);
}

#[test]
fn a_record_with_a_long_note_fits_one_request() {
    let mut journal = crate::day::tests::journal();
    let record = DayRecord {
        note: Some("n".repeat(8 * 1024)),
        ..recordable(&mut journal)
    };
    let line = serde_json::to_vec(&ours(record)).unwrap();
    assert!((line.len() as u64) < LIMIT, "{}", line.len());
}

#[test]
fn root_reapply_is_acknowledged_only_after_the_transaction_succeeds() {
    let mut journal = crate::day::tests::journal();
    let mut calls = 0;
    let reply = respond(
        &mut journal,
        &FixedClock,
        0,
        Request::Reapply,
        &mut |_, _, control| {
            assert_eq!(control, Control::Reapply);
            calls += 1;
            Ok(status())
        },
    )
    .unwrap();
    assert!(matches!(reply, Reply::Reapplied));
    assert_eq!(calls, 1);
}

#[test]
fn a_failed_reapply_is_never_acknowledged() {
    let mut journal = crate::day::tests::journal();
    let result = respond(
        &mut journal,
        &FixedClock,
        0,
        Request::Reapply,
        &mut |_, _, _| Err(super::super::Error::ClockReversed),
    );
    assert!(result.is_err());
}

#[test]
fn reapply_from_the_answer_group_is_refused_without_running_it() {
    let mut journal = crate::day::tests::journal();
    let reply = serve(&mut journal, 1000, Request::Reapply).unwrap();
    assert!(matches!(reply, Reply::Refused { .. }));
}

#[test]
fn real_fs_a_missing_socket_means_no_service_runs() {
    let dir = tempfile::tempdir().unwrap();
    let reached = request_at(&dir.path().join("day.sock"), &Request::Questions);
    assert!(matches!(reached, Err(Error::Absent(_))), "{reached:?}");
}

#[test]
fn real_fs_a_socket_nothing_listens_at_means_no_service_runs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("day.sock");
    // A service that stopped without removing its socket leaves the file behind.
    drop(UnixListener::bind(&path).unwrap());
    assert!(
        std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_socket()
    );
    let reached = request_at(&path, &Request::Questions);
    assert!(matches!(reached, Err(Error::Absent(_))), "{reached:?}");
}

#[test]
fn real_fs_a_socket_path_that_cannot_be_reached_is_an_error_not_absence() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file");
    std::fs::write(&file, b"").unwrap();
    let reached = request_at(&file.join("day.sock"), &Request::Questions);
    assert!(matches!(reached, Err(Error::Io(_))), "{reached:?}");
}

/// Root search permission bypasses directory modes, so a denied connect is pinned without the filesystem.
#[test]
fn a_denied_connect_is_an_error_not_absence() {
    let error = connect_error(io::Error::from(io::ErrorKind::PermissionDenied));
    assert!(matches!(error, Error::Io(_)), "{error:?}");
}

fn status() -> ControlStatus {
    ControlStatus {
        pause: PauseState::Running {},
        frozen: false,
        revision: None,
    }
}

#[test_case(Request::State, Control::Status; "state")]
#[test_case(Request::Pause(PauseRequest { state: PauseState::UntilBoot {} }), Control::Pause { state: PauseState::UntilBoot {}, uid: 1000 }; "untimed pause")]
#[test_case(Request::Pause(PauseRequest { state: PauseState::Until { deadline: Timestamp::UNIX_EPOCH + Duration::from_secs(1) } }), Control::Pause { state: PauseState::Until { deadline: Timestamp::UNIX_EPOCH + Duration::from_secs(1) }, uid: 1000 }; "timed pause")]
#[test_case(Request::Extend(ExtendRequest { revision: Seq::new(3).unwrap() }), Control::Extend { revision: Seq::new(3).unwrap(), uid: 1000 }; "extend")]
#[test_case(Request::Resume, Control::Resume { uid: 1000 }; "resume")]
#[test_case(Request::Freeze(FreezeRequest { frozen: true }), Control::Freeze { frozen: true, uid: 1000 }; "freeze")]
#[test_case(Request::Freeze(FreezeRequest { frozen: false }), Control::Freeze { frozen: false, uid: 1000 }; "unfreeze")]
fn group_controls_use_kernel_identity_and_return_verified_status(
    request: Request,
    expected: Control,
) {
    let mut journal = crate::day::tests::journal();
    let state = matches!(&request, Request::State);
    let reply = respond(
        &mut journal,
        &FixedClock,
        1000,
        request,
        &mut |_, _, control| {
            assert_eq!(control, expected);
            Ok(status())
        },
    )
    .unwrap();
    let actual = match reply {
        Reply::State { status } if state => status,
        Reply::Controlled { status } if !state => status,
        other @ (Reply::Questions { .. }
        | Reply::State { .. }
        | Reply::Controlled { .. }
        | Reply::Answered
        | Reply::Recorded { .. }
        | Reply::Reapplied
        | Reply::Refused { .. }) => panic!("unexpected control reply: {other:?}"),
    };
    assert_eq!(actual, status());
}

#[test]
fn a_failed_control_is_not_acknowledged() {
    let mut journal = crate::day::tests::journal();
    let result = respond(
        &mut journal,
        &FixedClock,
        1000,
        Request::Resume,
        &mut |_, _, _| Err(super::super::Error::ClockReversed),
    );
    assert!(result.is_err());
}

#[test]
fn control_status_round_trips_with_its_revision() {
    let expected = ControlStatus {
        pause: PauseState::Until {
            deadline: Timestamp::UNIX_EPOCH,
        },
        frozen: true,
        revision: Some(Seq::new(7).unwrap()),
    };
    let mut wire = Vec::new();
    write_reply(
        &mut wire,
        &Reply::State {
            status: expected.clone(),
        },
    )
    .unwrap();
    let Reply::State { status } = read_reply(&mut Cursor::new(wire)).unwrap() else {
        panic!("expected state reply")
    };
    assert_eq!(status, expected);
}

#[test_case(PauseState::Running {}; "resume cannot bypass transaction ordering")]
#[test_case(PauseState::Until { deadline: Timestamp::UNIX_EPOCH }; "deadline is now")]
#[test_case(PauseState::Until { deadline: Timestamp::UNIX_EPOCH - Duration::from_secs(1) }; "deadline expired")]
fn invalid_pause_states_are_refused_without_entering_the_controller(state: PauseState) {
    let mut journal = crate::day::tests::journal();
    let reply = serve(&mut journal, 1000, Request::Pause(PauseRequest { state })).unwrap();
    assert!(matches!(reply, Reply::Refused { .. }));
}

#[test_case(|| crate::day::Error::PauseDeadline, "a pause needs an unexpired deadline or the next cold boot"; "deadline passed while checkpointing")]
#[test_case(|| crate::day::Error::PauseChanged, "the pause changed before the extension reached the service"; "pause changed since the revision")]
#[test_case(|| crate::day::Error::PauseNotTimed, "only a timed pause can be extended"; "pause has no deadline")]
fn a_control_the_state_no_longer_allows_is_a_refusal_not_a_service_failure(
    refusal: fn() -> crate::day::Error,
    reason: &str,
) {
    let mut journal = crate::day::tests::journal();
    let reply = respond(
        &mut journal,
        &FixedClock,
        1000,
        Request::Extend(ExtendRequest {
            revision: Seq::new(1).unwrap(),
        }),
        &mut |_, _, _| Err(refusal()),
    )
    .unwrap();
    let Reply::Refused { reason: actual } = reply else {
        panic!("expected a refusal, got {reply:?}")
    };
    assert_eq!(actual, reason);
}

#[test_case(r#"{"pause":{"state":{"kind":"until_boot"}}}"#, r#"{"pause":[{"kind":"until_boot"}]}"#; "pause")]
#[test_case(r#"{"extend":{"revision":7}}"#, r#"{"extend":[7]}"#; "extend")]
#[test_case(r#"{"attest_stock":{"session":"20260101T000000Z"}}"#, r#"{"attest_stock":["20260101T000000Z"]}"#; "attest stock")]
#[test_case(r#"{"freeze":{"frozen":true}}"#, r#"{"freeze":[true]}"#; "freeze")]
fn control_requests_decode_from_objects_only(object: &str, array: &str) {
    serde_json::from_str::<Request>(object).unwrap();
    let decoded = serde_json::from_str::<Request>(array);
    assert!(decoded.is_err(), "{array} decoded as {decoded:?}");
}

#[test]
fn stock_attestation_requires_root_even_for_the_day_answer_group() {
    let mut journal = crate::day::tests::journal();
    let session = journal.session().unwrap();
    let reply = serve(
        &mut journal,
        1000,
        Request::AttestStock(AttestStockRequest { session }),
    )
    .unwrap();
    assert!(matches!(reply, Reply::Refused { .. }));
    assert_eq!(journal.events().len(), 1);
}

#[test]
fn stock_attestation_refuses_a_changed_session_before_reading_or_appending() {
    let mut journal = crate::day::tests::journal();
    let session = SessionId::new(Timestamp::UNIX_EPOCH + Duration::from_secs(1), None).unwrap();
    let reply = serve(
        &mut journal,
        0,
        Request::AttestStock(AttestStockRequest { session }),
    )
    .unwrap();
    assert!(matches!(reply, Reply::Refused { .. }));
    assert_eq!(journal.events().len(), 1);
}

#[test]
fn stock_attestation_failure_is_never_acknowledged() {
    let mut journal = crate::day::tests::journal();
    let session = journal.session().unwrap();
    let result = respond(
        &mut journal,
        &FixedClock,
        0,
        Request::AttestStock(AttestStockRequest { session }),
        &mut |_, _, action| {
            assert_eq!(action, Control::AttestStock);
            Err(super::super::Error::ClockReversed)
        },
    );
    assert!(result.is_err());
}
