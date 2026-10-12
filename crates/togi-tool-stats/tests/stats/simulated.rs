use togi_journal::Payload;
use togi_tool_stats::{Journal, report};

use crate::sim::hunting_session;

/// The rows of the section titled `title`: its lines after the header, up to the blank line.
fn rows<'a>(text: &'a str, title: &str) -> Vec<Vec<&'a str>> {
    let mut lines = text.lines().skip_while(|line| *line != title).skip(2);
    let mut rows = Vec::new();
    while let Some(line) = lines.next().filter(|line| !line.is_empty()) {
        rows.push(line.split_whitespace().collect());
    }
    rows
}

#[test]
fn a_simulated_sessions_report_agrees_with_its_events() {
    let events = hunting_session();
    let intents = events
        .iter()
        .filter(|event| matches!(event.payload, Payload::TrialIntent(_)))
        .count();
    let crashes = events
        .iter()
        .filter(|event| matches!(event.payload, Payload::CrashDetected(_)))
        .count();
    assert!(crashes > 0, "the machine never crashed");
    let text = report(&Journal::new(events).unwrap(), None);

    let runs = rows(&text, "Runs");
    let column = |at: usize| -> usize {
        runs.iter()
            .map(|run| run[at].parse::<usize>().unwrap())
            .sum()
    };
    assert_eq!((column(4), column(5)), (intents, crashes), "{text}");
    let exposure: usize = rows(&text, "Exposure")
        .iter()
        .map(|row| row[2].parse::<usize>().unwrap())
        .sum();
    assert!(exposure <= intents && exposure > 0, "{text}");
    let hunts = rows(&text, "Hunts");
    assert!(!hunts.is_empty(), "the crash was never hunted:\n{text}");
    for hunt in &hunts {
        assert_ne!(hunt[10], "open", "{text}");
    }
    let timing: usize = rows(&text, "Crash timing (last evidence)")
        .iter()
        .map(|row| row[1].parse::<usize>().unwrap())
        .sum();
    assert!(timing > 0 && timing <= crashes, "{text}");
}

#[test]
fn a_report_is_the_same_for_the_same_journal() {
    let events = hunting_session();
    let journal = Journal::new(events).unwrap();
    assert_eq!(report(&journal, None), report(&journal, None));
}
