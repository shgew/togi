#[cfg(test)]
mod properties;

use std::fs;
use std::panic;
use std::path::{Path, PathBuf};

use togi_journal::EventReader;

fn seed_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/reader_fuzz/seeds")
}

#[test]
fn every_fuzz_seed_keeps_the_reader_properties() {
    let dir = seed_dir();
    let mut seeds = 0;
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let data = fs::read(&path).unwrap();
        let kept = panic::catch_unwind(|| properties::check(&data));
        assert!(
            kept.is_ok(),
            "seed {} breaks a reader property",
            path.display()
        );
        seeds += 1;
    }
    assert!(seeds > 0, "no seeds in {}", dir.display());
}

#[test]
fn the_ruleset13_seed_is_read_strictly_in_full() {
    let data = fs::read(seed_dir().join("ruleset13")).unwrap();
    let mut reader = EventReader::new(data.as_slice());
    let mut count = 0;
    for event in reader.by_ref() {
        let event = event.unwrap();
        count += 1;
        assert_eq!(event.seq.get(), count);
    }
    assert_eq!(count, 16);
    assert_eq!(usize::try_from(reader.accepted()).unwrap(), data.len());
}
