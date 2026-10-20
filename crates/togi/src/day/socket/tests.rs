use super::*;
use jiff::Timestamp;
use std::io::Cursor;
use test_case::test_case;
use togi_machine::{CancellationToken, EndEvidence, PerCore, ResetReason};

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
            offsets: PerCore::default(),
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

#[test_case(b"{\"request\":\"answer\",\"boot\":\"crashed\",\"answer\":\"frozen\",\"uid\":0}\n"; "claimed identity refused")]
#[test_case(b"{\"request\":\"answer\",\"boot\":\"crashed\",\"answer\":\"other\"}\n"; "unknown answer refused")]
#[test_case(b"{\"request\":\"questions\"}"; "unterminated request refused")]
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
fn answers_are_durable_associated_with_profile_and_can_be_changed() {
    let mut journal = crate::day::tests::journal();
    let boot = pending(&mut journal);
    assert_eq!(DayQuestions::of(journal.events()).pending().count(), 1);
    let reply = respond(
        &mut journal,
        &FixedClock,
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
    respond(
        &mut journal,
        &FixedClock,
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
    let reply = respond(
        &mut journal,
        &FixedClock,
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
