use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;

use togi_journal::{
    Activity, Attribution, Combination, CorePhase, CycleEvent, Decision, Event, Outcome, Payload,
    Seq, TrialId, TrialIntent,
};
use togi_machine::{CcdId, Condition, CoreId, PerCore, Regime, Signal, TargetOffset, workloads};
use togi_session::Stop;
use togi_sim::model::Script;
use togi_sim::{MachineDefinition, regime_index};
use togi_simrun::{Observe, Observer};

use crate::common::{Bench, finish, machine_file};

const VOLTAGE_TARGETED: &str = "voltage-targeted R7 backoff";

fn at(profile: &PerCore<TargetOffset>, core: CoreId) -> i64 {
    profile.get(core).map_or(0, TargetOffset::get)
}

fn target_load(intent: &TrialIntent) -> bool {
    intent.regime == Regime::R7
        && intent.workload.as_str() == workloads(Regime::R7)[0].id
        && intent.cores.len() == 8
        && intent.cores.iter().all(|core| core.get() < 8)
}

fn named_target_observer(bench: &Bench, definition: MachineDefinition) -> Observer {
    let sim = bench.sim.clone();
    let limits = definition.core;
    let first = CoreId::new(4).unwrap();
    let named = CoreId::new(2).unwrap();
    let mut long_failures = 0;
    Box::new(move |event| {
        let Payload::TrialIntent(intent) = &event.payload else {
            return Observe::Continue;
        };
        let (signal, core) = if intent.condition == Condition::Alone {
            let core = intent.core.unwrap();
            let limit = limits[core.index()].alone[regime_index(intent.regime)];
            (
                (at(&intent.profile, core) < limit).then_some(Signal::ComputationError),
                core,
            )
        } else if target_load(intent) && long_failures < 2 {
            // The first long failure raises the top requester by one count. Its rerun earns
            // n short passes at one exact profile before the second long failure names core 2.
            // That core's real request is then 10.6 mV below the passing full-CCD top: 3 counts.
            let core = if long_failures == 0 { first } else { named };
            let signal = (intent.duration_s == 300).then_some(Signal::ComputationError);
            long_failures += usize::from(signal.is_some());
            (signal, core)
        } else {
            return Observe::Continue;
        };
        sim.script_trial(
            intent.trial.as_str(),
            Script {
                signal,
                at_s: 60.0,
                core,
                reset: None,
                then_crash: false,
            },
        )
        .unwrap();
        Observe::Continue
    })
}

fn assert_named_target(events: &[Event], needed: u32) {
    let intents: BTreeMap<_, _> = events
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::TrialIntent(intent) => Some((&intent.trial, intent)),
            _ => None,
        })
        .collect();
    let ends: BTreeMap<_, _> = events
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::TrialEnd(end) => Some((event.seq, (intents[&end.trial], end))),
            _ => None,
        })
        .collect();
    let named = CoreId::new(2).unwrap();
    let Some((failure_seq, intent)) = events.iter().find_map(|event| {
        let Payload::Failure(failure) = &event.payload else {
            return None;
        };
        let intent = *intents.get(failure.trial.as_ref()?)?;
        (failure.core == Some(named)
            && failure.attribution == Attribution::Attributed
            && failure.signal == Signal::ComputationError
            && target_load(intent)
            && intent.duration_s == 300)
            .then_some((event.seq, intent))
    }) else {
        panic!("no scripted named full-CCD computation error");
    };
    let failed = ends
        .values()
        .find(|(_, end)| end.trial == intent.trial)
        .unwrap()
        .1;
    assert_eq!(failed.signal, Some(Signal::ComputationError));
    assert_eq!(failed.core, Some(named));
    let failing_mhz = failed.ccd_mhz[&CcdId(0)];
    let request = failed.voltage_requests_v.get(named).unwrap();
    let Some((decision_event, decision)) = events.iter().find_map(|event| match &event.payload {
        Payload::TunerDecision(decision)
            if event.cause.first() == Some(&failure_seq)
                && decision.decision == Decision::Backoff =>
        {
            Some((event, decision))
        }
        _ => None,
    }) else {
        panic!("no decision answering the scripted named failure");
    };
    assert_eq!(decision.core, named);
    assert_eq!(decision.to_offset.get() - decision.from_offset.get(), 3);
    assert!(decision.reason.contains(VOLTAGE_TARGETED));

    let passes: Vec<_> = decision_event
        .cause
        .iter()
        .filter_map(|seq| ends.get(seq))
        .filter(|(_, end)| end.outcome == Outcome::Pass)
        .collect();
    assert_eq!(u32::try_from(passes.len()).unwrap(), needed);
    for (passed_intent, end) in passes {
        assert!(target_load(passed_intent));
        assert_eq!(passed_intent.profile, intent.profile);
        assert_eq!(passed_intent.duration_s, 120);
        assert!(passed_intent.rerun);
        assert!(end.ccd_mhz[&CcdId(0)] >= failing_mhz.saturating_sub(25));
        let top = end
            .voltage_requests_v
            .iter()
            .map(|(_, volts)| volts)
            .reduce(f64::max)
            .unwrap();
        assert!(
            (0.0072..0.0108).contains(&(top - request)),
            "passing CCD top {top} and failing request {request} must require three counts"
        );
    }
}

#[test]
fn sixteen_cores_reach_a_clean_cycle() {
    let definition = machine_file("shared-voltage");
    let bench = Bench::new(&definition, 1000);
    let mut input = bench.input();
    let needed = input.config.evidence().trials().get();
    input.verify_every = None;
    input.observer = Some(named_target_observer(&bench, definition));
    let finished = finish(input);
    assert_eq!(finished.stop, Some(Stop::Cycles));
    assert!(finished.errors.is_empty(), "{:?}", finished.errors);
    assert_eq!(bench.sim.violations(), Vec::<String>::new());

    let events = bench.events();
    let projection = togi_tuner::replay(&events).unwrap().project();
    assert_eq!(projection.cores.len(), 16);
    assert!(projection.deepening.is_none(), "{:?}", projection.deepening);
    assert!(projection.checking.profile.is_some() && projection.clean_cycles > 0);
    assert_named_target(&events, needed);

    let backed_off: BTreeSet<CoreId> = events
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::TunerDecision(decision) if decision.decision == Decision::Backoff => {
                Some(decision.core)
            }
            _ => None,
        })
        .collect();
    for core in &projection.cores {
        let short_of_failure = core.phase == Some(CorePhase::HasRoom)
            && backed_off.contains(&core.core)
            && core
                .failure_point
                .is_some_and(|failure| core.offset > failure);
        assert!(
            core.phase == Some(CorePhase::AtLimit) || short_of_failure,
            "core {} is {:?} at {:?}, want at its limit or backed off short of its failure point",
            core.core,
            core.phase,
            core.offset
        );
    }

    let mut passed_full_cycle = false;
    let mut parts: BTreeMap<Option<NonZeroU32>, BTreeMap<u32, u32>> = BTreeMap::new();
    let mut combinations: Vec<&Combination> = Vec::new();
    let mut failed: BTreeMap<CoreId, i64> = BTreeMap::new();
    let mut multi_r7: BTreeMap<&TrialId, bool> = BTreeMap::new();
    let mut named: BTreeMap<Seq, (CoreId, Signal)> = BTreeMap::new();
    let mut measured_reorder = false;
    let mut named_multi_count = false;
    let mut named_backoffs = 0;
    for event in &events {
        match &event.payload {
            Payload::CheckingCycle(cycle) => {
                passed_full_cycle |= cycle.event == CycleEvent::End && cycle.passed && cycle.full;
            }
            Payload::TrialIntent(intent) => {
                multi_r7.insert(
                    &intent.trial,
                    intent.regime == Regime::R7 && intent.cores.len() > 1,
                );
                if intent.regime == Regime::R7 && intent.activity == Some(Activity::Checking) {
                    *parts
                        .entry(intent.cycle)
                        .or_default()
                        .entry(intent.duration_s)
                        .or_default() += 1;
                }
                for (core, failure) in &failed {
                    assert!(
                        at(&intent.profile, *core) > *failure,
                        "trial {} reaches failure point of core {core} at {failure}",
                        intent.trial
                    );
                }
                for combination in &combinations {
                    let reaches = combination
                        .members
                        .iter()
                        .all(|member| at(&intent.profile, member.core) <= member.offset.get());
                    assert!(
                        !reaches,
                        "trial {} reaches combination C{}",
                        intent.trial, combination.combination
                    );
                }
            }
            Payload::Failure(failure) => {
                let multi = failure
                    .trial
                    .as_ref()
                    .is_some_and(|trial| multi_r7.get(trial).copied().unwrap_or(false));
                let attributed = failure.attribution == Attribution::Attributed;
                if let (false, true, Some(core), Some(offset)) =
                    (multi, attributed, failure.core, failure.offset)
                {
                    failed.insert(core, offset.get());
                }
                if let (true, true, Some(core)) = (multi, attributed, failure.core) {
                    named.insert(event.seq, (core, failure.signal));
                }
            }
            Payload::TunerDecision(decision) => {
                if let Some(failure) = decision.failure_point {
                    failed.insert(decision.core, failure.get());
                }
                let Some(cause) = event.cause.first() else {
                    continue;
                };
                if let Some((core, signal)) = named.get(cause)
                    && decision.decision == Decision::Backoff
                {
                    assert_eq!(
                        decision.core,
                        *core,
                        "named R7 failure #{} on core {core} backed off core {}",
                        cause.get(),
                        decision.core
                    );
                    named_backoffs += 1;
                    named_multi_count |= *signal == Signal::ComputationError
                        && decision.to_offset.get() - decision.from_offset.get() > 1
                        && decision.reason.contains(VOLTAGE_TARGETED);
                }
            }
            Payload::CheckingChain(chain) => {
                let shallowest = chain
                    .groups
                    .iter()
                    .flat_map(togi_machine::CoreSet::iter)
                    .map(|core| at(&chain.profile, core))
                    .fold(-50, i64::max);
                measured_reorder |= !chain.source_seqs.is_empty()
                    && chain.groups.first().is_some_and(|group| {
                        group
                            .iter()
                            .any(|core| at(&chain.profile, core) != shallowest)
                    });
            }
            Payload::HuntStart(hunt) => {
                assert!(
                    !(hunt.regime == Regime::R7 && hunt.cores.len() > 1),
                    "multi-core R7 failure started hunt {}",
                    hunt.hunt
                );
            }
            Payload::Combination(combination) => combinations.push(combination),
            _ => {}
        }
    }
    assert!(
        measured_reorder,
        "no checking chain ordered its partial by measured requests that differ from offset order"
    );
    assert!(
        named_backoffs > 0,
        "no named multi-core R7 failure was backed off"
    );
    assert!(
        named_multi_count,
        "no named R7 computation error moved its core by a multi-count voltage-targeted backoff"
    );
    assert!(passed_full_cycle, "no passed full cycle end");
    assert!(
        parts.values().any(|durations| {
            let count = |seconds| durations.get(&seconds).copied().unwrap_or(0);
            count(120) >= 9 && count(300) >= 6 && count(600) >= 3
        }),
        "R7 parts did not run three trials of 120s and long 300/300/600s: {parts:?}"
    );
}
