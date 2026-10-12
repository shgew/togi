use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;

use togi_journal::{
    Activity, Attribution, Combination, CorePhase, CycleEvent, Decision, Payload, Seq, TrialId,
};
use togi_machine::{CoreId, PerCore, Regime, TargetOffset};
use togi_session::Stop;

use crate::common::{Bench, finish, machine_file};

const VOLTAGE_TARGETED: &str = "voltage-targeted R7 backoff";

fn at(profile: &PerCore<TargetOffset>, core: CoreId) -> i64 {
    profile.get(core).map_or(0, TargetOffset::get)
}

#[test]
fn sixteen_cores_reach_a_clean_cycle() {
    let bench = Bench::new(&machine_file("shared-voltage"), 1000);
    let mut input = bench.input();
    input.verify_every = None;
    let finished = finish(input);
    assert_eq!(finished.stop, Some(Stop::Cycles));
    assert!(finished.errors.is_empty(), "{:?}", finished.errors);
    assert_eq!(bench.sim.violations(), Vec::<String>::new());

    let events = bench.events();
    let projection = togi_tuner::replay(&events).unwrap().project();
    assert_eq!(projection.cores.len(), 16);
    assert!(projection.deepening.is_none(), "{:?}", projection.deepening);
    assert!(projection.checking.profile.is_some() && projection.clean_cycles > 0);

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
    let mut named: BTreeMap<Seq, CoreId> = BTreeMap::new();
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
                    named.insert(event.seq, core);
                }
            }
            Payload::TunerDecision(decision) => {
                if let Some(failure) = decision.failure_point {
                    failed.insert(decision.core, failure.get());
                }
                let Some(cause) = event.cause.first() else {
                    continue;
                };
                if let Some(core) = named.get(cause)
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
                    named_multi_count |= decision.to_offset.get() - decision.from_offset.get() > 1
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
