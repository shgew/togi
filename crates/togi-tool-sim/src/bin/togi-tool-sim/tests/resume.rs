use std::path::Path;

use super::common::Host;

/// Reconstruct the simulator for each invocation, retaining only the journal and machine document.
#[test]
fn a_session_resumed_from_recorded_memory_ends_where_an_uninterrupted_one_does() {
    let mut host = Host::hunting();
    assert_eq!(
        host.run(["--machine", "/hunt.json", "--state-dir", "/whole"])
            .code,
        Some(0)
    );
    let mut resumes = 0;
    let split = [
        "--machine",
        "/hunt.json",
        "--state-dir",
        "/split",
        "--max-boots",
        "2",
    ];
    let mut code = host.run(split).code;
    while code == Some(3) {
        resumes += 1;
        assert!(resumes < 20, "the split session never finished");
        code = host.run(split).code;
    }
    assert_eq!(code, Some(0));
    assert!(resumes >= 2, "the session crashed too rarely to split it");
    let whole = &host.states[Path::new("/whole")];
    let split = &host.states[Path::new("/split")];
    assert!(
        !split
            .entries()
            .iter()
            .any(|(path, _)| path.starts_with("/state/archive"))
    );
    for name in ["events.jsonl", "state.json"] {
        let path = Path::new("/state").join(name);
        let whole = String::from_utf8(whole.contents(&path).unwrap()).unwrap();
        let split = String::from_utf8(split.contents(&path).unwrap()).unwrap();
        same_text(name, &whole, &split);
    }
}

fn same_text(name: &str, whole: &str, split: &str) {
    if let Some((line, (expected, found))) = whole
        .lines()
        .zip(split.lines())
        .enumerate()
        .find(|(_, (a, b))| a != b)
    {
        panic!(
            "{name} differs first at line {}:\nuninterrupted: {expected}\nresumed:       {found}",
            line + 1
        );
    }
    assert_eq!(whole.lines().count(), split.lines().count(), "{name} lines");
    let at = whole
        .bytes()
        .zip(split.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let near = |text: &str| {
        let rest = &text.as_bytes()[at..];
        rest[..rest.len().min(16)].escape_ascii().to_string()
    };
    assert_eq!(
        near(whole),
        near(split),
        "{name} differs outside its lines' text at byte {at}"
    );
}

#[test]
#[should_panic(expected = "differs outside its lines' text")]
fn files_that_differ_only_in_their_final_newline_differ() {
    same_text("events.jsonl", "{\"seq\":1}\n", "{\"seq\":1}");
}

#[test]
#[should_panic(expected = "differs outside its lines' text")]
fn files_that_differ_only_in_line_endings_differ() {
    same_text("events.jsonl", "{\"seq\":1}\n", "{\"seq\":1}\r\n");
}
