use togi_journal::Payload;
use togi_tool_stats::{Journal, report};

use crate::sim::{chain_session, hunting_session};

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
    let events = chain_session();
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
    let mut reruns = 0;
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
                reruns += usize::from(rerun);
                let intent = intents[&seq];
                assert!(
                    seq > record["seq"].as_u64().unwrap(),
                    "trial {seq} of {line}"
                );
                assert_eq!(
                    intent.regime,
                    togi_machine::Regime::R7,
                    "trial {seq} of {line}"
                );
                let (cycle, step) = if rerun {
                    (None, None)
                } else {
                    (record["cycle"].as_u64(), record["step"].as_u64())
                };
                assert_eq!(
                    intent.cycle.map(|c| u64::from(c.get())),
                    cycle,
                    "trial {seq} of {line}: a rerun belongs to no cycle"
                );
                assert_eq!(
                    intent.step.map(|s| u64::from(s.get())),
                    step,
                    "trial {seq} of {line}"
                );
                assert_eq!(
                    &serde_json::to_value(intent.workload).unwrap(),
                    &record["workload"],
                    "trial {seq} of {line}"
                );
                assert_eq!(
                    &serde_json::to_value(intent.cores).unwrap(),
                    cores,
                    "trial {seq} of {line}"
                );
                assert_eq!(intent.rerun, rerun, "trial {seq} of {line}");
            }
        }
    }
    assert!(
        kinds.contains_key("first") && kinds.contains_key("unchanged"),
        "the backoff re-derives the session's chains: {kinds:?}"
    );
    assert!(
        seen.len() > reruns && reruns > 0,
        "the session's chain trials and the failed load's reruns are attributed: {text}"
    );
}

#[test]
fn a_simulated_sessions_backoff_records_charge_each_backoff_once_to_its_failed_trial() {
    let events = chain_session();
    let journal = Journal::new(events.clone()).unwrap();
    let text = togi_tool_stats::backoff_records(&journal, None).unwrap();
    let mut intents = std::collections::HashMap::new();
    let mut r7 = std::collections::HashSet::new();
    let mut backoffs = std::collections::BTreeSet::new();
    for event in &events {
        if let Payload::TrialIntent(intent) = &event.payload {
            intents.insert(serde_json::to_value(&intent.trial).unwrap(), intent);
            if intent.regime == togi_machine::Regime::R7 && intent.cores.len() > 1 {
                r7.insert(intent.trial.clone());
            }
        }
        let failure_of = |seq: &togi_journal::Seq| {
            events.iter().find(|e| e.seq == *seq).is_some_and(|cause| {
                if let Payload::Failure(f) = &cause.payload {
                    f.trial.as_ref().is_some_and(|t| r7.contains(t))
                } else if let Payload::TrialEnd(end) = &cause.payload {
                    r7.contains(&end.trial)
                } else {
                    false
                }
            })
        };
        if let Payload::TunerDecision(d) = &event.payload {
            if d.decision == togi_journal::Decision::Backoff && event.cause.iter().any(failure_of) {
                backoffs.insert(event.seq.get());
            }
        }
    }
    assert!(!backoffs.is_empty(), "the session backed off no R7 failure");

    let mut charged = std::collections::BTreeSet::new();
    let mut phases = Vec::new();
    let (mut failures, mut counts) = (0, 0);
    for line in text.lines() {
        let record: serde_json::Value = serde_json::from_str(line).unwrap();
        failures += 1;
        assert_eq!(record["carried"], false, "{line}");
        let intent = intents[&record["trial"]];
        assert_eq!(
            serde_json::to_value(intent.cores).unwrap(),
            record["cores"],
            "{line}"
        );
        assert_eq!(
            serde_json::to_value(intent.workload).unwrap(),
            record["workload"],
            "{line}"
        );
        assert_eq!(
            intent.cycle.map(|c| u64::from(c.get())),
            record["cycle"].as_u64(),
            "{line}"
        );
        let requirement = &record["requirement"];
        if intent.cycle.is_some() && !intent.rerun {
            assert_eq!(requirement["kind"], "cycle", "{line}");
            assert_eq!(
                intent.step.map(|s| u64::from(s.get())),
                requirement["step"].as_u64(),
                "{line}"
            );
        }
        assert!(requirement["needed"].as_u64().unwrap() > 0, "{line}");
        let phase = record["phase"].as_str().unwrap();
        phases.push(
            ["phase_1", "phase_2", "concluded"]
                .iter()
                .position(|known| *known == phase)
                .unwrap_or_else(|| panic!("{line}")),
        );
        for backoff in record["backoffs"].as_array().unwrap() {
            let seq = backoff["seq"].as_u64().unwrap();
            assert!(charged.insert(seq), "backoff {seq} charged twice: {text}");
            assert!(seq > record["seq"].as_u64().unwrap(), "{line}");
            let moved = backoff["to"].as_i64().unwrap() - backoff["from"].as_i64().unwrap();
            assert!(moved > 0, "{line}");
            assert_eq!(backoff["counts"].as_i64(), Some(moved), "{line}");
            counts += moved;
        }
    }
    assert_eq!(
        charged, backoffs,
        "every voltage-targeted backoff, once: {text}"
    );
    assert!(
        phases.is_sorted(),
        "phase 1 failures before phase 2's, before the conclusion's: {phases:?}"
    );

    let rendered = report(&journal, None);
    let listed = rows(&rendered, "R7 voltage-targeted backoffs");
    assert_eq!(listed.len(), backoffs.len(), "{rendered}");
    assert!(
        listed.iter().all(|row| row[1] == "phase"),
        "a simulated session folds, so every backoff has its phase: {rendered}"
    );
    let all = rows(&rendered, "R7 failures and backoffs by phase")
        .into_iter()
        .find(|row| row[0] == "all")
        .unwrap();
    assert_eq!(
        all,
        [
            "all".to_owned(),
            failures.to_string(),
            "0".to_owned(),
            text.lines()
                .filter(|line| !line.contains("\"backoffs\":[]"))
                .count()
                .to_string(),
            backoffs.len().to_string(),
            counts.to_string(),
        ]
    );
}
