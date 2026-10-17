//! What the model computes once per change: the tuner's projection and forecast, and the views that join them
//! with what the journal recorded.

use togi_journal::{Member, State as JournalState};
use togi_machine::{CoreId, CoreSet, ObservedOffset, Regime, TargetOffset};
use togi_tuner::{
    CombinationId, CyclePlan, Forecast, GroupState, Limit, Projection, Reached, State as TunerState,
};

use crate::facts::{Facts, secs, trial_cores};
use crate::history::Entry;
use crate::model::{
    ComboView, Confirm, CoreState, CoreView, Count, CyclePlace, HuntCause, HuntFacts, HuntPlace,
    HuntShape, Parking, RecoveryView, TrialView,
};

#[derive(Debug)]
pub(crate) struct Derived {
    pub(crate) projection: Projection,
    pub(crate) forecast: Forecast,
    pub(crate) cores: Vec<CoreView>,
    pub(crate) combos: Vec<ComboView>,
    pub(crate) trial: Option<TrialView>,
    pub(crate) recover: Option<RecoveryView>,
    pub(crate) hunt: Option<HuntFacts>,
    pub(crate) step_hunts: Vec<Vec<u32>>,
    pub(crate) history: Vec<Entry>,
    pub(crate) history_dropped: usize,
    pub(crate) order: Vec<CoreId>,
}

impl Derived {
    pub(crate) fn empty() -> Self {
        Self {
            projection: TunerState::new().project(),
            forecast: Forecast::default(),
            cores: Vec::new(),
            combos: Vec::new(),
            trial: None,
            recover: None,
            hunt: None,
            step_hunts: Vec::new(),
            history: Vec::new(),
            history_dropped: 0,
            order: Vec::new(),
        }
    }

    pub(crate) fn new(journal: &JournalState, tuner: &TunerState, facts: &Facts) -> Self {
        let projection = tuner.project();
        let forecast = tuner.forecast();
        let plan = &projection.cycle_plan;
        let mut trial = facts.current.as_ref().map(|current| {
            let mut view = new_trial(&current.intent, facts, journal);
            if let Some(requirement) = projection.requirement {
                view.count = Count {
                    passed: requirement.passed,
                    index: requirement.trial,
                    of: requirement.needed,
                };
            }
            view.crashed = current.boot_ended;
            view
        });
        if let Some(view) = &mut trial {
            place_in_cycle(plan, view);
        }
        let recover = facts.recover.as_ref().map(|recover| {
            let mut view = recover.intent.as_ref().map(|intent| {
                let mut view = new_trial(intent, facts, journal);
                if view.cycle.is_some_and(|place| place.step.is_none()) {
                    let step = facts.steps.get(&view.id).copied().unwrap_or(0);
                    if let (Some(place), true) = (&mut view.cycle, step > 0) {
                        place.step = Some(step);
                    }
                }
                place_in_cycle(plan, &mut view);
                if let Some(count) = facts.counts.get(&view.id) {
                    view.count = *count;
                }
                view
            });
            RecoveryView {
                crash_at: recover.crash_at,
                reset: recover.reset,
                trial: view.take(),
                end: recover.end.clone(),
            }
        });
        let hunt = projection
            .hunt_plan
            .as_ref()
            .map(|hunt| hunt_facts(hunt, plan, facts, journal));
        if let (Some(view), Some(plan)) = (&mut trial, &projection.hunt_plan) {
            fit_hunt(view, plan);
        }
        let cores = core_views(
            journal,
            tuner,
            &projection,
            facts,
            trial.as_mut(),
            hunt.is_some(),
        );
        let combos = combo_views(&projection, &cores, facts);
        let step_hunts = plan
            .number
            .map(|number| {
                (1..=ord(plan.steps.len()))
                    .map(|step| {
                        facts
                            .check_hunts
                            .get(&(number.get(), step))
                            .cloned()
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let (history, history_dropped) = facts.history.recent();
        let order = journal.cores.iter().map(|core| core.core).collect();
        Self {
            projection,
            forecast,
            cores,
            combos,
            trial,
            recover,
            hunt,
            step_hunts,
            history,
            history_dropped,
            order,
        }
    }
}

fn new_trial(
    intent: &togi_journal::TrialIntent,
    facts: &Facts,
    journal: &JournalState,
) -> TrialView {
    let cores = trial_cores(intent);
    let partial = intent.regime == Regime::R7
        && journal.cores.iter().any(|core| {
            cores.contains(core.core)
                && journal
                    .cores
                    .iter()
                    .any(|other| other.ccd == core.ccd && !cores.contains(other.core))
        });
    TrialView {
        id: intent.trial.clone(),
        regime: intent.regime,
        workload: intent.workload,
        cores,
        parked: CoreSet::default(),
        idle: CoreSet::default(),
        condition: intent.condition,
        activity: intent.activity,
        profile: intent.profile,
        started: facts.starts.get(&intent.trial).copied(),
        crashed: false,
        duration: secs(intent.duration_s),
        partial,
        rerun: intent.rerun,
        retry: intent.retry,
        cycle: intent.cycle.map(|cycle| CyclePlace {
            cycle: cycle.get(),
            step: intent.step.map(std::num::NonZeroU32::get),
            part: None,
            parts: 0,
        }),
        hunt: intent
            .hunt
            .zip(intent.group)
            .map(|(hunt, group)| HuntPlace {
                hunt: hunt.get(),
                group: group.get(),
                part: None,
                parts: 0,
                cut: 0,
                probe: None,
            }),
        core: intent.core,
        offset: intent.offset,
        round: intent.round.map(std::num::NonZeroU32::get),
        count: Count::default(),
    }
}

/// Finds the step and part of its checking cycle a trial loads, and which trial of that part it is.
pub(crate) fn place_in_cycle(plan: &CyclePlan, trial: &mut TrialView) {
    let Some(place) = trial.cycle else {
        return;
    };
    if plan.number.map(std::num::NonZeroU32::get) != Some(place.cycle) {
        return;
    }
    // `part.passed` is credited per quota (see `CyclePart::passed`), so N never exceeds M and
    // surplus short passes never stand in for the long trial.
    let put = |trial: &mut TrialView, step: usize, index: usize| {
        let parts = &plan.steps[step].parts;
        let part = &parts[index];
        let of = part.short + part.long;
        trial.cycle = Some(CyclePlace {
            cycle: place.cycle,
            step: Some(ord(step) + 1),
            part: Some(ord(index) + 1),
            parts: ord(parts.len()),
        });
        trial.count = Count {
            passed: part.passed,
            index: (part.passed + 1).min(of.max(1)),
            of,
        };
    };
    for (i, step) in plan.steps.iter().enumerate() {
        for (j, part) in step.parts.iter().enumerate() {
            if part.running
                && part.cores == trial.cores
                && place.step.is_none_or(|s| s == ord(i) + 1)
            {
                put(trial, i, j);
                return;
            }
        }
    }
    let step = place.step.map_or(plan.current + 1, |s| s as usize);
    if step < 1 || step > plan.steps.len() {
        return;
    }
    if let Some(j) = plan.steps[step - 1]
        .parts
        .iter()
        .position(|part| part.cores == trial.cores)
    {
        put(trial, step - 1, j);
    }
}

pub(crate) fn fit_hunt(trial: &mut TrialView, plan: &togi_tuner::HuntPlan) {
    let Some(place) = &mut trial.hunt else {
        return;
    };
    if place.hunt != plan.number {
        return;
    }
    place.parts = ord(plan.parts.len());
    place.cut = plan.cut;
    for (i, part) in plan.parts.iter().enumerate() {
        if part.running {
            place.part = Some(ord(i) + 1);
        }
    }
    for group in &plan.groups {
        if group.number == place.group {
            place.probe = group.probe;
        }
    }
}

/// Where a hunt's parked profile puts each core. A core parked at 0 is parked at 0 whatever it failed at; one
/// parked at its failing offset is held, or kept when it is a candidate; the rest sit shallower than they failed.
pub(crate) fn parking_of(shape: &HuntShape, candidates: CoreSet) -> Parking {
    let mut parking = Parking::default();
    for (core, parked) in shape.parked.iter() {
        if parked == TargetOffset::ZERO {
            parking.zero.insert(core);
        } else if shape.failing.get(core) == Some(parked) {
            if candidates.contains(core) {
                parking.kept.insert(core);
            } else {
                parking.held.insert(core);
            }
        } else {
            parking.shallow.push((core, parked));
        }
    }
    parking
}

pub(crate) fn hunt_facts(
    plan: &togi_tuner::HuntPlan,
    cycle: &CyclePlan,
    facts: &Facts,
    journal: &JournalState,
) -> HuntFacts {
    let failure = facts.by_failure.get(&plan.failure);
    let mut cause = HuntCause {
        at: failure.and_then(|f| f.at),
        trial: None,
        end: None,
        signal: failure.and_then(|f| f.signal),
        core: failure.and_then(|f| f.core),
        known: failure.is_some_and(|f| f.known),
        carried: failure.is_some_and(|f| f.carried),
        regime: failure.and_then(|f| f.regime),
        rerun_of: false,
        trial_number: 0,
    };
    if let Some(intent) = plan
        .trial
        .as_ref()
        .and_then(|trial| facts.intents.get(trial))
    {
        let mut view = new_trial(intent, facts, journal);
        cause.rerun_of = intent.rerun;
        if let Some(history) = facts.failed.get(&intent.trial) {
            cause.trial_number = history.trial;
            view.count = Count {
                passed: history.passed,
                index: history.trial,
                of: history.needed,
            };
        }
        place_in_cycle(cycle, &mut view);
        cause.end = facts.ends.get(&intent.trial).cloned();
        cause.trial = Some(view);
    }
    let parking = facts
        .shapes
        .get(&plan.number)
        .map(|shape| parking_of(shape, plan.candidates));
    HuntFacts {
        started: facts.hunt_started.get(&plan.number).copied(),
        cause,
        parking,
        group_signals: facts
            .group_signals
            .iter()
            .filter(|((hunt, _), _)| *hunt == plan.number)
            .map(|((_, group), signal)| (*group, *signal))
            .collect(),
    }
}

fn core_views(
    journal: &JournalState,
    tuner: &TunerState,
    projection: &Projection,
    facts: &Facts,
    mut trial: Option<&mut TrialView>,
    hunting: bool,
) -> Vec<CoreView> {
    let turns = &projection.search_turns;
    let hunt = projection.hunt_plan.as_ref().filter(|_| hunting);
    let mut views = Vec::new();
    for facts_core in &journal.cores {
        let id = facts_core.core;
        let Some(c) = projection.cores.iter().find(|c| c.core == id) else {
            continue;
        };
        let mut v = CoreView {
            id,
            ccd: facts_core.ccd,
            state: CoreState::Waiting,
            queued: facts.queued.get(&id).copied(),
            applied: facts
                .applied
                .get(&id)
                .copied()
                .unwrap_or(ObservedOffset(c.offset.get())),
            profile: c.offset,
            solo: c.solo_limit,
            pass: c.pass,
            next: None,
            fail: c.failure_point,
            holder: None,
            gave_back: None,
            group_fails: Vec::new(),
            returns_to: None,
            confirm: None,
            loaded: false,
            judged: false,
        };
        if v.solo.is_some_and(|solo| v.profile.get() > solo.get()) {
            v.gave_back = facts.backs.get(&id).copied().flatten();
        }
        v.state = match c.phase {
            Some(togi_journal::CorePhase::AtLimit) => CoreState::AtLimit,
            Some(togi_journal::CorePhase::HasRoom) => CoreState::HasRoom,
            Some(togi_journal::CorePhase::Search) | None => CoreState::Waiting,
        };
        if !turns.is_empty() && c.phase != Some(togi_journal::CorePhase::Search) && v.solo.is_some()
        {
            v.state = CoreState::Found;
        }
        for turn in turns.iter().filter(|turn| turn.core == id) {
            v.next = Some(turn.offset);
            v.state = CoreState::Search;
            if turn.confirm {
                v.state = CoreState::Confirm;
                v.confirm = Some(Confirm {
                    offset: turn.offset,
                    light: turn.light,
                    heavy: turn.heavy,
                    needed: turn.needed,
                });
            }
            if !turn.running && c.offset.get() == 0 && c.pass.is_none() {
                v.state = CoreState::Waiting;
            }
        }
        if hunt.is_some_and(|hunt| hunt.candidates.contains(id)) {
            v.state = CoreState::Suspect;
        }
        v.holder = tuner.core_limit(id);
        if let Some(trial) = trial.as_deref_mut() {
            trial_role(&mut v, trial, hunt, facts);
        }
        if let Some(hunt) = hunt {
            v.group_fails = group_failures(hunt, id);
        }
        views.push(v);
    }
    views.sort_by_key(|v| v.id);
    if let Some(trial) = trial {
        trial.idle = views
            .iter()
            .map(|v| v.id)
            .filter(|id| !trial.cores.contains(*id) && !trial.parked.contains(*id))
            .collect();
    }
    views
}

pub(crate) fn trial_role(
    v: &mut CoreView,
    trial: &mut TrialView,
    hunt: Option<&togi_tuner::HuntPlan>,
    facts: &Facts,
) {
    let started = trial.started.is_some();
    v.loaded = started && trial.cores.contains(v.id);
    v.judged = v.loaded;
    let Some(hunt) = hunt else {
        return;
    };
    if trial.condition != togi_machine::Condition::Parked {
        return;
    }
    let mut member = false;
    for group in hunt
        .groups
        .iter()
        .filter(|g| g.outcome == GroupState::Running)
    {
        member = group.cores.contains(v.id)
            || group.probe.is_some_and(|p| p.core == v.id)
            || group.held.iter().any(|m| m.core == v.id);
        if group.probe.is_some_and(|p| p.core == v.id) {
            v.state = CoreState::Probe;
        }
    }
    if member {
        if v.state != CoreState::Probe {
            v.state = CoreState::Member;
        }
        v.judged = started;
        return;
    }
    let parked = facts.shapes.get(&hunt.number).is_some_and(|shape| {
        let held = shape.parked.get(v.id);
        held.is_some() && trial.profile.get(v.id) == held
    });
    v.judged = v.loaded && !parked;
    if parked {
        trial.parked.insert(v.id);
    }
    if hunt.candidates.contains(v.id) {
        v.state = CoreState::Parked;
        v.returns_to = Some(v.profile);
    }
}

fn combo_views(projection: &Projection, cores: &[CoreView], facts: &Facts) -> Vec<ComboView> {
    let Some(constraints) = &projection.constraints else {
        return Vec::new();
    };
    constraints
        .combinations
        .iter()
        .map(|combination| {
            let id: CombinationId = combination.id();
            let recorded = facts.combos.get(&id);
            let members: Vec<Member> = combination
                .members()
                .iter()
                .map(|(core, offset)| Member { core, offset })
                .collect();
            let hunt = recorded.map_or(0, |r| r.hunt);
            let mut view = ComboView {
                id,
                members: members.clone(),
                hunt,
                probed: facts.probes.contains(&hunt),
                fallback: recorded.is_some_and(|r| r.fallback),
                holds: None,
                clear: CoreSet::default(),
            };
            for core in cores {
                let Some(member) = members.iter().find(|member| member.core == core.id) else {
                    continue;
                };
                if core.profile.get() > member.offset.get() {
                    view.clear.insert(member.core);
                }
                if core.holder == Some(Limit::Reached(Reached::Combination(id))) {
                    view.holds = Some(Member {
                        core: core.id,
                        offset: core.profile,
                    });
                }
            }
            view
        })
        .collect()
}

fn ord(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The offsets at which a group trial of the running hunt failed with `core` loaded.
pub(crate) fn group_failures(hunt: &togi_tuner::HuntPlan, core: CoreId) -> Vec<TargetOffset> {
    let mut offsets = Vec::new();
    for group in &hunt.groups {
        let member =
            group.cores.contains(core) || group.probe.is_some_and(|probe| probe.core == core);
        if group.outcome == GroupState::Failed && member {
            if let Some(offset) = group.profile.get(core) {
                if !offsets.contains(&offset) {
                    offsets.push(offset);
                }
            }
        }
    }
    offsets
}
