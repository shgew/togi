use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::tempdir;
use togi_journal::Seq;
use togi_tool_stats::{Journal, backoff_records, chain_records, counterfactual, report};

use crate::sim::{chain_session, hunting_session, write_journal};

fn stats(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_togi-tool-stats"))
        .args(args)
        .output()
        .unwrap()
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap()
}

fn os(text: &str) -> &std::ffi::OsStr {
    text.as_ref()
}

fn state_with_torn_tail() -> (tempfile::TempDir, String) {
    let events = hunting_session();
    let want = report(&Journal::new(events.clone()).unwrap(), None);
    let dir = tempdir().unwrap();
    write_journal(dir.path(), &events);
    let path = dir.path().join("events.jsonl");
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(b"{\"unfinished\":");
    fs::write(&path, bytes).unwrap();
    fs::write(dir.path().join("state.json"), "invalid state").unwrap();
    (dir, want)
}

#[test]
fn real_process_the_report_reads_only_the_journal_and_leaves_it_as_it_was() {
    let (dir, want) = state_with_torn_tail();
    let journal = dir.path().join("events.jsonl");
    let before = fs::read(&journal).unwrap();
    let absent = dir.path().join("absent");
    for args in [
        vec![os("--state-dir"), dir.path().as_os_str()],
        vec![
            os("--journal"),
            journal.as_os_str(),
            os("--state-dir"),
            absent.as_os_str(),
        ],
    ] {
        let ran = stats(&args);
        assert_eq!(ran.status.code(), Some(0));
        assert_eq!(text(ran.stderr), "");
        assert_eq!(text(ran.stdout), want);
    }
    assert_eq!(fs::read(&journal).unwrap(), before);
}

#[test]
fn real_process_chains_writes_the_chain_records_beside_the_unchanged_report() {
    let events = hunting_session();
    let journal = Journal::new(events.clone()).unwrap();
    let dir = tempdir().unwrap();
    write_journal(dir.path(), &events);
    let out = dir.path().join("chains.jsonl");
    let ran = stats(&[
        os("--state-dir"),
        dir.path().as_os_str(),
        os("--chains"),
        out.as_os_str(),
    ]);
    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(text(ran.stderr), "");
    assert_eq!(text(ran.stdout), report(&journal, None));
    let written = fs::read_to_string(&out).unwrap();
    assert_eq!(written, chain_records(&journal, None).unwrap());
    let derived = events
        .iter()
        .filter(|event| matches!(event.payload, togi_journal::Payload::CheckingChain(_)))
        .count();
    assert!(derived > 0, "the session derived no chain");
    assert_eq!(written.lines().count(), derived);

    let unwritable = dir.path().join("absent").join("chains.jsonl");
    let ran = stats(&[
        os("--state-dir"),
        dir.path().as_os_str(),
        os("--chains"),
        unwritable.as_os_str(),
    ]);
    assert_eq!(ran.status.code(), Some(1));
    assert_eq!(text(ran.stdout), "");
    let stderr = text(ran.stderr);
    assert!(
        stderr.starts_with("togi-tool-stats: write chain records "),
        "{stderr}"
    );
}

#[test]
fn real_process_backoffs_writes_the_failure_records_beside_the_unchanged_report() {
    let events = chain_session();
    let journal = Journal::new(events.clone()).unwrap();
    let dir = tempdir().unwrap();
    write_journal(dir.path(), &events);
    let out = dir.path().join("backoffs.jsonl");
    let ran = stats(&[
        os("--state-dir"),
        dir.path().as_os_str(),
        os("--backoffs"),
        out.as_os_str(),
    ]);
    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(text(ran.stderr), "");
    assert_eq!(text(ran.stdout), report(&journal, None));
    let written = fs::read_to_string(&out).unwrap();
    assert_eq!(written, backoff_records(&journal, None).unwrap());
    assert!(written.contains("\"backoffs\":[{"), "{written}");

    let unwritable = dir.path().join("absent").join("backoffs.jsonl");
    let ran = stats(&[
        os("--state-dir"),
        dir.path().as_os_str(),
        os("--backoffs"),
        unwritable.as_os_str(),
    ]);
    assert_eq!(ran.status.code(), Some(1));
    assert_eq!(text(ran.stdout), "");
    let stderr = text(ran.stderr);
    assert!(
        stderr.starts_with("togi-tool-stats: write backoff records "),
        "{stderr}"
    );
}

#[test]
fn real_process_counterfactual_prints_the_accounting_and_writes_its_records() {
    let events = chain_session();
    let want = counterfactual(&Journal::new(events.clone()).unwrap()).unwrap();
    let dir = tempdir().unwrap();
    write_journal(dir.path(), &events);
    let out = dir.path().join("witnesses.jsonl");
    let ran = stats(&[
        os("--state-dir"),
        dir.path().as_os_str(),
        os("--counterfactual"),
        os("--witnesses"),
        out.as_os_str(),
    ]);
    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(text(ran.stderr), "");
    assert_eq!(text(ran.stdout), want.text);
    assert_eq!(fs::read_to_string(&out).unwrap(), want.records);
    assert_eq!(
        want.records
            .lines()
            .filter(|line| line.starts_with(r#"{"record":"summary""#))
            .count(),
        3
    );

    let unwritable = dir.path().join("absent").join("witnesses.jsonl");
    let ran = stats(&[
        os("--state-dir"),
        dir.path().as_os_str(),
        os("--counterfactual"),
        os("--witnesses"),
        unwritable.as_os_str(),
    ]);
    assert_eq!(ran.status.code(), Some(1));
    assert_eq!(text(ran.stdout), "");
    let stderr = text(ran.stderr);
    assert!(
        stderr.starts_with("togi-tool-stats: write counterfactual records "),
        "{stderr}"
    );
}

#[test]
fn real_process_since_keeps_what_starts_at_or_after_the_cutoff() {
    let (dir, _) = state_with_torn_tail();
    let ran = stats(&[
        os("--state-dir"),
        dir.path().as_os_str(),
        os("--since"),
        os("2100-01-01T00:00:00Z"),
    ]);
    assert_eq!(ran.status.code(), Some(0));
    let out = text(ran.stdout);
    assert!(out.contains("since     2100-01-01T00:00:00Z\n"), "{out}");
    assert!(out.contains("Hunts\nhunt  start"), "{out}");
    let hunts = out.split("\nHunts\n").nth(1).unwrap();
    assert_eq!(
        hunts.split("\n\n").next().unwrap().lines().count(),
        1,
        "{out}"
    );
}

#[test]
fn real_process_rejected_arguments_exit_two_without_a_report() {
    let dir = tempdir().unwrap();
    for args in [
        vec![os("unexpected")],
        vec![os("--since"), os("not-a-time")],
        vec![os("--unknown")],
        vec![
            os("--forecast"),
            os("forecast.json"),
            os("--journal"),
            os("events.jsonl"),
        ],
        vec![
            os("--forecast"),
            os("forecast.json"),
            os("--since"),
            os("2026-10-07T00:00:00Z"),
        ],
        vec![
            os("--forecast"),
            os("forecast.json"),
            os("--chains"),
            os("chains.jsonl"),
        ],
        vec![
            os("--forecast"),
            os("forecast.json"),
            os("--backoffs"),
            os("backoffs.jsonl"),
        ],
        vec![
            os("--counterfactual"),
            os("--since"),
            os("2026-10-07T00:00:00Z"),
        ],
        vec![os("--counterfactual"), os("--chains"), os("chains.jsonl")],
        vec![os("--witnesses"), os("witnesses.jsonl")],
        vec![
            os("--counterfactual"),
            os("--backoffs"),
            os("backoffs.jsonl"),
        ],
    ] {
        let mut full = vec![os("--state-dir"), dir.path().as_os_str()];
        full.extend(args.iter().copied());
        let ran = stats(&full);
        assert_eq!(ran.status.code(), Some(2), "{args:?}");
        assert_eq!(text(ran.stdout), "", "{args:?}");
        assert!(!text(ran.stderr).is_empty(), "{args:?}");
    }
}

#[test]
fn real_process_an_unreadable_journal_exits_one_naming_the_tool_and_the_file() {
    let dir = tempdir().unwrap();
    let ran = stats(&[os("--state-dir"), dir.path().as_os_str()]);
    assert_eq!(ran.status.code(), Some(1));
    assert_eq!(text(ran.stdout), "");
    let stderr = text(ran.stderr);
    assert!(stderr.starts_with("togi-tool-stats: "), "{stderr}");
    assert!(stderr.contains("events.jsonl"), "{stderr}");
}

#[test]
fn real_process_a_journal_whose_events_skip_a_number_is_refused() {
    let dir = tempdir().unwrap();
    let events = hunting_session();
    // The journal header, then an event that skips seq 2.
    let mut kept = vec![events[0].clone()];
    kept.extend_from_slice(&events[2..]);
    write_journal(dir.path(), &kept);
    let ran = stats(&[os("--state-dir"), dir.path().as_os_str()]);
    assert_eq!(ran.status.code(), Some(1));
    let stderr = text(ran.stderr);
    assert!(stderr.contains("events.jsonl"), "{stderr}");
    assert!(stderr.contains("expected seq 2, found 3"), "{stderr}");
}

#[test]
fn real_process_a_journal_whose_first_line_is_not_a_journal_header_is_refused() {
    let dir = tempdir().unwrap();
    let events = hunting_session();
    write_journal(dir.path(), &events[1..]);
    let ran = stats(&[os("--state-dir"), dir.path().as_os_str()]);
    assert_eq!(ran.status.code(), Some(1));
    let stderr = text(ran.stderr);
    assert!(stderr.contains("events.jsonl"), "{stderr}");
    assert!(
        stderr.contains("first line is not a complete header of this build"),
        "{stderr}"
    );
}

#[test]
fn real_process_an_event_of_an_unknown_kind_is_skipped_and_named_on_stderr_and_the_report_is_still_written()
 {
    let dir = tempdir().unwrap();
    let mut events = hunting_session();
    let mut last = events.pop().unwrap();
    write_journal(dir.path(), &events);
    let at = last.seq.get();
    let unknown = format!(
        "{{\"seq\":{at},\"time\":\"{}\",\"boot\":\"{}\",\"kind\":\"future.thing\",\"msg\":\"x\",\"data\":{{}}}}\n",
        last.time,
        last.boot.as_str()
    );
    last.seq = Seq::new(at + 1).unwrap();
    let path = dir.path().join("events.jsonl");
    let mut journal = fs::read_to_string(&path).unwrap();
    journal.push_str(&unknown);
    journal.push_str(&last.encode().unwrap());
    journal.push('\n');
    fs::write(&path, journal).unwrap();
    let before = fs::read(&path).unwrap();
    let ran = stats(&[os("--state-dir"), dir.path().as_os_str()]);
    assert_eq!(ran.status.code(), Some(0));
    assert!(text(ran.stdout).contains("Hunts\n"));
    assert_eq!(
        text(ran.stderr),
        format!(
            "togi-tool-stats: skipped 1 event of a kind this build does not know (future.thing at line {at})\n"
        )
    );
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[cfg(target_os = "linux")]
#[test]
fn real_process_a_report_that_cannot_be_written_exits_one_and_names_the_write() {
    let (dir, _) = state_with_torn_tail();
    let full = fs::File::options().write(true).open("/dev/full").unwrap();
    let ran = Command::new(env!("CARGO_BIN_EXE_togi-tool-stats"))
        .args([os("--state-dir"), dir.path().as_os_str()])
        .stdout(full)
        .output()
        .unwrap();
    assert_eq!(ran.status.code(), Some(1));
    assert!(text(ran.stderr).starts_with("togi-tool-stats: write the report: "));
}

#[test]
fn real_process_a_forecast_scores_the_state_directory_instead_of_reporting() {
    let events = hunting_session();
    let dir = tempdir().unwrap();
    write_journal(dir.path(), &events);
    let session = togi_tool_forecast::Session::from_events(events).unwrap();
    let anchor = session
        .anchor_at(togi_journal::Seq::new(2).unwrap())
        .unwrap();
    let cores = hunting_cores();
    let runs = vec![forecast_run(&cores, togi_tool_forecast::Status::Concluded)];
    let record = togi_tool_forecast::Record::new(anchor, "dev", false, Vec::new(), runs).unwrap();
    let forecast = dir.path().join("forecast.json");
    let mut bytes = Vec::new();
    record.write(&mut bytes).unwrap();
    fs::write(&forecast, bytes).unwrap();
    let ran = stats(&[
        os("--state-dir"),
        dir.path().as_os_str(),
        os("--forecast"),
        forecast.as_os_str(),
    ]);
    let out = text(ran.stdout);
    assert_eq!(ran.status.code(), Some(0), "{}", text(ran.stderr));
    assert!(out.starts_with("Forecast: commit dev"), "{out}");
    assert!(out.contains("\nForecast score\n"), "{out}");
    assert!(Path::new(&forecast).exists());
}

fn hunting_cores() -> Vec<i64> {
    (0..4).collect()
}

fn forecast_run(cores: &[i64], status: togi_tool_forecast::Status) -> togi_tool_forecast::Run {
    togi_tool_forecast::Run {
        machine: "m.json".to_owned(),
        seed: 1,
        split: togi_tool_forecast::Split::Dev,
        outcome: togi_tool_forecast::Outcome {
            status,
            hours: 5.0,
            crashes: 3,
            hunts: 1,
            profile: cores
                .iter()
                .map(|&n| {
                    (
                        togi_machine::CoreId::new(n).unwrap(),
                        togi_machine::ObservedOffset(-10),
                    )
                })
                .collect(),
            depth: -10 * i64::try_from(cores.len()).unwrap(),
        },
    }
}
