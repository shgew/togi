use std::fs;
use std::process::{Command, Output};

use tempfile::tempdir;
use test_case::test_case;
use togi_journal::Seq;
use togi_tool_stats::{Journal, report};

use crate::fixture::{crashed_session, write_journal};

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
    let events = crashed_session();
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

#[derive(Clone, Copy)]
enum Source {
    StateDirectory,
    Journal,
}

#[test_case(Source::StateDirectory; "state directory")]
#[test_case(Source::Journal; "explicit journal overrides absent state directory")]
fn real_process_the_report_reads_only_the_journal_and_leaves_it_as_it_was(source: Source) {
    let (dir, want) = state_with_torn_tail();
    let journal = dir.path().join("events.jsonl");
    let before = fs::read(&journal).unwrap();
    let absent = dir.path().join("absent");
    let args = match source {
        Source::StateDirectory => vec![os("--state-dir"), dir.path().as_os_str()],
        Source::Journal => vec![
            os("--journal"),
            journal.as_os_str(),
            os("--state-dir"),
            absent.as_os_str(),
        ],
    };
    let ran = stats(&args);
    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(text(ran.stderr), "");
    assert_eq!(text(ran.stdout), want);
    assert_eq!(fs::read(&journal).unwrap(), before);
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
    assert!(out.contains("\nTctl\n"), "{out}");
}

#[test_case(&["unexpected"]; "positional argument")]
#[test_case(&["--since", "not-a-time"]; "invalid cutoff")]
#[test_case(&["--unknown"]; "unknown option")]
#[test_case(&["--forecast", "forecast.json"]; "retired forecast option")]
fn real_process_rejected_arguments_exit_two_without_a_report(args: &[&str]) {
    let dir = tempdir().unwrap();
    let mut full = vec![os("--state-dir"), dir.path().as_os_str()];
    full.extend(args.iter().map(|arg| os(arg)));
    let ran = stats(&full);
    assert_eq!(ran.status.code(), Some(2), "{args:?}");
    assert_eq!(text(ran.stdout), "", "{args:?}");
    assert!(!text(ran.stderr).is_empty(), "{args:?}");
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
    let events = crashed_session();
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
    let events = crashed_session();
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
    let mut events = crashed_session();
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
    assert!(text(ran.stdout).contains("Exposure\n"));
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
