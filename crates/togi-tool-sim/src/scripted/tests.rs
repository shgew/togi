use std::fmt::Write as _;

use sha2::{Digest as _, Sha256};

use super::CONTRACT_VERSION;

/// The files that define the named cases' inputs: the case set and boot bounds, each case's outcomes and milestones,
/// the staged day the profile case runs, and the scripted topology, register reset and BIOS context. How the
/// simulator turns a scripted outcome into a crash, and the session itself, are what `same` compares, not inputs.
const INPUTS: [(&str, &[u8]); 5] = [
    (
        "togi-tool-sim/src/scripted.rs",
        include_bytes!("../scripted.rs"),
    ),
    (
        "togi-tool-sim/src/bin/togi-tool-sim/scripted.rs",
        include_bytes!("../bin/togi-tool-sim/scripted.rs"),
    ),
    (
        "togi-simrun/src/scripted.rs",
        include_bytes!("../../../togi-simrun/src/scripted.rs"),
    ),
    (
        "togi-simrun/src/staged_day.rs",
        include_bytes!("../../../togi-simrun/src/staged_day.rs"),
    ),
    (
        "togi-sim/src/source.rs",
        include_bytes!("../../../togi-sim/src/source.rs"),
    ),
];

/// The contract version and the digest of [`INPUTS`] it was declared for.
const PINNED: (&str, &str) = (
    "1",
    "8f09ea9b96f7fea9d742fc6eb184a6da36ae37f6d8722714e4567c927c2fe748",
);

#[test]
fn the_contract_version_is_pinned_to_the_files_that_define_the_scripted_inputs() {
    let mut hash = Sha256::new();
    for (path, bytes) in INPUTS {
        hash.update(path.as_bytes());
        hash.update(b"\0");
        hash.update(u64::try_from(bytes.len()).unwrap().to_le_bytes());
        hash.update(bytes);
    }
    let digest = hash
        .finalize()
        .iter()
        .fold(String::new(), |mut text, byte| {
            write!(text, "{byte:02x}").unwrap();
            text
        });
    assert_eq!(
        (CONTRACT_VERSION, digest.as_str()),
        PINNED,
        "the scripted inputs changed: if a case, its topology, clock, boot bound or faults changed, raise \
         crates/togi-tool-sim/scripted-input-version so older bases are refused; then pin the new version and digest"
    );
}
