#[cfg(test)]
mod properties;

use std::fs;
use std::panic;
use std::path::Path;

#[test]
fn every_fuzz_seed_keeps_the_reader_properties() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/reader_fuzz/seeds");
    let mut seeds = 0;
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let data = fs::read(&path).unwrap();
        let kept = panic::catch_unwind(|| properties::check(&data));
        assert!(kept.is_ok(), "seed {} breaks a reader property", path.display());
        seeds += 1;
    }
    assert!(seeds > 0, "no seeds in {}", dir.display());
}
