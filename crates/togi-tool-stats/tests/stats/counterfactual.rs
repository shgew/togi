use std::collections::{HashMap, HashSet};

use serde_json::Value;
use test_case::test_case;
use togi_config::Config;
use togi_journal::{CycleEvent, Event, EventKind, Outcome, Payload};
use togi_tool_stats::{Journal, chain_records, counterfactual};

use crate::sim::{TARGET_FIT_0, TARGET_FIT_1, TARGET_FIT_2, machine_session, machine_session_with};

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
    /// The `--chains` records of the journal's derivations, in journal order.
    chains: Vec<Value>,
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
    let mut chains = Vec::new();
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
        chains.push(record);
    }
    Session {
        records,
        chain_ends,
        chains,
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

/// The trial ends of every derivation `candidate` decided with one of `verdicts`.
fn ends_of(session: &Session, candidate: &str, verdicts: &[&str]) -> HashSet<u64> {
    of(session, "derivation", candidate)
        .into_iter()
        .filter(|d| verdicts.iter().any(|&verdict| d["verdict"] == verdict))
        .flat_map(|d| session.chain_ends[&n(&d["seq"])].iter().copied())
        .collect()
}

#[test_case(TARGET_FIT_0, 2 ; "target_fit_0_seed_2")]
#[test_case(TARGET_FIT_0, 3 ; "target_fit_0_seed_3")]
#[test_case(TARGET_FIT_1, 5 ; "target_fit_1_seed_5")]
#[test_case(TARGET_FIT_2, 2 ; "target_fit_2_seed_2")]
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
        let unkept = ends_of(&session, candidate, &["skipped", "unresolved"]);
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
                        unkept.contains(&seq),
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
    let session = accounted(&machine_session(TARGET_FIT_2, 2));
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

#[test_case("coalesce", true ; "coalesce_delays_it")]
#[test_case("defer", true ; "defer_delays_it")]
#[test_case("tie", false ; "tie_loses_it")]
fn a_skipped_failure_whose_set_the_final_chain_still_holds_is_delayed(
    candidate: &str,
    delayed: bool,
) {
    let session = accounted(&machine_session(TARGET_FIT_2, 5));
    let failures = of(&session, "failure", candidate);
    let [failure] = failures.as_slice() else {
        panic!("{candidate} skips one failing trial: {failures:?}");
    };
    assert_eq!(failure["delayed"], delayed, "{candidate}: {failure}");
    let summary = of(&session, "summary", candidate)[0];
    assert_eq!(
        (n(&summary["delayed"]), n(&summary["lost"])),
        if delayed { (1, 0) } else { (0, 1) },
        "{candidate}: {summary}"
    );
}

#[test_case(EventKind::TunerDecision ; "tuner_decision")]
#[test_case(EventKind::HuntStart ; "located_hunt")]
#[test_case(EventKind::HuntGroup ; "hunt_group")]
#[test_case(EventKind::HuntEnd ; "hunt_end")]
#[test_case(EventKind::Combination ; "combination")]
#[test_case(EventKind::DeepeningRound ; "deepening_round")]
#[test_case(EventKind::CorePhase ; "core_phase")]
fn a_tuner_decision_citing_a_skipped_trial_leaves_what_follows_unresolved(kind: EventKind) {
    let mut events = machine_session(TARGET_FIT_0, 2);
    let skipped = ends_of(&accounted(&events), "coalesce", &["skipped"]);
    let source = events
        .iter()
        .map(|event| event.seq)
        .find(|seq| skipped.contains(&seq.get()))
        .unwrap();
    let at = events
        .iter()
        .position(|event| event.seq > source && event.payload.kind() == kind)
        .unwrap();
    events[at].cause.push(source);
    let decision = events[at].seq.get();
    let session = accounted(&events);
    let after: Vec<&Value> = of(&session, "derivation", "coalesce")
        .into_iter()
        .filter(|d| n(&d["seq"]) > decision)
        .collect();
    assert!(!after.is_empty(), "{:?}", session.records);
    for d in after {
        assert_eq!(
            (&d["reason"], n(&d["decision"]), n(&d["source"])),
            (&Value::from("after_decision"), decision, source.get()),
            "{d}"
        );
    }
    let ends: Vec<u64> = events
        .iter()
        .filter(|event| event.seq.get() > decision)
        .filter_map(|event| {
            let Payload::CheckingCycle(cycle) = &event.payload else {
                return None;
            };
            (cycle.event == CycleEvent::End).then(|| u64::from(cycle.cycle.get()))
        })
        .collect();
    assert!(!ends.is_empty());
    for cycle in of(&session, "cycle", "coalesce")
        .into_iter()
        .filter(|c| ends.contains(&n(&c["cycle"])))
    {
        assert_eq!(
            (&cycle["status"], n(&cycle["skipped_s"])),
            (&Value::from("diverged"), 0),
            "{cycle}"
        );
    }
}

/// The derivation at `seq` in `events`, to change what the journal recorded for it.
fn chain_at(events: &mut [Event], seq: u64) -> &mut togi_journal::CheckingChain {
    let event = events
        .iter_mut()
        .find(|event| event.seq.get() == seq)
        .unwrap();
    let Payload::CheckingChain(chain) = &mut event.payload else {
        panic!("{seq} is no checking.chain");
    };
    chain
}

#[test]
fn a_derivation_citing_a_skipped_trial_is_unresolved_and_its_passes_witness_nothing() {
    let mut events = machine_session(TARGET_FIT_1, 5);
    let session = accounted(&events);
    let skipped = ends_of(&session, "coalesce", &["skipped"]);
    let source = *skipped.iter().min().unwrap();
    let unkept: HashSet<u64> = of(&session, "derivation", "coalesce")
        .into_iter()
        .map(|d| n(&d["seq"]))
        .collect();
    let witnesses: HashSet<u64> = of(&session, "cycle", "coalesce")
        .into_iter()
        .filter(|c| c["status"] == "covered")
        .flat_map(|c| c["coverage"].as_array().unwrap())
        .flat_map(|coverage| coverage["witnesses"].as_array().unwrap())
        .map(n)
        .collect();
    let slot = |d: &Value| ["cycle", "step", "ccd", "part", "cores"].map(|field| d[field].clone());
    // A kept derivation after the skipped trial whose passes witness a final class that a later
    // derivation of the same position holds.
    let held = session
        .chains
        .iter()
        .enumerate()
        .find(|&(at, d)| {
            let seq = n(&d["seq"]);
            seq > source
                && !unkept.contains(&seq)
                && session.chain_ends[&seq]
                    .iter()
                    .any(|end| witnesses.contains(end))
                && session.chains[at + 1..]
                    .iter()
                    .any(|later| slot(later) == slot(d))
        })
        .map(|(_, d)| n(&d["seq"]))
        .unwrap();
    let cited = events
        .iter()
        .find(|event| event.seq.get() == source)
        .unwrap()
        .seq;
    chain_at(&mut events, held).source_seqs.push(cited);
    let session = accounted(&events);
    let derivation = of(&session, "derivation", "coalesce")
        .into_iter()
        .find(|d| n(&d["seq"]) == held)
        .unwrap();
    assert_eq!(
        (&derivation["reason"], n(&derivation["source"])),
        (&Value::from("source"), source),
        "{derivation}"
    );
    let ends = &session.chain_ends[&held];
    let covered: Vec<&Value> = of(&session, "cycle", "coalesce")
        .into_iter()
        .filter(|c| c["status"] == "covered")
        .flat_map(|c| c["coverage"].as_array().unwrap())
        .collect();
    assert!(
        covered.iter().all(|coverage| {
            coverage["witnesses"]
                .as_array()
                .unwrap()
                .iter()
                .all(|seq| !ends.contains(&n(seq)))
        }),
        "{covered:?}"
    );
    assert!(
        covered.iter().any(|coverage| {
            coverage["rerun"]
                .as_array()
                .unwrap()
                .iter()
                .any(|seq| ends.contains(&n(seq)))
        }),
        "{covered:?}"
    );
}

#[test]
fn a_tie_re_derivation_whose_recorded_order_the_tuner_does_not_give_is_unresolved() {
    let mut events = machine_session(TARGET_FIT_0, 3);
    let tied = of(&accounted(&events), "derivation", "tie")
        .into_iter()
        .find(|d| d["verdict"] == "skipped")
        .map(|d| n(&d["seq"]))
        .unwrap();
    chain_at(&mut events, tied).groups.reverse();
    let session = accounted(&events);
    let derivation = of(&session, "derivation", "tie")
        .into_iter()
        .find(|d| n(&d["seq"]) == tied)
        .unwrap();
    assert_eq!(
        (&derivation["verdict"], &derivation["reason"]),
        (&Value::from("unresolved"), &Value::from("order")),
        "{derivation}"
    );
}

#[test]
fn a_cycle_the_journal_ends_in_is_open_and_its_seconds_unresolved() {
    let events = machine_session(TARGET_FIT_0, 3);
    let first_end = events
        .iter()
        .position(|event| {
            matches!(&event.payload, Payload::CheckingCycle(cycle) if cycle.event == CycleEvent::End)
        })
        .unwrap();
    let whole = accounted(&events);
    let cut = accounted(&events[..first_end]);
    for candidate in ["coalesce", "defer", "tie"] {
        let ended = of(&whole, "cycle", candidate)[0];
        let [open] = of(&cut, "cycle", candidate)[..] else {
            panic!("{candidate}: one cycle: {:?}", cut.records);
        };
        assert_eq!(
            (
                &open["status"],
                n(&open["skipped_s"]),
                n(&open["unresolved_s"])
            ),
            (
                &Value::from("open"),
                0,
                n(&ended["skipped_s"]) + n(&ended["unresolved_s"])
            ),
            "{candidate}: {open} cut from {ended}"
        );
        let summary = of(&cut, "summary", candidate)[0];
        assert_eq!(
            n(&summary["kept_s"]) + n(&summary["skipped_s"]) + n(&summary["unresolved_s"]),
            cut.chain_s,
            "{candidate}: {summary}"
        );
    }
    let ended = of(&whole, "cycle", "coalesce")[0];
    assert!(n(&ended["skipped_s"]) > 0, "{ended}");
}

#[test]
fn a_class_a_later_step_of_its_workload_repeats_needs_only_that_steps_quota() {
    let config = Config::parse(
        r#"{"checking":{"cycle":["R7","R7","R7","R7","R2","R2","R2","R6","R5","R1","R1","R1","R3","R3","R3","R4","R4","R4","R6"]}}"#,
    )
    .unwrap();
    let session = accounted(&machine_session_with(TARGET_FIT_0, 3, config));
    let mut repeated = false;
    for candidate in ["coalesce", "defer", "tie"] {
        for coverage in of(&session, "cycle", candidate)
            .into_iter()
            .filter(|c| c["status"] == "covered")
            .flat_map(|c| c["coverage"].as_array().unwrap())
            .filter(|coverage| coverage["instead_of"].is_null())
        {
            repeated |= n(&coverage["needed"]) > 3;
            assert_eq!(
                n(&coverage["unobserved"]),
                0,
                "{candidate}: the tuner passed the cycle on these passes: {coverage}"
            );
        }
    }
    assert!(repeated, "{:?}", session.records);
}

#[test]
fn a_set_tie_keeps_in_two_steps_of_a_workload_needs_the_later_steps_quota_once() {
    let config = Config::parse(
        r#"{"checking":{"cycle":["R7","R7","R7","R7","R2","R2","R2","R6","R5","R1","R1","R1","R3","R3","R3","R4","R4","R4","R6"]}}"#,
    )
    .unwrap();
    let session = accounted(&machine_session_with(TARGET_FIT_0, 3, config));
    let kept: Vec<u64> = of(&session, "cycle", "tie")
        .into_iter()
        .filter(|c| c["status"] == "covered")
        .flat_map(|c| c["coverage"].as_array().unwrap())
        .filter(|coverage| !coverage["instead_of"].is_null() && coverage["duration_s"] == 120)
        .map(|coverage| n(&coverage["needed"]))
        .collect();
    // Steps 1 and 4 share a workload: the set tie keeps in both needs three passes, then six by the
    // fourth step's quota, not the three of either step alone nor the nine of both summed.
    assert!(kept.contains(&6), "{kept:?}");
    assert!(kept.iter().all(|&needed| needed <= 6), "{kept:?}");
}

#[test]
fn the_tie_candidate_counts_the_set_it_keeps_instead_of_the_tuners() {
    let session = accounted(&machine_session(TARGET_FIT_0, 3));
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
