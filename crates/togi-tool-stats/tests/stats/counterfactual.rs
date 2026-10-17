use std::collections::{HashMap, HashSet};

use serde_json::Value;
use test_case::test_case;
use togi_journal::{CycleEvent, Event, Outcome, Payload};
use togi_tool_stats::{Journal, chain_records, counterfactual};

use crate::sim::machine_session;

/// A trial's end as the checks need it: its intent's class and its outcome.
struct Ended {
    workload: Value,
    cores: Value,
    duration_s: u64,
    pass: bool,
}

struct Session {
    records: Vec<Value>,
    /// Each chain derivation's trial ends, by derivation seq.
    chain_ends: HashMap<u64, Vec<u64>>,
    chain_s: u64,
    ends: HashMap<u64, Ended>,
    cycle_starts: HashMap<u64, u64>,
}

fn accounted(events: &[Event]) -> Session {
    let journal = Journal::new(events.to_vec()).unwrap();
    let accounting = counterfactual(&journal).unwrap();
    assert_eq!(
        counterfactual(&journal).unwrap().records,
        accounting.records,
        "the accounting of one journal is deterministic"
    );
    let records = accounting
        .records
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let mut intents = HashMap::new();
    let mut ends = HashMap::new();
    let mut end_of = HashMap::new();
    let mut cycle_starts = HashMap::new();
    for event in events {
        if let Payload::TrialIntent(intent) = &event.payload {
            intents.insert(intent.trial.clone(), (event.seq.get(), intent));
        } else if let Payload::TrialEnd(end) = &event.payload {
            let (seq, intent) = intents[&end.trial];
            end_of.insert(seq, event.seq.get());
            ends.insert(
                event.seq.get(),
                Ended {
                    workload: serde_json::to_value(intent.workload).unwrap(),
                    cores: serde_json::to_value(intent.cores).unwrap(),
                    duration_s: u64::from(intent.duration_s),
                    pass: end.outcome == Outcome::Pass,
                },
            );
        } else if let Payload::CheckingCycle(cycle) = &event.payload
            && cycle.event == CycleEvent::Start
        {
            cycle_starts.insert(u64::from(cycle.cycle.get()), event.seq.get());
        }
    }
    let mut chain_ends = HashMap::new();
    let mut chain_s = 0;
    for line in chain_records(&journal, None).unwrap().lines() {
        let record: Value = serde_json::from_str(line).unwrap();
        let trials = record["trials"].as_array().unwrap();
        chain_s += trials
            .iter()
            .map(|t| t["duration_s"].as_u64().unwrap())
            .sum::<u64>();
        chain_ends.insert(
            record["seq"].as_u64().unwrap(),
            trials
                .iter()
                .filter_map(|t| end_of.get(&t["seq"].as_u64().unwrap()).copied())
                .collect(),
        );
    }
    Session {
        records,
        chain_ends,
        chain_s,
        ends,
        cycle_starts,
    }
}

fn of<'a>(session: &'a Session, record: &str, candidate: &str) -> Vec<&'a Value> {
    session
        .records
        .iter()
        .filter(|r| r["record"] == record && r["candidate"] == candidate)
        .collect()
}

fn n(value: &Value) -> u64 {
    value.as_u64().unwrap()
}

/// The trial ends of every derivation `candidate` skipped.
fn skipped(session: &Session, candidate: &str) -> HashSet<u64> {
    of(session, "derivation", candidate)
        .into_iter()
        .filter(|d| d["verdict"] == "skipped")
        .flat_map(|d| session.chain_ends[&n(&d["seq"])].iter().copied())
        .collect()
}

#[test_case("target-fit-0.json", 2)]
#[test_case("target-fit-0.json", 3)]
#[test_case("target-fit-1.json", 5)]
#[test_case("target-fit-2.json", 2)]
fn every_chain_second_is_kept_skipped_or_unresolved_and_every_witness_is_a_valid_pass(
    machine: &str,
    seed: u64,
) {
    let session = accounted(&machine_session(machine, seed));
    for candidate in ["coalesce", "defer", "tie"] {
        let summary = of(&session, "summary", candidate);
        let [summary] = summary.as_slice() else {
            panic!("one summary of {candidate}: {:?}", session.records);
        };
        let cycles = of(&session, "cycle", candidate);
        let sum = |field: &str| cycles.iter().map(|c| n(&c[field])).sum::<u64>();
        assert_eq!(n(&summary["trial_s"]), session.chain_s, "{candidate}");
        assert_eq!(
            n(&summary["kept_s"]) + n(&summary["skipped_s"]) + n(&summary["unresolved_s"]),
            session.chain_s,
            "{candidate}: {summary}"
        );
        assert_eq!(n(&summary["skipped_s"]), sum("skipped_s"), "{candidate}");
        assert_eq!(n(&summary["required_s"]), sum("required_s"), "{candidate}");
        assert_eq!(
            n(&summary["unresolved_s"]),
            sum("unresolved_s"),
            "{candidate}"
        );
        assert_eq!(
            summary["avoidable_s"].as_i64().unwrap(),
            i64::try_from(n(&summary["skipped_s"])).unwrap()
                - i64::try_from(n(&summary["required_s"])).unwrap(),
            "{candidate}"
        );
        let skipped = skipped(&session, candidate);
        for cycle in cycles.iter().filter(|c| c["status"] == "covered") {
            let start = session.cycle_starts[&n(&cycle["cycle"])];
            let mut required = 0;
            for coverage in cycle["coverage"].as_array().unwrap() {
                let witnesses = coverage["witnesses"].as_array().unwrap();
                let rerun = coverage["rerun"].as_array().unwrap();
                assert_eq!(
                    witnesses.len()
                        + rerun.len()
                        + usize::try_from(n(&coverage["unobserved"])).unwrap(),
                    usize::try_from(n(&coverage["needed"])).unwrap(),
                    "{candidate}: {coverage}"
                );
                required += (u64::try_from(rerun.len()).unwrap() + n(&coverage["unobserved"]))
                    * n(&coverage["duration_s"]);
                for (seq, from_skipped) in witnesses
                    .iter()
                    .map(|s| (n(s), false))
                    .chain(rerun.iter().map(|s| (n(s), true)))
                {
                    let end = &session.ends[&seq];
                    assert!(end.pass && seq > start, "{candidate}: {seq} of {coverage}");
                    assert_eq!(
                        (&end.workload, &end.cores, end.duration_s),
                        (
                            &coverage["workload"],
                            &coverage["cores"],
                            n(&coverage["duration_s"])
                        ),
                        "{candidate}: {seq} of {coverage}"
                    );
                    assert_eq!(
                        skipped.contains(&seq),
                        from_skipped,
                        "{candidate}: {seq} of {coverage}"
                    );
                }
            }
            assert_eq!(required, n(&cycle["required_s"]), "{candidate}: {cycle}");
        }
        for failure in of(&session, "failure", candidate) {
            let at = n(&failure["seq"]);
            for d in of(&session, "derivation", candidate)
                .into_iter()
                .filter(|d| n(&d["seq"]) > at)
            {
                assert_eq!(
                    d["reason"], "after_failure",
                    "{candidate}: {d} after {failure}"
                );
            }
            assert!(
                cycles
                    .iter()
                    .filter(|c| n(&c["cycle"]) >= n(&failure["cycle"]))
                    .all(|c| c["status"] != "covered" && n(&c["skipped_s"]) == 0),
                "{candidate}: no cycle from {failure} on is counted"
            );
        }
    }
}

#[test]
fn a_skipped_failure_is_reported_and_leaves_what_follows_unresolved() {
    let session = accounted(&machine_session("target-fit-2.json", 2));
    for candidate in ["coalesce", "defer", "tie"] {
        let failures = of(&session, "failure", candidate);
        let [failure] = failures.as_slice() else {
            panic!("{candidate} skips one failing trial: {failures:?}");
        };
        assert_eq!(failure["delayed"], false, "{candidate}: {failure}");
        let summary = of(&session, "summary", candidate)[0];
        assert_eq!(n(&summary["lost"]), 1, "{candidate}: {summary}");
        assert!(
            n(&summary["unresolved"]) > 0 && n(&summary["unresolved_s"]) > 0,
            "{candidate}: {summary}"
        );
    }
}

#[test]
fn the_tie_candidate_counts_the_set_it_keeps_instead_of_the_tuners() {
    let session = accounted(&machine_session("target-fit-0.json", 3));
    let kept: Vec<&Value> = of(&session, "cycle", "tie")
        .into_iter()
        .filter(|c| c["status"] == "covered")
        .flat_map(|c| c["coverage"].as_array().unwrap())
        .filter(|coverage| !coverage["instead_of"].is_null())
        .collect();
    assert!(!kept.is_empty(), "{:?}", session.records);
    for coverage in kept {
        assert_ne!(coverage["cores"], coverage["instead_of"], "{coverage}");
    }
    let summary = of(&session, "summary", "tie")[0];
    assert!(n(&summary["skipped"]) > 0, "{summary}");
}
