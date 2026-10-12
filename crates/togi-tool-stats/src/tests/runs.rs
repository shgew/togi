use togi_journal::{Outcome, ShutdownReason};
use togi_machine::Regime;

use super::support::{Log, config_loaded, crash_detected, intent, shutdown, trial};
use crate::project::Projection;
use crate::report::compute;

fn endings(log: &Log) -> Vec<String> {
    let journal = log.journal();
    Projection::new(&journal)
        .runs
        .iter()
        .map(|run| run.ending.clone())
        .collect()
}

#[test]
fn a_crash_reboot_continues_the_run_until_a_shutdown() {
    let mut log = Log::new(&[(0, 0)]);
    log.add(config_loaded(super::support::build("test", None)));
    log.boot("b");
    log.add(config_loaded(super::support::build("test", None)));
    log.add(crash_detected("a", None));
    log.boot("c");
    log.add(config_loaded(super::support::build("test", None)));
    log.add(shutdown(ShutdownReason::Signal));
    assert_eq!(endings(&log), ["signal"]);
}

#[test]
fn a_shutdown_splits_the_boots_into_runs() {
    let mut log = Log::new(&[(0, 0)]);
    log.add(config_loaded(super::support::build("test", None)));
    log.add(shutdown(ShutdownReason::Signal));
    log.boot("b");
    log.add(config_loaded(super::support::build("test", None)));
    log.add(shutdown(ShutdownReason::Command));
    assert_eq!(endings(&log), ["signal", "command"]);
}

#[test]
fn a_restart_within_a_boot_starts_a_run_that_stays_open() {
    let mut log = Log::new(&[(0, 0)]);
    log.add(config_loaded(super::support::build("test", None)));
    log.add(config_loaded(super::support::build("test", None)));
    assert_eq!(endings(&log), ["open", "open"]);
}

#[test]
fn a_run_counts_its_trials_and_crashes_and_names_its_builds() {
    let mut log = Log::new(&[(0, 0)]);
    log.add(config_loaded(super::support::build("1.2.3", Some("abc"))));
    trial(&mut log, intent("1", Regime::R1, &[0], &[0]), Outcome::Pass);
    log.add(crash_detected("a", None));
    let journal = log.journal();
    let p = Projection::new(&journal);
    assert_eq!(p.runs.len(), 1);
    assert_eq!((p.runs[0].trials, p.runs[0].crashes), (1, 1));
    assert_eq!(
        p.runs[0].builds.iter().collect::<Vec<_>>(),
        [(&"1.2.3+abc".to_owned(), &1)]
    );
    assert_eq!(compute(&p, None).runs.len(), 1);
}

#[test]
fn a_cutoff_keeps_the_runs_that_end_at_or_after_it() {
    let mut log = Log::new(&[(0, 0)]);
    log.at(0)
        .add(config_loaded(super::support::build("test", None)));
    log.at(10).add(shutdown(ShutdownReason::Signal));
    log.at(20)
        .add(config_loaded(super::support::build("test", None)));
    log.at(30).add(shutdown(ShutdownReason::Command));
    let journal = log.journal();
    let p = Projection::new(&journal);
    let at = |seconds| Some(super::support::EPOCH + jiff::SignedDuration::from_secs(seconds));
    let numbers = |since| {
        compute(&p, since)
            .runs
            .iter()
            .map(|(number, _)| *number)
            .collect::<Vec<_>>()
    };
    assert_eq!(numbers(None), [1, 2]);
    assert_eq!(numbers(at(10)), [1, 2]);
    assert_eq!(numbers(at(11)), [2]);
    assert_eq!(numbers(at(31)), Vec::<usize>::new());
}
