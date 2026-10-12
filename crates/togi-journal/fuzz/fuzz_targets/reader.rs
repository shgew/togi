#![no_main]

use std::path::Path;

use libfuzzer_sys::fuzz_target;
use togi_journal::{EventReader, MemFs, Recording};

fuzz_target!(|data: &[u8]| {
    let mut reader = EventReader::new(data);
    let mut count = 0u64;
    let mut failed = false;
    for event in reader.by_ref() {
        match event {
            Ok(event) => {
                count += 1;
                assert_eq!(event.seq.get(), count);
            }
            Err(_) => {
                failed = true;
                break;
            }
        }
    }
    let accepted = usize::try_from(reader.accepted()).unwrap();
    assert!(accepted <= data.len());
    assert!(accepted == 0 || data[accepted - 1] == b'\n');
    if !failed {
        let tail = &data[accepted..];
        assert_eq!(reader.torn().unwrap_or_default(), tail);
        assert!(!tail.contains(&b'\n'));
    }

    let mut lenient = EventReader::lenient(data);
    let mut last = 0;
    for event in lenient.by_ref() {
        let Ok(event) = event else { break };
        assert!(event.seq.get() > last);
        last = event.seq.get();
    }
    assert!(lenient.accepted() >= reader.accepted());

    let fs = MemFs::new();
    fs.put("/journal.jsonl", data);
    match Recording::read_in(&fs, Path::new("/journal.jsonl")) {
        Ok(recording) => {
            assert!(!failed);
            assert_eq!(recording.events().len() as u64, count);
            for (index, event) in recording.events().iter().enumerate() {
                assert_eq!(event.seq.get(), index as u64 + 1);
                assert_eq!(recording.event(event.seq), Some(event));
            }
            match recording.torn() {
                Some(torn) => {
                    assert_eq!(torn.offset, accepted as u64);
                    assert_eq!(torn.bytes, &data[accepted..]);
                }
                None => assert_eq!(accepted, data.len()),
            }
        }
        Err(_) => assert!(failed),
    }
});
