use togi_journal::EventKind;
use togi_session::Stop;

use crate::common::{Bench, STATE, finish, hunt_machine, until_kind};

/// The `trial.start` lines of a fresh simulation with `seed`, run to its first trial's end, exactly as journaled.
fn trial_starts(seed: u64) -> Vec<String> {
    let bench = Bench::new(&hunt_machine(4), seed);
    let mut input = bench.input();
    input.observer = Some(until_kind(EventKind::TrialEnd));
    assert_eq!(finish(input).stop, Some(Stop::Signal));
    let journal = bench.fs.contents(format!("{STATE}/events.jsonl")).unwrap();
    String::from_utf8(journal)
        .unwrap()
        .lines()
        .filter(|line| line.contains(r#""kind":"trial.start""#))
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_same_seed_journals_byte_identical_trial_starts() {
    let first = trial_starts(1);
    assert!(!first.is_empty());
    assert_eq!(first, trial_starts(1));
    // Both runs share this test's process, so equality alone would not catch a controller pid read from it: the
    // simulator reports its fixed pid, never the process that runs it.
    for line in &first {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(
            event["data"]["controller"]["pid"],
            u64::from(togi_sim::CONTROLLER_PID),
            "{line}"
        );
    }
}
