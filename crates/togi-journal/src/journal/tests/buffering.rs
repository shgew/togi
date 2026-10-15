use super::*;

pub(super) fn begin(fs: &MemFs) -> Journal {
    let dir =
        StateDir::lock_in(Arc::new(fs.clone()), std::path::Path::new("/state"), true).unwrap();
    let mut journal = dir.open(boot(1)).unwrap();
    journal
        .append(stamp(0), start(default_session()), &[])
        .unwrap();
    journal
}

fn appended(journal: &Journal) -> Vec<(Seq, Payload)> {
    journal
        .events()
        .iter()
        .map(|event| (event.seq, event.payload.clone()))
        .collect()
}

fn written(fs: &MemFs) -> Vec<(Seq, Payload)> {
    Recording::parse(&live(fs))
        .unwrap()
        .events()
        .iter()
        .map(|event| (event.seq, event.payload.clone()))
        .collect()
}

#[test]
fn the_buffer_flushes_on_request_close_or_64_kib() {
    let fs = MemFs::new();
    let mut journal = begin(&fs);
    journal.append(stamp(1), note("small"), &[]).unwrap();
    assert!(live(&fs).is_empty());
    journal.flush().unwrap();
    assert_eq!(written(&fs), appended(&journal));
    assert_eq!(written(&fs).len(), 2);

    journal.append(stamp(2), note("buffered"), &[]).unwrap();
    assert_eq!(written(&fs), appended(&journal)[..2]);
    journal
        .append(stamp(3), note(&"x".repeat(70 * 1024)), &[])
        .unwrap();
    assert_eq!(written(&fs), appended(&journal));
    assert_eq!(written(&fs).len(), 4);

    journal.append(stamp(4), note("tail"), &[]).unwrap();
    let expected = appended(&journal);
    assert_eq!(written(&fs), expected[..4]);
    drop(journal.close().unwrap());
    assert_eq!(written(&fs), expected);
    assert_eq!(expected.len(), 5);
}

#[test]
fn a_buffered_journal_dropped_without_close_loses_only_what_was_buffered() {
    let fs = MemFs::new();
    let mut journal = begin(&fs);
    journal.append(stamp(1), note("flushed"), &[]).unwrap();
    journal.flush().unwrap();
    let flushed = appended(&journal);
    assert_eq!(flushed[1].1, note("flushed"));
    journal.append(stamp(2), note("lost"), &[]).unwrap();
    drop(journal);
    assert_eq!(written(&fs), flushed);
}

#[test]
fn a_buffered_smu_receipt_follows_the_intent_write() {
    let fs = MemFs::new();
    let mut journal = begin(&fs);
    journal.append(stamp(1), note("before"), &[]).unwrap();
    assert!(live(&fs).is_empty());
    let receipt = journal.append_smu_intent(stamp(2), command(), &[]).unwrap();
    let events = written(&fs);
    assert_eq!(events, appended(&journal));
    assert_eq!(events.len(), 3);
    let (seq, payload) = events.last().unwrap();
    assert_eq!(*seq, receipt.intent());
    assert!(matches!(payload, Payload::SmuIntent(_)));
}
