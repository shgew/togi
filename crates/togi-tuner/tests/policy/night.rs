//! The night model: a night opens at its first plan, runs until it closes, ends on its own only at a dead
//! end or when no runnable cell is left, and owes its closure to the next query.

use std::collections::{BTreeMap, BTreeSet};

use togi_journal::{
    Activity, CommandReset, NightId, Payload, QualificationState, Queue, ResetTarget, Seq,
    Shutdown, ShutdownReason, TaskId, Tested, UnavailableBackend,
};
use togi_machine::{Backend, BootId};
use togi_tuner::day::{Invocation, Pause};
use togi_tuner::night::Roles;
use togi_tuner::policy::{Action, DayQuery, EndReason, TrialPlan};

use crate::fixture::{Ending, Journal, at, cores};

/// Runs `trials` passing trials, returning the night they ran in.
fn run_trials(journal: &mut Journal, trials: i64) -> NightId {
    let mut night = None;
    for step in 0..trials {
        let plan = journal.until_trial(at(100 + step));
        night = Some(plan.night);
        journal.run(&plan, Ending::Pass);
    }
    night.unwrap()
}

fn seq(value: u64) -> Seq {
    Seq::new(value).unwrap()
}

/// Excluded backends, each with an inconclusive streak of two events from `first`.
fn excluded(backends: &[(Backend, u64)]) -> BTreeMap<Backend, Vec<Seq>> {
    backends
        .iter()
        .map(|&(backend, first)| (backend, vec![seq(first), seq(first + 1)]))
        .collect()
}

/// The night plans appended since `from`, as (night, causes).
fn plans_since(journal: &Journal, from: usize) -> Vec<(Option<NightId>, Vec<Seq>)> {
    journal.events[from..]
        .iter()
        .filter_map(|event| {
            if let Payload::NightPlan(plan) = &event.payload {
                Some((plan.night, event.cause.clone()))
            } else {
                None
            }
        })
        .collect()
}

fn cold_boot(boot: &str) -> DayQuery {
    DayQuery {
        boot: BootId::from(boot.to_owned()),
        invocation: Invocation::ColdBoot {
            pause: Pause::Running,
        },
        now: at(50_000),
    }
}

#[test]
fn a_later_boots_start_closes_the_night_and_the_day_owes_its_closure_first() {
    let mut journal = Journal::ready();
    let night = run_trials(&mut journal, 3);
    let closing = journal.boot_start("day1");
    let projected = journal.state.project().night.unwrap();
    assert_eq!(projected.night, night);
    assert_eq!(projected.closed_by, Some(closing));
    let from = journal.events.len();
    let owed = journal.state.day(&cold_boot("day1")).unwrap();
    let [first] = owed.as_slice() else {
        panic!("the closure comes one commitment at a time, got {owed:?}");
    };
    assert!(first.causes.contains(&closing), "{first:?}");
    // Drain the closure: each commitment cites the boot that closed the night.
    for _ in 0..10 {
        let owed = journal.state.day(&cold_boot("day1")).unwrap();
        let [decision] = owed.as_slice() else {
            break;
        };
        let payload = Payload::from(decision.payload.clone());
        if !matches!(
            payload,
            Payload::NightPlan(_) | Payload::NightQualification(_) | Payload::NightReproduction(_)
        ) {
            break;
        }
        assert!(decision.causes.contains(&closing), "{decision:?}");
        journal.append(payload, &decision.causes);
    }
    let plans = plans_since(&journal, from);
    let [(closed, causes)] = plans.as_slice() else {
        panic!("one final plan, got {plans:?}");
    };
    assert_eq!(*closed, Some(night));
    assert!(causes.contains(&closing));
    // The closure is owed once.
    let owed = journal.state.day(&cold_boot("day1")).unwrap();
    assert!(
        owed.iter().all(|decision| !matches!(
            Payload::from(decision.payload.clone()),
            Payload::NightPlan(_)
        )),
        "{owed:?}"
    );
}

#[test]
fn the_next_night_opens_after_the_closed_one_is_closed() {
    let mut journal = Journal::ready();
    let first = run_trials(&mut journal, 3);
    let closing = journal.boot_start("day1");
    journal.reboot("tuning2");
    let from = journal.events.len();
    let plan = journal.until_trial(at(60_000));
    assert_ne!(plan.night, first);
    let plans = plans_since(&journal, from);
    // The closed night's final plan comes before the plan that opens the next one.
    assert_eq!(plans.first().map(|(night, _)| *night), Some(Some(first)));
    assert!(plans[0].1.contains(&closing));
    assert!(plans.iter().any(|(night, _)| night.is_none()));
}

#[test]
fn a_crash_reboot_back_into_tuning_continues_the_night() {
    let mut journal = Journal::ready();
    let night = run_trials(&mut journal, 3);
    journal.reboot("tuning-after-crash");
    let from = journal.events.len();
    let plan = journal.until_trial(at(5_000));
    assert_eq!(plan.night, night);
    assert!(
        plans_since(&journal, from)
            .iter()
            .all(|(planned, _)| *planned == Some(night))
    );
    assert_eq!(journal.state.project().night.unwrap().closed_by, None);
}

#[test]
fn the_operators_stop_closes_the_night_and_a_restart_opens_a_new_one() {
    let mut journal = Journal::ready();
    let night = run_trials(&mut journal, 3);
    let stop = journal.append(
        Shutdown {
            reason: ShutdownReason::Signal,
            kernel_cursor: None,
            kernel_error: None,
            cycles: None,
        }
        .into(),
        &[],
    );
    assert_eq!(journal.state.project().night.unwrap().closed_by, Some(stop));
    let plan = journal.until_trial(at(5_000));
    assert_ne!(plan.night, night);
}

#[test]
fn a_reset_of_every_core_closes_the_night() {
    let mut journal = Journal::ready();
    run_trials(&mut journal, 2);
    let reset = journal.append(
        CommandReset {
            target: ResetTarget::All,
        }
        .into(),
        &[],
    );
    assert_eq!(
        journal.state.project().night.unwrap().closed_by,
        Some(reset)
    );
}

#[test]
fn a_night_whose_every_cell_needs_an_excluded_backend_ends_as_unavailable() {
    let mut journal = Journal::ready();
    let every = excluded(&[(Backend::Mprime, 40), (Backend::Ycruncher, 50)]);
    let from = journal.events.len();
    assert_eq!(
        journal.until_end(at(100), &every).unwrap_err(),
        EndReason::Unavailable
    );
    // Before the night ends, its plan records both backends and cites what excluded them.
    let Payload::NightPlan(plan) = &journal.events.last().unwrap().payload else {
        panic!("expected the plan before the end");
    };
    assert_eq!(
        plan.unavailable,
        vec![
            UnavailableBackend {
                backend: Backend::Mprime,
                causes: vec![seq(40), seq(41)],
            },
            UnavailableBackend {
                backend: Backend::Ycruncher,
                causes: vec![seq(50), seq(51)],
            },
        ]
    );
    let cause = &journal.events.last().unwrap().cause;
    for excluded in [40, 41, 50, 51] {
        assert!(cause.contains(&seq(excluded)), "{cause:?}");
    }
    assert!(journal.events.len() > from);
    // Asked again, the plan stands: the night ends without another commitment.
    let before = journal.events.len();
    assert_eq!(
        journal.until_end(at(101), &every).unwrap_err(),
        EndReason::Unavailable
    );
    assert_eq!(journal.events.len(), before);
    // With the backends back, the same night runs and its next plan records none.
    let plan = journal.until_end(at(102), &BTreeMap::new()).unwrap();
    let Payload::NightPlan(latest) = &journal.events.last().unwrap().payload else {
        panic!("expected a plan before the trial");
    };
    assert!(latest.unavailable.is_empty());
    assert_eq!(Some(plan.night), latest.night);
}

#[test]
fn an_excluded_backend_leaves_the_night_running_on_the_other() {
    let mut journal = Journal::ready();
    let mprime = excluded(&[(Backend::Mprime, 7)]);
    let mut recorded = false;
    for step in 0..40_i64 {
        let Ok(plan) = journal.until_end(at(100 + step), &mprime) else {
            // Every cell may need mprime: then the night ends as unavailable, never otherwise.
            return;
        };
        assert!(
            plan.workloads()
                .iter()
                .all(|workload| workload.backend() != Backend::Mprime),
            "{plan:?}"
        );
        recorded |= journal.events.iter().any(|event| {
            matches!(&event.payload, Payload::NightPlan(plan)
                if plan.unavailable.iter().any(|excluded| excluded.backend == Backend::Mprime))
        });
        journal.run(&plan, Ending::Pass);
    }
    assert!(recorded, "no plan recorded the excluded backend");
}

/// The committed cap S of `ccd`, from the projection.
fn cap_of(journal: &Journal, ccd: u8) -> Option<i64> {
    let json = serde_json::to_value(journal.state.project()).unwrap();
    json["ccds"][usize::from(ccd)]["cap"]["offset"].as_i64()
}

#[test]
fn a_passing_night_never_ends_and_falls_back_to_a_cap_probe_one_count_deeper() {
    let mut journal = Journal::ready();
    let plan = journal.until_filler(|_| Ending::Pass);
    assert!(!plan.retry);
    // Every CCD's S is far from its censored edge: the probe runs S − 1 on one CCD alone.
    assert_eq!(plan.activity, Activity::Mapping);
    let Tested::Isolated { ccd, offset } = plan.tested else {
        panic!("expected a cap probe, got {plan:?}");
    };
    assert_eq!(
        Some(offset.get()),
        cap_of(&journal, ccd.get()).map(|cap| cap - 1)
    );
}

#[test]
fn each_filler_trial_advances_the_durable_rotation_across_nights() {
    let mut journal = Journal::ready();
    let mut plan = journal.until_filler(|_| Ending::Pass);
    let first_night = plan.night;
    let mut cells = Vec::new();
    for step in 0..6_i64 {
        assert_eq!(plan.task.queue, Queue::Filler);
        assert_eq!(plan.night, first_night);
        cells.push(plan.cell);
        journal.run(&plan, Ending::Pass);
        assert_eq!(
            journal.state.project().night.unwrap().filler,
            u32::try_from(step + 1).unwrap()
        );
        plan = journal.until_trial(at(10_000 + step));
    }
    // One filler task for the night, whose trials rotate through the matrix rather than repeat one cell.
    let distinct: BTreeSet<String> = cells.iter().map(|cell| format!("{cell:?}")).collect();
    assert!(distinct.len() > 1, "{cells:?}");
    // The operator stops the night; the next night's filler continues the cursor instead of restarting it.
    journal.append(
        Shutdown {
            reason: ShutdownReason::Signal,
            kernel_cursor: None,
            kernel_error: None,
            cycles: None,
        }
        .into(),
        &[],
    );
    let next = journal.until_filler(|_| Ending::Pass);
    assert_ne!(next.night, first_night);
    assert_eq!(next.task.ordinal, 1);
    assert_eq!(journal.state.project().night.unwrap().filler, 6);
}

#[test]
fn with_no_admissible_cap_probe_the_filler_reruns_ds_exact_cells_round_robin() {
    let mut journal = Journal::ready();
    // Every mapping trial at -5 or deeper fails on its tested CCD, so U is observed at -5 and S can be no
    // deeper than U + 5 = 0: S − 1 would cross the margin.
    let mapping_fails = |plan: &TrialPlan| match plan.tested {
        Tested::Isolated { offset, .. }
            if plan.task.queue == Queue::Mapping && offset.get() <= -5 =>
        {
            Ending::Fail(None)
        }
        Tested::Isolated { .. } | Tested::Exact {} => Ending::Pass,
    };
    let mut plan = journal.until_filler(mapping_fails);
    let projection = serde_json::to_value(journal.state.project()).unwrap();
    for ccd in 0..2 {
        let edge = &projection["ccds"][ccd]["edge"];
        assert_eq!(edge["kind"], "observed", "{edge}");
        let u = edge["offset"].as_i64().unwrap();
        let s = projection["ccds"][ccd]["cap"]["offset"].as_i64().unwrap();
        // S − 1 would cross the edge margin: no cap probe is admissible.
        assert!(s - 1 < u + 5, "S {s}, U {u}");
    }
    // The journal committed each edge from a mapping failure and each cap from its edge.
    for ccd in 0..2_u8 {
        assert!(journal.events.iter().any(|event| matches!(&event.payload,
            Payload::CcdEdge(change) if change.ccd.get() == ccd
                && matches!(change.edge, togi_journal::Edge::Observed { .. }))));
        assert!(journal.events.iter().any(|event| matches!(&event.payload,
            Payload::CcdCap(cap) if cap.ccd.get() == ccd
                && matches!(cap.edge, togi_journal::Edge::Observed { .. }))));
    }
    let mut cells = Vec::new();
    for step in 0..4_i64 {
        assert_eq!(plan.task.queue, Queue::Filler);
        assert_eq!(plan.activity, Activity::Qualification);
        assert_eq!(plan.tested, Tested::Exact {});
        assert!(plan.profile.is_zero(), "D is still all-zero: {plan:?}");
        cells.push(format!("{:?}", plan.cell));
        journal.run(&plan, Ending::Pass);
        plan = journal.until_trial(at(10_000 + step));
    }
    let distinct: BTreeSet<&String> = cells.iter().collect();
    assert!(distinct.len() > 1, "{cells:?}");
}

#[test]
fn switching_away_from_a_qualification_task_commits_its_progress_first() {
    let mut journal = Journal::ready();
    let mut previous: Option<TaskId> = None;
    let mut switches = 0;
    for step in 0..3_500_i64 {
        let from = journal.events.len();
        let plan = journal.until_trial(at(100 + step));
        if let Some(previous) = previous
            && matches!(
                previous.queue,
                Queue::Qualification | Queue::Requalification
            )
            && previous != plan.task
        {
            switches += 1;
            let committed = journal.events[from..].iter().any(|event| {
                matches!(&event.payload, Payload::NightQualification(record)
                    if matches!(record.state, QualificationState::Progress { .. }
                        | QualificationState::Complete { .. }
                        | QualificationState::Failed { .. }))
            });
            assert!(
                committed,
                "no progress before switching from {previous:?} to {:?}",
                plan.task
            );
        }
        if plan.task.queue == Queue::Filler {
            break;
        }
        previous = Some(plan.task);
        journal.run(&plan, Ending::Pass);
    }
    assert!(
        switches > 0,
        "the night never switched away from a qualification task"
    );
}

#[test]
fn no_query_before_a_night_owes_a_closure() {
    let journal = Journal::ready();
    let Ok(Action::Decide(decision)) = journal.next(at(100)) else {
        panic!("expected the opening plan");
    };
    let Payload::NightPlan(plan) = Payload::from(decision.payload) else {
        panic!("expected a night.plan");
    };
    assert_eq!(plan.night, None);
}

#[test]
fn an_isolated_all_thread_trial_loads_only_the_tested_ccd() {
    let mut journal = Journal::ready();
    let cores = cores();
    let mut checked = 0;
    for step in 0..3_500_i64 {
        let plan = journal.until_trial(at(100 + step));
        if plan.task.queue == Queue::Filler {
            break;
        }
        if let (Tested::Isolated { ccd, .. }, Roles::AllThreads { .. }) =
            (plan.tested, plan.cell.roles)
        {
            let on_ccd: Vec<_> = cores.iter().filter(|info| info.ccd == ccd).collect();
            let roles = plan.roles(1, &cores).unwrap();
            // Every logical CPU of the tested CCD's cores, and none of another CCD's.
            assert_eq!(
                roles.len(),
                on_ccd.iter().map(|info| info.cpus.len()).sum::<usize>(),
                "{plan:?}"
            );
            assert!(
                roles
                    .iter()
                    .all(|role| on_ccd.iter().any(|info| info.core == role.core)),
                "{roles:?}"
            );
            checked += 1;
        }
        journal.run(&plan, Ending::Pass);
    }
    assert!(checked > 0, "the night ran no isolated R9 trial");
}
