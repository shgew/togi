use togi_journal::Payload;
use togi_tool_stats::{Journal, report};
use togi_tuner::TrialContext;

use crate::sim::{chain_session, hazard_session, hunting_session};

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
        .filter_map(|event| {
            if let Payload::HuntEnd(end) = &event.payload {
                Some(end)
            } else {
                None
            }
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
        .filter_map(|event| {
            if let Payload::TrialIntent(intent) = &event.payload {
                Some((event.seq.get(), intent))
            } else {
                None
            }
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

/// The phase the tuner stood in before `seq` folded, read from the journal's own phase ends: the full
/// `checking.cycle` that ends phase 1, and the confirmation cycle that ends phase 2.
struct PhaseEnds {
    one: Option<u64>,
    two: Option<u64>,
}

impl PhaseEnds {
    fn of(events: &[togi_journal::Event]) -> Self {
        let end = |prefix: &str| {
            events.iter().find_map(|event| {
                let Payload::CheckingCycle(c) = &event.payload else {
                    return None;
                };
                c.reason
                    .as_deref()
                    .is_some_and(|r| r.starts_with(prefix))
                    .then(|| event.seq.get())
            })
        };
        Self {
            one: end("phase 1 ends:"),
            two: end("phase 2 ends:"),
        }
    }

    /// The JSON name and the rendered label of the phase at `seq`.
    fn at(&self, seq: u64) -> (&'static str, &'static str) {
        if self.one.is_none_or(|end| seq < end) {
            ("phase_1", "phase 1")
        } else if self.two.is_none_or(|end| seq < end) {
            ("phase_2", "phase 2")
        } else {
            ("concluded", "concluded")
        }
    }
}

/// Checks a simulated session's backoff records and report against its events, and returns the phases its
/// failures and backoffs were recorded in.
fn check_backoff_records(events: &[togi_journal::Event]) -> std::collections::BTreeSet<String> {
    let journal = Journal::new(events.to_vec()).unwrap();
    let text = togi_tool_stats::backoff_records(&journal, None).unwrap();
    let ends = PhaseEnds::of(events);
    let mut intents = std::collections::HashMap::new();
    let mut r7 = std::collections::HashSet::new();
    let mut held = std::collections::HashMap::new();
    let mut backoffs = std::collections::BTreeSet::new();
    let mut state = togi_tuner::State::new();
    for event in events {
        state.fold(event).unwrap();
        if let Payload::TrialIntent(intent) = &event.payload {
            intents.insert(serde_json::to_value(&intent.trial).unwrap(), intent);
            if intent.regime == togi_machine::Regime::R7 && intent.cores.len() > 1 {
                r7.insert(intent.trial.clone());
                let requirement = state.open_requirement().unwrap();
                let kind = match requirement.context {
                    TrialContext::Search { .. } => "search",
                    TrialContext::SoloLimit { .. } => "solo_limit",
                    TrialContext::Cycle { .. } => "cycle",
                    TrialContext::Rerun { .. } => "rerun",
                    TrialContext::ZeroRerun { .. } => "zero_rerun",
                    TrialContext::Round { .. } => "round",
                    TrialContext::Hunt { .. } => "hunt",
                };
                let mut value = serde_json::json!({
                    "kind": kind,
                    "needed": requirement.needed.get(),
                });
                if let TrialContext::Cycle { step, part, .. } = requirement.context {
                    value["step"] = serde_json::to_value(step).unwrap();
                    value["part"] = serde_json::to_value(part).unwrap();
                }
                held.insert(intent.trial.clone(), value);
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

    let rendered = report(&journal, None);
    let listed: Vec<String> = rows(&rendered, "R7 voltage-targeted backoffs")
        .into_iter()
        .map(|row| row.join(" "))
        .collect();
    assert_eq!(listed.len(), backoffs.len(), "{rendered}");
    let mut charged = std::collections::BTreeSet::new();
    let mut phases = std::collections::BTreeSet::new();
    let mut totals: std::collections::BTreeMap<&str, [i64; 5]> = std::collections::BTreeMap::new();
    let mut distribution: std::collections::BTreeMap<i64, u32> = std::collections::BTreeMap::new();
    let mut with_part = 0;
    for line in text.lines() {
        let record: serde_json::Value = serde_json::from_str(line).unwrap();
        let seq = record["seq"].as_u64().unwrap();
        assert_eq!(record["origin"], "trial", "{line}");
        assert_eq!(record.get("carried"), None, "{line}");
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
        assert_eq!(
            requirement, &held[&intent.trial],
            "the requirement the tuner held for the trial: {line}"
        );
        if intent.cycle.is_some() && !intent.rerun {
            assert_eq!(requirement["kind"], "cycle", "{line}");
            assert_eq!(
                intent.step.map(|s| u64::from(s.get())),
                requirement["step"].as_u64(),
                "{line}"
            );
        }
        let kind = requirement["kind"].as_str().unwrap();
        let needed = requirement["needed"].as_u64().unwrap();
        let label = match (requirement["step"].as_u64(), requirement["part"].as_u64()) {
            (Some(step), Some(part)) => {
                with_part += 1;
                format!("{kind} {step}.{part} x{needed}")
            }
            (Some(step), None) => format!("{kind} {step} x{needed}"),
            (None, Some(part)) => format!("{kind} part {part} x{needed}"),
            (None, None) => format!("{kind} x{needed}"),
        };
        let (phase, _) = ends.at(seq);
        assert_eq!(record["phase"], phase, "{line}");
        phases.insert(phase.to_owned());
        let total = totals.entry(phase).or_default();
        total[0] += 1;
        let record_backoffs = record["backoffs"].as_array().unwrap();
        total[2] += i64::from(!record_backoffs.is_empty());
        let cores: Vec<String> = intent
            .cores
            .iter()
            .map(|core| format!("{:02}", core.get()))
            .collect();
        let mut moved_total = 0;
        for backoff in record_backoffs {
            let at = backoff["seq"].as_u64().unwrap();
            assert!(charged.insert(at), "backoff {at} charged twice: {text}");
            assert!(at > seq, "{line}");
            let moved = backoff["to"].as_i64().unwrap() - backoff["from"].as_i64().unwrap();
            assert!(moved > 0, "{line}");
            assert_eq!(backoff["counts"].as_i64(), Some(moved), "{line}");
            moved_total += moved;
            let (phase, shown) = ends.at(at);
            assert_eq!(backoff["phase"], phase, "{line}");
            phases.insert(phase.to_owned());
            let total = totals.entry(phase).or_default();
            total[3] += 1;
            total[4] += moved;
            let causes: Vec<String> = backoff["causes"]
                .as_array()
                .unwrap()
                .iter()
                .map(ToString::to_string)
                .collect();
            let row = format!(
                "{at} {shown} backoff {:02} {} {} {moved} {} {} {label} {} ",
                backoff["core"].as_u64().unwrap(),
                backoff["from"],
                backoff["to"],
                record["trial"].as_str().unwrap(),
                cores.join(","),
                causes.join(","),
            );
            assert!(
                listed.iter().any(|listed| listed.starts_with(&row)),
                "backoff {at} is listed as {row}: {rendered}"
            );
        }
        *distribution.entry(moved_total).or_default() += 1;
    }
    assert_eq!(
        charged, backoffs,
        "every voltage-targeted backoff, once: {text}"
    );
    assert!(with_part > 0, "no failed trial loaded a chain part: {text}");

    let mut expected = Vec::new();
    let mut all = [0; 5];
    for (phase, label) in [
        ("phase_1", "phase 1"),
        ("phase_2", "phase 2"),
        ("concluded", "concluded"),
    ] {
        if let Some(total) = totals.get(phase) {
            for (sum, value) in all.iter_mut().zip(total) {
                *sum += value;
            }
            expected.push(format!(
                "{label} {}",
                total.map(|value| value.to_string()).join(" ")
            ));
        }
    }
    expected.push(format!(
        "all {}",
        all.map(|value| value.to_string()).join(" ")
    ));
    let by_phase: Vec<String> = rows(&rendered, "R7 failures and backoffs by phase")
        .into_iter()
        .map(|row| row.join(" "))
        .collect();
    assert_eq!(by_phase, expected, "{rendered}");
    let by_counts: Vec<String> = rows(&rendered, "R7 failures by counts charged")
        .into_iter()
        .map(|row| row.join(" "))
        .collect();
    assert_eq!(
        by_counts,
        distribution
            .iter()
            .map(|(counts, failures)| format!("{counts} {failures}"))
            .collect::<Vec<_>>(),
        "{rendered}"
    );
    phases
}

#[test]
fn a_simulated_sessions_backoff_records_charge_each_backoff_once_to_its_failed_trial() {
    assert_eq!(
        check_backoff_records(&chain_session()),
        ["phase_1".to_owned()].into(),
        "the session stops at its first clean cycle, the end of phase 1"
    );
}

#[test]
fn a_simulated_hazard_sessions_backoffs_are_attributed_to_the_phase_they_were_decided_in() {
    assert_eq!(
        check_backoff_records(&hazard_session()),
        [
            "concluded".to_owned(),
            "phase_1".to_owned(),
            "phase_2".to_owned()
        ]
        .into(),
        "the session fails R7 in each phase"
    );
}
