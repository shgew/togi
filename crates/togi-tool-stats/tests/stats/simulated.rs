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
    let text = report(&Journal::new(events.clone()).unwrap(), None);

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
    let ended: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::HuntEnd(end) => Some(end),
            _ => None,
        })
        .map(|end| {
            let members = if end.members.is_empty() {
                let cores: Vec<String> = end
                    .cores
                    .iter()
                    .map(|c| format!("{:02}", c.get()))
                    .collect();
                if cores.is_empty() {
                    "-".to_owned()
                } else {
                    cores.join(",")
                }
            } else {
                let mut members: Vec<_> = end.members.iter().collect();
                members.sort_by_key(|member| member.core);
                let labels: Vec<String> = members
                    .iter()
                    .map(|member| format!("{:02}:{}", member.core.get(), member.offset.get()))
                    .collect();
                labels.join(",")
            };
            (end.hunt.get(), end.result.to_string(), members)
        })
        .collect();
    assert!(!ended.is_empty(), "the crash was never hunted");
    let mut hunts: Vec<_> = rows(&text, "Hunts")
        .iter()
        .map(|hunt| {
            (
                hunt[0].parse::<u32>().unwrap(),
                hunt[9].to_owned(),
                hunt[10].to_owned(),
            )
        })
        .collect();
    hunts.sort();
    let mut ended = ended;
    ended.sort();
    assert_eq!(hunts, ended, "{text}");
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

#[test]
fn a_simulated_sessions_chain_records_attribute_each_trial_once_to_its_own_set() {
    let events = hunting_session();
    let text =
        togi_tool_stats::chain_records(&Journal::new(events.clone()).unwrap(), None).unwrap();
    let intents: std::collections::HashMap<u64, &togi_journal::TrialIntent> = events
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::TrialIntent(intent) => Some((event.seq.get(), intent)),
            _ => None,
        })
        .collect();
    let mut seen = std::collections::HashSet::new();
    let mut kinds = std::collections::BTreeMap::new();
    for line in text.lines() {
        let record: serde_json::Value = serde_json::from_str(line).unwrap();
        *kinds
            .entry(record["kind"].as_str().unwrap().to_owned())
            .or_insert(0) += 1;
        let cores = &record["cores"];
        for (list, rerun) in [("trials", false), ("reruns", true)] {
            for trial in record[list].as_array().unwrap() {
                let seq = trial["seq"].as_u64().unwrap();
                assert!(seen.insert(seq), "trial {seq} attributed twice: {text}");
                let intent = intents[&seq];
                assert!(seq > record["seq"].as_u64().unwrap());
                assert_eq!(intent.regime, togi_machine::Regime::R7);
                assert_eq!(
                    intent.cycle.map(|c| u64::from(c.get())),
                    record["cycle"].as_u64()
                );
                assert_eq!(
                    intent.step.map(|s| u64::from(s.get())),
                    record["step"].as_u64()
                );
                assert_eq!(&serde_json::to_value(intent.cores).unwrap(), cores);
                assert_eq!(intent.rerun, rerun);
            }
        }
    }
    assert!(
        kinds.contains_key("first") && kinds.contains_key("unchanged"),
        "the hunt's backoff re-derives the session's chains: {kinds:?}"
    );
}
