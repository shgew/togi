#![no_main]

#[path = "../../tests/reader_fuzz/properties.rs"]
mod properties;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| properties::check(data));
