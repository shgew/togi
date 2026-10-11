mod archive;
mod repair;

use std::fmt::Write as _;

use togi_machine::TargetOffset;

use super::*;
use crate::fs::{Fault, MemFs, Op};
use crate::payload::HuntStart;
use crate::testing::{
    begin_default, boot, default_session, kinds, live, note, reopen, simulated, stamp, start,
    try_simulated,
};
use crate::{LockError, Recording};
use togi_machine::{CoreSet, PerCore, Regime};

pub(super) fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

pub(crate) fn sample(fs: &MemFs) -> Vec<u8> {
    let mut journal = begin_default(fs);
    journal
        .append(stamp(1), note("first"), &[Seq::FIRST])
        .unwrap();
    journal
        .append(
            stamp(2),
            note("second, with a longer message to cross lines"),
            &[],
        )
        .unwrap();
    journal
        .append(
            stamp(3),
            note("third"),
            &[Seq::new(2).unwrap(), Seq::new(3).unwrap()],
        )
        .unwrap();
    journal.close().unwrap();
    live(fs)
}

fn command() -> SmuCommand {
    SmuCommand::SetAll {
        offset: TargetOffset::ZERO,
    }
}

fn nan_payload() -> Payload {
    Payload::HuntStart(HuntStart {
        hunt: std::num::NonZeroU32::MIN,
        failure: Seq::FIRST,
        trial: None,
        regime: Regime::R2,
        workload: "mprime-sse-4k-21k".parse().unwrap(),
        cores: CoreSet::default(),
        duration_s: 1,
        failing: PerCore::default(),
        parked: PerCore::default(),
        parked_seq: None,
        candidates: CoreSet::default(),
        trials: 1,
        trial_s: 1,
        miss: f64::NAN,
        rate: 0.0,
        ranking: Vec::new(),
        reason: None,
    })
}

#[test]
fn events_are_published_in_order_and_found_by_seq() {
    let fs = MemFs::new();
    let mut journal = begin_default(&fs);
    let second = journal.append(stamp(1), note("a"), &[Seq::FIRST]).unwrap();
    assert_eq!(second.seq.get(), 2);
    assert_eq!(second.cause, [Seq::FIRST]);
    assert_eq!(journal.events().len(), 2);
    assert_eq!(
        journal.event(Seq::new(2).unwrap()).unwrap().kind().as_str(),
        "session.warning"
    );
    assert!(journal.event(Seq::new(3).unwrap()).is_none());
    assert_eq!(journal.session(), Some(default_session()));
    assert_eq!(journal.repaired(), None);
}

#[test]
fn an_event_is_one_write_then_one_fsync() {
    let fs = MemFs::new();
    let mut journal = begin_default(&fs);
    let before = fs.log().len();
    journal.append(stamp(1), note("a"), &[]).unwrap();
    let ops: Vec<Op> = fs.log()[before..].iter().map(|(op, _)| *op).collect();
    assert_eq!(ops, [Op::Write, Op::Sync]);
}

#[test]
fn events_survive_close_and_reopen_in_order() {
    let fs = MemFs::new();
    let bytes = sample(&fs);
    assert_eq!(bytes.split_inclusive(|byte| *byte == b'\n').count(), 4);
    let journal = reopen(&fs, 2);
    assert_eq!(
        kinds(journal.events()),
        [
            "session.start",
            "session.warning",
            "session.warning",
            "session.warning"
        ]
    );
    assert_eq!(journal.repaired(), None);
}

#[test]
fn the_first_event_must_be_session_start_and_consumes_no_seq() {
    let fs = MemFs::new();
    let mut journal = simulated(&fs).open(boot(1)).unwrap();
    let mutations = fs.mutations();
    assert!(matches!(
        journal.append(stamp(0), note("x"), &[]),
        Err(AppendError::FirstEvent { .. })
    ));
    assert_eq!(fs.mutations(), mutations);
    let first = journal
        .append(stamp(0), start(default_session()), &[])
        .unwrap();
    assert_eq!(first.seq, Seq::FIRST);
}

#[test]
fn a_cause_must_precede_its_event_and_a_refusal_changes_nothing() {
    let fs = MemFs::new();
    let mut journal = begin_default(&fs);
    let mutations = fs.mutations();
    for cause in [2, 3, 99] {
        let error = journal
            .append(stamp(1), note("x"), &[Seq::new(cause).unwrap()])
            .unwrap_err();
        assert!(matches!(error, AppendError::Cause { .. }));
    }
    assert_eq!(fs.mutations(), mutations);
    assert_eq!(
        journal
            .append(stamp(1), note("x"), &[Seq::FIRST])
            .unwrap()
            .seq
            .get(),
        2
    );
}

#[test]
fn an_event_that_would_not_read_back_neither_poisons_nor_consumes_a_seq() {
    let fs = MemFs::new();
    let mut journal = begin_default(&fs);
    let mutations = fs.mutations();
    assert!(matches!(
        journal.append(stamp(1), nan_payload(), &[]),
        Err(AppendError::Unreadable(_))
    ));
    assert_eq!(fs.mutations(), mutations);
    assert_eq!(
        journal
            .append(stamp(1), note("fine"), &[])
            .unwrap()
            .seq
            .get(),
        2
    );
}

#[test]
fn generic_append_refuses_the_smu_records() {
    let fs = MemFs::new();
    let mut journal = begin_default(&fs);
    let mutations = fs.mutations();
    let intent = Payload::SmuIntent(crate::SmuIntentRecord::new(command()));
    let write = Payload::SmuWrite(crate::SmuWriteRecord::new(command()));
    assert!(matches!(
        journal.append(stamp(1), intent, &[]),
        Err(AppendError::Refused(EventKind::SmuIntent))
    ));
    assert!(matches!(
        journal.append(stamp(1), write, &[]),
        Err(AppendError::Refused(EventKind::SmuWrite))
    ));
    assert_eq!(fs.mutations(), mutations);
    assert_eq!(journal.events().len(), 1);
}

#[test]
fn a_receipt_follows_a_durable_intent_and_the_write_cites_it() {
    let fs = MemFs::new();
    let mut journal = begin_default(&fs);
    let receipt = journal
        .append_smu_intent(stamp(1), command(), &[Seq::FIRST])
        .unwrap();
    assert_eq!(receipt.intent().get(), 2);
    assert_eq!(receipt.command(), command());
    let on_disk = Recording::read_in(&fs, &crate::testing::path("events.jsonl")).unwrap();
    assert_eq!(on_disk.events()[1].kind(), EventKind::SmuIntent);
    let written = journal.append_smu_write(stamp(2), receipt).unwrap();
    assert_eq!((written.seq().get(), written.intent().get()), (3, 2));
    assert_eq!(journal.events()[2].cause, [Seq::new(2).unwrap()]);
    assert_eq!(journal.events()[2].kind(), EventKind::SmuWrite);
}

#[test]
fn no_receipt_comes_from_an_intent_that_was_not_made_durable() {
    for fault in [
        Fault::before(Op::Write, 1),
        Fault::short_write(1),
        Fault::before(Op::Sync, 1),
    ] {
        let fs = MemFs::new();
        let mut journal = begin_default(&fs);
        fs.inject(fault);
        assert!(journal.append_smu_intent(stamp(1), command(), &[]).is_err());
        assert_eq!(journal.events().len(), 1);
    }
}

#[test]
fn a_receipt_is_refused_by_another_journal_and_by_a_reopened_one() {
    let fs = MemFs::new();
    let mut journal = begin_default(&fs);
    let receipt = journal.append_smu_intent(stamp(1), command(), &[]).unwrap();

    let other_fs = MemFs::new();
    let mut other = begin_default(&other_fs);
    assert!(matches!(
        other.append_smu_write(stamp(2), receipt),
        Err(AppendError::ForeignReceipt)
    ));
    assert_eq!(other.events().len(), 1);

    let receipt = journal.append_smu_intent(stamp(3), command(), &[]).unwrap();
    drop(journal);
    let mut reopened = reopen(&fs, 2);
    assert!(matches!(
        reopened.append_smu_write(stamp(4), receipt),
        Err(AppendError::ForeignReceipt)
    ));
}

#[test]
fn a_write_or_fsync_failure_poisons_without_further_io_and_the_survivor_replays_once() {
    let cases = [
        (Fault::before(Op::Write, 1), 1, "write never happened"),
        (Fault::short_write(1), 1, "short write"),
        (
            Fault::after(Op::Write, 1),
            2,
            "write landed, error reported",
        ),
        (
            Fault::before(Op::Sync, 1),
            2,
            "sync failed after the write landed",
        ),
        (Fault::after(Op::Sync, 1), 2, "sync reported failure"),
    ];
    for (fault, survivors, why) in cases {
        let fs = MemFs::new();
        let mut journal = begin_default(&fs);
        fs.inject(fault);
        assert!(
            matches!(
                journal.append(stamp(1), note("lost?"), &[]),
                Err(AppendError::Failed(_))
            ),
            "{why}"
        );
        assert_eq!(journal.events().len(), 1, "{why}: nothing is published");
        let mutations = fs.mutations();
        for _ in 0..3 {
            assert!(matches!(
                journal.append(stamp(2), note("again"), &[]),
                Err(AppendError::Poisoned(_))
            ));
        }
        assert!(matches!(
            journal.append_smu_intent(stamp(2), command(), &[]),
            Err(AppendError::Poisoned(_))
        ));
        assert!(matches!(journal.flush(), Err(AppendError::Poisoned(_))));
        assert_eq!(
            fs.mutations(),
            mutations,
            "{why}: a poisoned journal does no I/O"
        );

        let dir = journal.close().unwrap();
        assert_eq!(fs.mutations(), mutations);
        let mut journal = dir.open(boot(2)).unwrap();
        let replayed = journal
            .events()
            .iter()
            .filter(|event| event.kind() == EventKind::SessionWarning)
            .count();
        assert_eq!(replayed, survivors - 1, "{why}");
        let torn = journal
            .events()
            .iter()
            .filter(|event| event.kind() == EventKind::JournalTorn)
            .count();
        assert_eq!(torn, usize::from(why == "short write"), "{why}");
        let next = journal
            .append(stamp(3), note("after"), &[])
            .unwrap()
            .seq
            .get();
        assert_eq!(
            next,
            journal.events().len() as u64,
            "{why}: seqs stay contiguous"
        );
        let bytes = journal.close().map(|_| live(&fs)).unwrap();
        assert!(Recording::parse(&bytes).unwrap().torn().is_none(), "{why}");
    }
}

#[test]
fn the_first_failure_is_reported_as_the_cause_of_every_later_refusal() {
    let fs = MemFs::new();
    let mut journal = begin_default(&fs);
    fs.inject(Fault::before(Op::Sync, 1));
    let Err(AppendError::Failed(first)) = journal.append(stamp(1), note("x"), &[]) else {
        panic!("expected the first failure");
    };
    let Err(AppendError::Poisoned(later)) = journal.append(stamp(1), note("x"), &[]) else {
        panic!("expected a poisoned refusal");
    };
    assert!(Arc::ptr_eq(&first, &later));
    assert_eq!(first.op, "sync");
}

#[test]
fn the_writer_lock_is_held_until_close_or_drop() {
    let fs = MemFs::new();
    let journal = begin_default(&fs);
    assert!(matches!(try_simulated(&fs), Err(LockError::Held { .. })));
    let dir = journal.close().unwrap();
    assert!(matches!(try_simulated(&fs), Err(LockError::Held { .. })));
    drop(dir);
    let journal = reopen(&fs, 2);
    drop(journal);
    drop(try_simulated(&fs).unwrap());
}

#[test]
fn a_failed_open_releases_the_lock_and_leaves_no_writer() {
    let fs = MemFs::new();
    fs.put("/state/events.jsonl", "not json\n");
    assert!(simulated(&fs).open(boot(1)).is_err());
    drop(try_simulated(&fs).unwrap());
}

#[test]
fn a_failed_close_still_releases_the_lock() {
    let fs = MemFs::new();
    let journal = begin_default(&fs);
    fs.inject(Fault::before(Op::Sync, 1));
    assert!(journal.close().is_err());
    drop(try_simulated(&fs).unwrap());
}

#[test]
fn state_is_published_through_the_journal_and_survives_a_reopen() {
    let fs = MemFs::new();
    let mut journal = begin_default(&fs);
    journal.write_state(&vec![1, 2, 3]).unwrap();
    drop(journal);
    let journal = reopen(&fs, 2);
    assert_eq!(
        journal.read_state::<Vec<i32>>().unwrap(),
        Some(vec![1, 2, 3])
    );
}

#[test]
fn a_state_failure_is_not_a_journal_failure() {
    let fs = MemFs::new();
    let mut journal = begin_default(&fs);
    fs.fail_state_publication();
    assert!(journal.write_state(&1).is_err());
    assert!(journal.append(stamp(1), note("still fine"), &[]).is_ok());
}

#[test]
fn boot_reasons_are_found_live_and_in_archives_by_complete_lines_only() {
    let fs = MemFs::new();
    let mut journal = begin_default(&fs);
    let leave = |id: &str| {
        Payload::BootLeaveReason(crate::BootLeaveReason {
            reason_id: id.into(),
            restart_limit_count: 1,
            reason: "why".into(),
        })
    };
    journal.append(stamp(1), leave("r1"), &[]).unwrap();
    assert!(journal.boot_reason_recorded("r1").unwrap());
    assert!(!journal.boot_reason_recorded("r2").unwrap());
    let dir = journal
        .archive(stamp(2), ArchiveKind::Transition, &[])
        .unwrap();
    assert!(dir.boot_reason_recorded("r1").unwrap());
    assert!(!dir.boot_reason_recorded("r2").unwrap());

    let mut next = begin_default_at(&fs, dir, "20231115T000000Z");
    next.append(stamp(5), leave("r2"), &[]).unwrap();
    let bytes = live(&fs);
    drop(next);
    let torn_ack = &bytes[..bytes.len() - 5];
    fs.put("/state/events.jsonl", torn_ack);
    let dir = simulated(&fs);
    assert!(!dir.boot_reason_recorded("r2").unwrap());
    assert!(dir.boot_reason_recorded("r1").unwrap());
}

fn begin_default_at(_fs: &MemFs, dir: StateDir, session: &str) -> Journal {
    let mut journal = dir.open(boot(3)).unwrap();
    journal
        .append(stamp(4), start(crate::testing::session(session)), &[])
        .unwrap();
    journal
}

#[test]
fn an_unreadable_archive_is_reported_not_taken_for_absence() {
    let fs = MemFs::new();
    let dir = simulated(&fs);
    fs.put("/state/archive/20231114T221320Z.jsonl", "garbage\n");
    assert!(matches!(
        dir.boot_reason_recorded("r"),
        Err(crate::LookupError::File(_))
    ));
}

fn end_with(median: Option<f64>) -> Payload {
    let mut end: crate::TrialEnd =
        serde_json::from_str(r#"{"trial":"0001","outcome":"pass","duration_s":1}"#).unwrap();
    end.voltage_request_median_v = median;
    Payload::TrialEnd(end)
}

#[test]
fn a_payload_that_reads_back_different_is_refused_without_poison_or_seq() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let fs = MemFs::new();
        let mut journal = begin_default(&fs);
        let mutations = fs.mutations();
        let refused = journal.append(stamp(1), end_with(Some(value)), &[]);
        assert!(matches!(refused, Err(AppendError::Lossy)), "{value}");
        assert_eq!(fs.mutations(), mutations);
        assert_eq!(
            journal
                .append(stamp(2), end_with(Some(0.5)), &[])
                .unwrap()
                .seq
                .get(),
            2
        );
    }
}
