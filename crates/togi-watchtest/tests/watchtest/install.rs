use std::path::Path;
use std::sync::Arc;

use togi_journal::fs::Fs;
use togi_journal::{MemFs, Payload, Recording};
use togi_watchtest::{Error, InstallError, Scenario, install_with_fs};

use crate::support::lines;

#[test]
fn an_installed_scenario_reads_back_through_the_journal_reader() {
    for scenario in [
        Scenario::Search,
        Scenario::Recovering,
        Scenario::Deadend,
        Scenario::Stopped,
    ] {
        let fs = MemFs::new();
        let dir = Path::new("/session");
        let build =
            install_with_fs(Arc::new(fs.clone()), dir, scenario).expect("the scenario installs");
        let recording = Recording::read_in(&fs, &dir.join("events.jsonl")).expect("it reads back");
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
    let fs = MemFs::new();
    let dir = Path::new("/session");
    install_with_fs(Arc::new(fs.clone()), dir, Scenario::Recovering)
        .expect("the scenario installs");
    let recording = Recording::read_in(&fs, &dir.join("events.jsonl")).expect("it reads back");
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

#[test]
fn installing_where_no_state_directory_can_be_made_is_an_error() {
    let fs = MemFs::new();
    let file = Path::new("/file");
    fs.create_new(file).expect("a file");
    let error = install_with_fs(Arc::new(fs), file, Scenario::Confirm)
        .expect_err("a file is not a directory");
    assert!(matches!(error, Error::Install { dir, .. } if dir == file));
}

#[test]
fn installing_over_a_journal_is_refused_and_leaves_it_as_it_was() {
    let fs = MemFs::new();
    let dir = Path::new("/session");
    install_with_fs(Arc::new(fs.clone()), dir, Scenario::Confirm).expect("the scenario installs");
    let journal = dir.join("events.jsonl");
    let before = fs.contents(&journal).expect("the journal reads");
    let error = install_with_fs(Arc::new(fs.clone()), dir, Scenario::Confirm)
        .expect_err("a journal is already there");
    assert!(
        matches!(&error, Error::Install { dir: named, source: InstallError::NotEmpty } if named == dir),
        "{error}"
    );
    assert_eq!(fs.contents(&journal).expect("the journal reads"), before);
}
