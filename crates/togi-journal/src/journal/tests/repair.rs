use super::{hex_of, sample};
use crate::fs::{Fault, MemFs};
use crate::testing::{boot, kinds, live, note, reopen, simulated, stamp};
use crate::{EventKind, OpenError, Payload, Recording};

fn torn_events(journal: &crate::Journal) -> Vec<&crate::JournalTorn> {
    journal
        .events()
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::JournalTorn(torn) => Some(torn),
            _ => None,
        })
        .collect()
}

#[test]
fn every_byte_truncation_is_complete_lines_plus_a_tail() {
    let bytes = sample(&MemFs::new());
    let ends: Vec<usize> =
        bytes.iter().enumerate().filter(|(_, byte)| **byte == b'\n').map(|(index, _)| index + 1).collect();
    for cut in 0..=bytes.len() {
        let prefix = &bytes[..cut];
        let complete = ends.iter().filter(|end| **end <= cut).count();
        let boundary = ends.iter().rev().find(|end| **end <= cut).copied().unwrap_or(0);

        let recording = Recording::parse(prefix).unwrap();
        assert_eq!(recording.events().len(), complete, "cut {cut}");
        match recording.torn() {
            None => assert_eq!(boundary, cut, "cut {cut}"),
            Some(torn) => {
                assert_eq!(torn.offset, boundary as u64, "cut {cut}");
                assert_eq!(torn.bytes, &prefix[boundary..], "cut {cut}");
            }
        }

        let fs = MemFs::new();
        fs.put("/state/events.jsonl", prefix);
        let mut journal = simulated(&fs).open(boot(2)).unwrap();
        if boundary == cut {
            assert_eq!(journal.events().len(), complete, "cut {cut}");
            assert_eq!(journal.repaired(), None, "cut {cut}");
            assert_eq!(live(&fs), prefix, "cut {cut}: a clean journal is not rewritten");
        } else if complete == 0 {
            assert!(journal.events().is_empty(), "cut {cut}");
            assert_eq!(journal.repaired(), None, "cut {cut}: a torn first line records nothing");
            assert!(live(&fs).is_empty(), "cut {cut}");
            let first = journal.append(stamp(0), crate::testing::start(crate::testing::default_session()), &[]);
            assert_eq!(first.unwrap().seq.get(), 1, "cut {cut}");
        } else {
            assert_eq!(journal.events().len(), complete + 1, "cut {cut}");
            let torn = journal.repaired().unwrap();
            assert_eq!(torn.get(), complete as u64 + 1, "cut {cut}");
            let record = &torn_events(&journal)[0];
            assert_eq!(record.offset, boundary as u64, "cut {cut}");
            assert_eq!(record.bytes_hex, hex_of(&prefix[boundary..]), "cut {cut}");
            assert_eq!(&live(&fs)[..boundary], &prefix[..boundary], "cut {cut}: the complete prefix is kept");
            assert!(Recording::parse(&live(&fs)).unwrap().torn().is_none(), "cut {cut}");
        }
    }
}

fn torn_sample() -> (Vec<u8>, usize) {
    let bytes = sample(&MemFs::new());
    let complete = bytes.len();
    let mut torn = bytes;
    torn.extend_from_slice(b"{\"seq\":5,\"tim");
    (torn, complete)
}

#[test]
fn a_repair_is_recorded_once_across_reopens() {
    let (bytes, complete) = torn_sample();
    let fs = MemFs::new();
    fs.put("/state/events.jsonl", bytes.clone());
    let journal = reopen(&fs, 2);
    assert_eq!(journal.repaired().map(crate::Seq::get), Some(5));
    assert_eq!(torn_events(&journal)[0].offset, complete as u64);
    assert_eq!(torn_events(&journal)[0].bytes_hex, hex_of(&bytes[complete..]));
    journal.close().unwrap();
    for boot_n in 3..6 {
        let journal = reopen(&fs, boot_n);
        assert_eq!(journal.repaired(), None);
        assert_eq!(torn_events(&journal).len(), 1);
        assert_eq!(journal.events().len(), 5);
    }
}

#[test]
fn the_repair_event_carries_the_opening_boot_and_time() {
    let (bytes, _) = torn_sample();
    let fs = MemFs::new();
    fs.put("/state/events.jsonl", bytes);
    let now = jiff::Timestamp::from_second(1_800_000_000).unwrap();
    let journal = simulated(&fs).open_at(boot(9), now).unwrap();
    let record = journal.events().last().unwrap();
    assert_eq!((record.boot.clone(), record.time), (boot(9), now));
}

#[test]
fn every_repair_step_failure_keeps_the_evidence_and_records_it_exactly_once() {
    let (bytes, complete) = torn_sample();
    for nth in 1..=8 {
        for fault in [Fault::any_before(nth), Fault::any_after(nth)] {
            let fs = MemFs::new();
            fs.put("/state/events.jsonl", bytes.clone());
            let dir = crate::testing::simulated(&fs);
            fs.inject(fault.clone());
            let first = dir.open(boot(2));
            fs.clear_faults();
            if let Err(error) = &first {
                assert!(
                    matches!(error, OpenError::Repair { .. } | OpenError::Dir(_)),
                    "{fault:?}: {error}"
                );
                let on_disk = live(&fs);
                assert!(
                    on_disk == bytes || Recording::parse(&on_disk).unwrap().torn().is_none(),
                    "{fault:?}: the file is the original or the finished repair"
                );
            }
            drop(first);
            let journal = reopen(&fs, 3);
            let mut expected = vec!["session.start", "session.warning", "session.warning", "session.warning"];
            expected.push("journal.torn");
            assert_eq!(kinds(journal.events()), expected, "{fault:?}");
            let records = torn_events(&journal);
            assert_eq!(records.len(), 1, "{fault:?}");
            assert_eq!(records[0].offset, complete as u64, "{fault:?}");
            assert_eq!(records[0].bytes_hex, hex_of(&bytes[complete..]), "{fault:?}");
            assert_eq!(&live(&fs)[..complete], &bytes[..complete], "{fault:?}");
        }
    }
}

#[test]
fn a_stale_temporary_file_is_replaced_by_the_repair() {
    let (bytes, _) = torn_sample();
    let fs = MemFs::new();
    fs.put("/state/events.jsonl", bytes);
    fs.put("/state/.events.jsonl.tmp", "stale garbage that is not a journal");
    let journal = reopen(&fs, 2);
    assert_eq!(journal.events().len(), 5);
    assert!(fs.contents("/state/.events.jsonl.tmp").is_none());
    assert!(Recording::parse(&live(&fs)).unwrap().torn().is_none());
}

#[test]
fn a_stale_temporary_file_never_stands_in_for_the_journal() {
    let bytes = sample(&MemFs::new());
    let fs = MemFs::new();
    fs.put("/state/events.jsonl", bytes.clone());
    fs.put("/state/.events.jsonl.tmp", "stale garbage");
    let journal = reopen(&fs, 2);
    assert_eq!(journal.events().len(), 4);
    assert_eq!(live(&fs), bytes);
}

#[test]
fn a_torn_first_line_is_truncated_and_the_next_seq_is_one() {
    let fs = MemFs::new();
    fs.put("/state/events.jsonl", "{\"seq\":1,\"ti");
    let mut journal = reopen(&fs, 2);
    assert!(journal.events().is_empty());
    assert!(live(&fs).is_empty());
    let first = journal.append(stamp(0), crate::testing::start(crate::testing::default_session()), &[]);
    assert_eq!(first.unwrap().seq.get(), 1);
}

#[test]
fn a_torn_first_line_survives_every_truncation_step_failure() {
    for fault in [
        Fault::before(crate::fs::Op::SetLen, 1),
        Fault::after(crate::fs::Op::SetLen, 1),
        Fault::before(crate::fs::Op::Sync, 1),
        Fault::after(crate::fs::Op::Sync, 1),
    ] {
        let fs = MemFs::new();
        fs.put("/state/events.jsonl", "{\"seq\":1,\"ti");
        let dir = crate::testing::simulated(&fs);
        fs.inject(fault.clone());
        assert!(dir.open(boot(2)).is_err(), "{fault:?}");
        fs.clear_faults();
        let mut journal = reopen(&fs, 3);
        assert!(journal.events().is_empty(), "{fault:?}");
        let first = journal.append(stamp(0), crate::testing::start(crate::testing::default_session()), &[]);
        assert_eq!(first.unwrap().seq.get(), 1, "{fault:?}");
    }
}

#[test]
fn a_complete_line_that_does_not_decode_is_corruption_and_nothing_is_rewritten() {
    let mut bytes = sample(&MemFs::new());
    bytes.extend_from_slice(b"{\"not\":\"an event\"}\ntrailing");
    let fs = MemFs::new();
    fs.put("/state/events.jsonl", bytes.clone());
    assert!(matches!(simulated(&fs).open(boot(2)), Err(OpenError::Read(_))));
    assert_eq!(live(&fs), bytes);
}

#[test]
fn a_gap_a_wrong_first_event_and_a_forward_cause_are_refused() {
    let good = sample(&MemFs::new());
    let lines: Vec<&[u8]> = good.split_inclusive(|byte| *byte == b'\n').collect();

    let mut gap = lines[0].to_vec();
    gap.extend_from_slice(lines[2]);
    let headless: Vec<u8> = lines[1..].concat();
    let forward = String::from_utf8(good.clone()).unwrap().replacen("\"cause\":[1]", "\"cause\":[2]", 1);

    for (name, bytes) in [("gap", gap), ("headless", headless), ("forward cause", forward.into_bytes())] {
        let fs = MemFs::new();
        fs.put("/state/events.jsonl", bytes.clone());
        assert!(matches!(simulated(&fs).open(boot(2)), Err(OpenError::Read(_))), "{name}");
        assert_eq!(live(&fs), bytes, "{name}");
    }
}

#[test]
fn an_unknown_kind_is_refused_before_any_repair() {
    let mut bytes = sample(&MemFs::new());
    let line = String::from_utf8(bytes.split_inclusive(|byte| *byte == b'\n').nth(1).unwrap().to_vec())
        .unwrap()
        .replace("session.warning", "invented.kind")
        .replace("\"seq\":2", "\"seq\":5");
    bytes.extend_from_slice(line.as_bytes());
    bytes.extend_from_slice(b"torn");
    let fs = MemFs::new();
    fs.put("/state/events.jsonl", bytes.clone());
    assert!(matches!(simulated(&fs).open(boot(2)), Err(OpenError::Read(_))));
    assert_eq!(live(&fs), bytes);
}

#[test]
fn an_appended_event_after_repair_is_durable_and_contiguous() {
    let (bytes, _) = torn_sample();
    let fs = MemFs::new();
    fs.put("/state/events.jsonl", bytes);
    let mut journal = reopen(&fs, 2);
    journal.append(stamp(9), note("after repair"), &[]).unwrap();
    let recording = Recording::read_in(&fs, &crate::testing::path("events.jsonl")).unwrap();
    assert_eq!(recording.events().last().unwrap().seq.get(), 6);
    assert_eq!(recording.events()[4].kind(), EventKind::JournalTorn);
}
