use togi_journal::{Payload, Recording};
use togi_watchtest::{Scenario, install};

use crate::support::lines;

#[test]
fn an_installed_scenario_reads_back_through_the_journal_reader() {
    for scenario in [
        Scenario::Search,
        Scenario::Recovering,
        Scenario::Deadend,
        Scenario::Stopped,
    ] {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let build = install(dir.path(), scenario).expect("the scenario installs");
        let recording = Recording::read(&dir.path().join("events.jsonl")).expect("it reads back");
        assert!(recording.torn().is_none(), "{scenario}");
        let want = scenario.events().expect("the scenario builds");
        assert_eq!(lines(recording.events()), lines(&want), "{scenario}");
        let Payload::SessionStart(start) = &recording.events()[0].payload else {
            panic!("a journal opens with session.start");
        };
        assert_eq!(start.build, build, "{scenario}");
        assert_eq!(build, togi_watchtest::build(), "{scenario}");
    }
}

#[test]
fn an_installed_multi_boot_scenario_keeps_every_boot_and_smu_event() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    install(dir.path(), Scenario::Recovering).expect("the scenario installs");
    let recording = Recording::read(&dir.path().join("events.jsonl")).expect("it reads back");
    let mut boots: Vec<_> = recording
        .events()
        .iter()
        .map(|event| event.boot.clone())
        .collect();
    boots.dedup();
    assert!(boots.len() >= 2, "{boots:?}");
    assert!(
        recording
            .events()
            .iter()
            .any(|event| matches!(event.payload, Payload::SmuWrite(_)))
    );
}
