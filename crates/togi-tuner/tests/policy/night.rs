//! The night model: a night opens at its first plan, runs until it closes, ends on its own only at a dead
//! end, an unattested stock, an excluded backend or an empty nonzero queue, and owes its closure to the
//! next query. Stock is observation-only: no night ever runs an all-zero trial.

use std::collections::{BTreeMap, BTreeSet};

use test_case::test_case;
use togi_journal::{
    Activity, CommandReset, NightId, Payload, QualificationState, Queue, ResetTarget, Seq,
    SessionContext, Shutdown, ShutdownReason, TaskId, Tested, UnavailableBackend,
};
use togi_machine::{Backend, BootId, CoreId, Regime};
use togi_tuner::day::{Invocation, Pause};
use togi_tuner::night::{PackageLoad, Roles};
use togi_tuner::policy::{DayQuery, EndReason, Params, TrialPlan};

use crate::fixture::{Ending, Journal, at, context, cores, cores_per_ccd, firmware};
use crate::flow::promoted;

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
fn a_dead_end_shutdown_closes_the_night_and_a_restart_opens_a_new_one() {
    let mut journal = Journal::ready();
    let night = run_trials(&mut journal, 3);
    let stop = journal.append(
        Shutdown {
            reason: ShutdownReason::DeadEnd,
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

#[test]
fn the_filler_repeats_exact_current_cells_and_never_probes_below_the_cap() {
    let (mut journal, _) = promoted();
    journal.reboot("tuning2");
    let mut plan = journal.until_filler(|_| Ending::Pass).unwrap();
    let current = journal.state.project().applied.unwrap();
    assert!(!current.is_zero());
    for step in 0..6_i64 {
        assert!(!plan.retry);
        assert_eq!(plan.task.queue, Queue::Filler);
        assert_eq!(plan.activity, Activity::Qualification);
        assert_eq!(plan.tested, Tested::Exact {});
        assert_eq!(plan.profile, current);
        journal.run(&plan, Ending::Pass);
        plan = journal.until_trial(at(10_000 + step));
    }
    for ccd in 0..2_u8 {
        let edge = journal
            .events
            .iter()
            .rev()
            .find_map(|event| {
                let Payload::UnitEdge(edge) = &event.payload else {
                    return None;
                };
                (edge.unit == togi_journal::UnitId::Ccd(crate::fixture::ccd(ccd))).then_some(edge)
            })
            .unwrap();
        assert!(
            matches!(&edge.new, togi_journal::Edge::Censored { limit, witnesses }
            if limit.get() == -50 && !witnesses.is_empty())
        );
        assert_eq!(edge.ceiling.map(togi_machine::TargetOffset::get), Some(-45));
        assert_eq!(edge.margin, 5);
    }
    let sweep: Vec<_> = journal
        .events
        .iter()
        .filter_map(|event| {
            let Payload::TrialIntent(intent) = &event.payload else {
                return None;
            };
            if intent.task.is_none_or(|task| task.queue != Queue::Mapping) {
                return None;
            }
            let Some(Tested::Isolated { offset, .. }) = intent.tested else {
                return None;
            };
            (offset.get() % 5 != 0).then_some(offset.get())
        })
        .collect();
    assert!(!sweep.is_empty());
    assert!(sweep.iter().all(|&offset| offset <= -45), "{sweep:?}");
    assert!(
        !journal
            .events
            .iter()
            .any(|event| matches!(event.payload, Payload::CcdEdge(_)))
    );
}

#[test]
fn unknown_edges_are_committed_without_a_ceiling_before_the_descent_completes() {
    let mut journal = Journal::ready();
    journal.until_trial(at(100));
    let edges: Vec<_> = journal
        .events
        .iter()
        .filter_map(|event| {
            if let Payload::UnitEdge(edge) = &event.payload {
                Some(edge)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(edges.len(), 2);
    for edge in edges {
        assert_eq!(edge.new, togi_journal::Edge::Unknown {});
        assert_eq!(edge.ceiling, None);
        assert_eq!(edge.margin, 5);
        assert_eq!(edge.epoch, togi_tuner::EVIDENCE_EPOCH);
    }
}

#[test]
fn a_backend_change_invalidates_edges_without_repeating_unknown_commitments() {
    let mut journal = Journal::ready();
    let _ = journal.until_filler(|_| Ending::Pass);
    let from = journal.events.len();
    journal.rebuild(crate::fixture::rebuilt());
    // The rebuilt backends restart the nonzero mapping at its first step; no stock control precedes it.
    let plan = journal.until_trial(journal.now());
    assert_eq!(plan.activity, Activity::Mapping);
    assert!(!plan.profile.is_zero(), "{plan:?}");
    let edges: Vec<_> = journal.events[from..]
        .iter()
        .filter_map(|event| {
            if let Payload::UnitEdge(edge) = &event.payload {
                Some(edge)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(edges.len(), 2);
    for edge in edges {
        assert_eq!(edge.new, togi_journal::Edge::Unknown {});
        assert_eq!(edge.ceiling, None);
    }
}

#[test_case(-30, -25; "observed_deep_edge")]
#[test_case(-4, 0; "observed_near_stock_edge")]
fn isolated_failures_commit_the_observed_unit_edge_and_its_ceiling(u: i64, c: i64) {
    let mut journal = Journal::ready();
    let _ = journal.until_filler(|plan| match plan.tested {
        Tested::Isolated { offset, .. }
            if plan.task.queue == Queue::Mapping && offset.get() <= u =>
        {
            Ending::Fail(None)
        }
        Tested::Isolated { .. } | Tested::Exact {} => Ending::Pass,
    });
    for ccd in 0..2_u8 {
        let event = journal.events.iter().rev().find(|event| matches!(&event.payload,
            Payload::UnitEdge(edge) if edge.unit == togi_journal::UnitId::Ccd(crate::fixture::ccd(ccd))
        )).unwrap();
        let Payload::UnitEdge(edge) = &event.payload else {
            unreachable!()
        };
        let togi_journal::Edge::Observed { offset, witness } = edge.new else {
            panic!("expected observed U: {edge:?}");
        };
        assert_eq!(offset.get(), u);
        assert_eq!(edge.ceiling.map(togi_machine::TargetOffset::get), Some(c));
        assert_eq!(edge.margin, 5);
        assert!(event.cause.contains(&witness));
    }
}

#[test]
fn each_filler_trial_advances_the_durable_rotation_across_nights() {
    let (mut journal, _) = promoted();
    journal.reboot("tuning2");
    let mut plan = journal.until_filler(|_| Ending::Pass).unwrap();
    let first_night = plan.night;
    let first_ordinal = plan.task.ordinal;
    let mut cells = Vec::new();
    for step in 0..6_i64 {
        assert_eq!(plan.task.queue, Queue::Filler);
        assert_eq!(plan.night, first_night);
        assert!(!plan.profile.is_zero());
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
    let next = journal.until_filler(|_| Ending::Pass).unwrap();
    assert_ne!(next.night, first_night);
    assert_eq!(next.task.ordinal, first_ordinal + 1);
    assert_eq!(journal.state.project().night.unwrap().filler, 6);
}

#[test]
fn observed_edges_at_the_stock_ceiling_leave_no_nonzero_work_and_name_it() {
    let mut journal = Journal::ready();
    // Isolated mapping failures bracket U at the stock ceiling: no nonzero step is admissible, and stock
    // is observation-only, so the night ends for want of nonzero work rather than running a filler.
    let mapping_fails = |plan: &TrialPlan| match plan.tested {
        Tested::Isolated { offset, .. }
            if plan.task.queue == Queue::Mapping && offset.get() <= -5 =>
        {
            Ending::Fail(None)
        }
        Tested::Isolated { .. } | Tested::Exact {} => Ending::Pass,
    };
    assert_eq!(
        journal.until_night_end(mapping_fails),
        EndReason::NoNonzeroWork
    );
    for ccd in 0..2_u8 {
        assert!(journal.events.iter().any(|event| matches!(&event.payload,
            Payload::UnitEdge(change) if change.unit == togi_journal::UnitId::Ccd(crate::fixture::ccd(ccd))
                && matches!(change.new, togi_journal::Edge::Observed { .. })
                && change.ceiling == Some(togi_machine::TargetOffset::ZERO)
                && change.margin == 5)));
        assert!(journal.events.iter().any(|event| matches!(&event.payload,
            Payload::CcdCap(cap) if cap.ccd.get() == ccd
                && matches!(cap.edge, togi_journal::Edge::Observed { .. }))));
    }
    // The end is not an excluded backend's, and asked again it stands without another commitment.
    let before = journal.events.len();
    assert_eq!(
        journal.until_end(at(20_000), &BTreeMap::new()).unwrap_err(),
        EndReason::NoNonzeroWork
    );
    assert_eq!(journal.events.len(), before);
}

#[test]
fn switching_away_from_a_qualification_task_commits_its_progress_first() {
    let mut journal = Journal::ready();
    let mut previous: Option<TaskId> = None;
    let mut switches = 0;
    for step in 0..3_500_i64 {
        let from = journal.events.len();
        let Ok(plan) = journal.until_end(at(100 + step), &BTreeMap::new()) else {
            break;
        };
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
    let mut journal = Journal::ready();
    journal.until_trial(at(100));
    let plans = plans_since(&journal, 0);
    assert_eq!(plans.first().map(|(night, _)| *night), Some(None));
    assert_eq!(journal.state.project().night.unwrap().closed_by, None);
}

#[test]
fn an_isolated_all_thread_trial_loads_only_the_tested_ccd() {
    let mut journal = Journal::ready();
    let cores = cores();
    let mut checked = 0;
    for step in 0..3_500_i64 {
        let Ok(plan) = journal.until_end(at(100 + step), &BTreeMap::new()) else {
            break;
        };
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

/// Drives the night with passing trials until it asks for a mapping trial of CCD0 at a nonzero offset.
fn until_mapping_of_ccd0(journal: &mut Journal) -> TrialPlan {
    for step in 0..3_500_i64 {
        let plan = journal.until_trial(at(100 + step));
        if plan.task.queue == Queue::Mapping
            && matches!(plan.tested, Tested::Isolated { ccd, offset }
                if ccd.get() == 0 && offset.get() < 0)
        {
            return plan;
        }
        assert_ne!(
            plan.task.queue,
            Queue::Filler,
            "the night never mapped CCD0"
        );
        journal.run(&plan, Ending::Pass);
    }
    panic!("the night never mapped CCD0");
}

/// Drives the night to its end, each trial ending as `ending` says, and returns why it ended. Every trial
/// must carry a nonzero profile, none may be a control, and the night never reaches a filler.
fn drive_without_zero(journal: &mut Journal, ending: impl Fn(&TrialPlan) -> Ending) -> EndReason {
    for step in 0..6_000_i64 {
        let plan = match journal.until_end(at(100 + step), &BTreeMap::new()) {
            Ok(plan) => plan,
            Err(reason) => return reason,
        };
        assert!(!plan.profile.is_zero(), "an all-zero trial: {plan:?}");
        assert_ne!(plan.activity, Activity::Control, "{plan:?}");
        assert_ne!(plan.task.queue, Queue::Control, "{plan:?}");
        assert_ne!(plan.task.queue, Queue::Filler, "{plan:?}");
        journal.run(&plan, ending(&plan));
    }
    panic!("the night never ended");
}

#[test]
fn the_first_trial_of_a_fresh_night_maps_nonzero_minus_five() {
    let mut journal = Journal::ready();
    let plan = journal.until_trial(at(100));
    assert_eq!(plan.task.queue, Queue::Mapping);
    assert_eq!(plan.activity, Activity::Mapping);
    assert!(!plan.profile.is_zero(), "{plan:?}");
    let Tested::Isolated { offset, .. } = plan.tested else {
        panic!("mapping must isolate a CCD: {plan:?}");
    };
    assert_eq!(offset.get(), -5);
}

#[test_case(0; "every trial passes")]
#[test_case(1; "mapping fails at minus five")]
#[test_case(2; "a located failure on the other CCD's core")]
fn a_night_never_runs_an_all_zero_trial_and_ends_for_want_of_nonzero_work(mode: u8) {
    let mut journal = Journal::ready();
    let ending = move |plan: &TrialPlan| match plan.tested {
        Tested::Isolated { ccd, offset }
            if mode > 0 && plan.task.queue == Queue::Mapping && offset.get() <= -5 =>
        {
            Ending::Fail((mode == 2).then_some(if ccd.get() == 0 { 2 } else { 0 }))
        }
        Tested::Isolated { .. } | Tested::Exact {} => Ending::Pass,
    };
    assert_eq!(
        drive_without_zero(&mut journal, ending),
        EndReason::NoNonzeroWork
    );
    assert!(journal.events.iter().all(|event| {
        if let Payload::TrialIntent(intent) = &event.payload {
            intent.activity != Some(Activity::Control)
        } else {
            true
        }
    }));
}

/// Whether the projection's CCD `ccd` edge has been observed by a mapping failure.
fn edge_observed(journal: &Journal, ccd: usize) -> bool {
    let projection = serde_json::to_value(journal.state.project()).unwrap();
    projection["climb"]["units"][ccd]["edge"]["kind"] == "observed"
}

#[test]
fn a_located_failure_on_a_co_zero_core_charges_the_tested_ccd_and_runs_no_zero_trial() {
    let mut journal = Journal::ready();
    let failed = until_mapping_of_ccd0(&mut journal);
    // Core 2 is on CCD1, which an isolated CCD0 trial holds at CO 0.
    journal.run(&failed, Ending::Fail(Some(2)));
    assert_eq!(
        drive_without_zero(&mut journal, |_| Ending::Pass),
        EndReason::NoNonzeroWork
    );
    // The tested CCD carries the failure; the CCD held at stock does not.
    assert!(edge_observed(&journal, 0));
    assert!(!edge_observed(&journal, 1));
}

#[test]
fn a_night_without_the_operators_attestation_runs_nothing_and_ends_unattested() {
    let mut journal = Journal::ready_unattested();
    assert_eq!(
        journal.until_end(at(100), &BTreeMap::new()).unwrap_err(),
        EndReason::StockUnattested
    );
    let before = journal.events.len();
    assert_eq!(
        journal.until_end(at(101), &BTreeMap::new()).unwrap_err(),
        EndReason::StockUnattested
    );
    assert_eq!(journal.events.len(), before);
    assert!(
        !journal
            .events
            .iter()
            .any(|event| matches!(event.payload, Payload::TrialIntent(_)))
    );
    // The operator's attestation under the current context lets the night run, nonzero.
    journal.attest(&context());
    let plan = journal.until_trial(at(102));
    assert_eq!(plan.activity, Activity::Mapping);
    assert!(!plan.profile.is_zero(), "{plan:?}");
}

#[test]
fn an_attestation_folds_independently_of_the_current_context() {
    let mut journal = Journal::ready_unattested();
    // An attestation of another context does not attest the current one.
    journal.attest(&firmware("2"));
    assert_eq!(
        journal.until_end(at(100), &BTreeMap::new()).unwrap_err(),
        EndReason::StockUnattested
    );
    // It stands for that context when it later becomes current.
    journal.append(firmware("2").into(), &[]);
    let plan = journal.until_trial(at(101));
    assert!(!plan.profile.is_zero(), "{plan:?}");
}

fn changed_context(field: &str, value: &str) -> SessionContext {
    let mut changed = context();
    match field {
        "bios_version" => changed.bios_version = value.into(),
        "microcode" => changed.microcode = value.into(),
        other => panic!("unknown context field {other}"),
    }
    changed
}

#[test_case("bios_version", "2"; "BIOS version")]
#[test_case("bios_version", "1.2.0.7 AGESA ComboAM5PI 1.2.0.2"; "AGESA")]
#[test_case("microcode", "mc2"; "microcode")]
fn a_changed_firmware_context_invalidates_the_attestation(field: &str, value: &str) {
    let mut journal = Journal::ready();
    run_trials(&mut journal, 3);
    let changed = changed_context(field, value);
    journal.append(changed.clone().into(), &[]);
    let from = journal.events.len();
    assert_eq!(
        journal.until_end(at(5_000), &BTreeMap::new()).unwrap_err(),
        EndReason::StockUnattested
    );
    let ended = journal.events.len();
    assert_eq!(
        journal.until_end(at(5_001), &BTreeMap::new()).unwrap_err(),
        EndReason::StockUnattested
    );
    assert_eq!(journal.events.len(), ended);
    assert!(
        !journal.events[from..]
            .iter()
            .any(|event| matches!(event.payload, Payload::TrialIntent(_)))
    );
    journal.attest(&changed);
    let plan = journal.until_trial(at(5_002));
    assert!(!plan.profile.is_zero(), "{plan:?}");
}

#[test]
fn an_attestation_is_durable_when_its_firmware_context_returns() {
    let mut journal = Journal::ready();
    run_trials(&mut journal, 3);
    journal.append(firmware("2").into(), &[]);
    assert_eq!(
        journal.until_end(at(5_000), &BTreeMap::new()).unwrap_err(),
        EndReason::StockUnattested
    );
    journal.append(context().into(), &[]);
    let plan = journal.until_trial(at(5_001));
    assert!(!plan.profile.is_zero(), "{plan:?}");
}

#[test]
fn a_backend_rebuild_does_not_invalidate_the_attestation() {
    let mut journal = Journal::ready();
    run_trials(&mut journal, 3);
    journal.rebuild(crate::fixture::rebuilt());
    let plan = journal.until_trial(at(5_000));
    assert!(!plan.profile.is_zero(), "{plan:?}");
}

/// Whether `plan` is a mapping trial of CCD0 at a nonzero offset.
fn is_isolated_ccd0_mapping(plan: &TrialPlan) -> bool {
    plan.task.queue == Queue::Mapping
        && matches!(plan.tested, Tested::Isolated { ccd, offset }
            if ccd.get() == 0 && offset.get() < 0)
}

/// A night on two CCDs of four cores, so every CCD derives honest R7 partials.
fn eight_core_night() -> Journal {
    Journal::ready_on(Params::SPEC, cores_per_ccd(4))
}

#[test_case(false; "incremental queries")]
#[test_case(true; "cold journal fold")]
fn mapping_steps_keep_their_declared_partials_and_alternate_ccds(cold: bool) {
    let mut journal = eight_core_night();
    let mut tasks = Vec::new();
    let mut exposed = Vec::new();
    let mut changed = false;
    for _ in 0..2_000 {
        let plan = journal.until_trial(journal.now());
        if plan.task.queue == Queue::Mapping {
            let Tested::Isolated { ccd, offset } = plan.tested else {
                panic!("mapping must isolate a CCD");
            };
            if tasks.last().map(|(task, _, _)| *task) != Some(plan.task) {
                tasks.push((plan.task, ccd.get(), offset.get()));
            }
            assert!(
                !exposed.contains(&(plan.profile.clone(), plan.cell)),
                "mapping repeated an adequate cell at the same profile: {plan:?}"
            );
            exposed.push((plan.profile.clone(), plan.cell));
            if tasks.len() == 5 {
                assert!(changed);
                assert_eq!(
                    tasks
                        .iter()
                        .map(|(_, ccd, offset)| (*ccd, *offset))
                        .collect::<Vec<_>>(),
                    vec![(0, -5), (1, -5), (0, -10), (1, -10), (0, -15)]
                );
                return;
            }
        }
        if !changed
            && is_isolated_ccd0_mapping(&plan)
            && matches!(plan.cell.roles, Roles::Package(PackageLoad::FullCcd { .. }))
        {
            let requests = (0..4_i64)
                .zip([1.3, 1.2, 1.1, 1.0])
                .map(|(id, volts)| (CoreId::new(id).unwrap(), volts))
                .collect();
            journal.run_requesting(&plan, &requests);
            changed = true;
        } else {
            journal.run(&plan, Ending::Pass);
        }
        if cold {
            let mut state = togi_tuner::policy::State::new(Params::SPEC);
            for event in &journal.events {
                state.fold(event).unwrap();
            }
            journal.state = state;
        }
        if changed {
            let mapping = journal.state.project().night.unwrap().mapping;
            assert!(
                mapping[0].cursor.is_none_or(|offset| offset.get() <= -5),
                "later full-CCD telemetry rewound a finished step: {mapping:?}"
            );
            assert_eq!(tasks[0].0.ordinal, 0, "the original mapping task continues");
        }
    }
    panic!("mapping did not alternate through five steps");
}

#[test]
fn mapping_reuses_compatible_adequate_cells_from_another_task() {
    let mut journal = eight_core_night();
    let original = until_mapping_of_ccd0(&mut journal);
    let mut filler = original.clone();
    filler.task = TaskId {
        queue: Queue::Filler,
        ordinal: 0,
    };
    journal.run(&filler, Ending::Pass);
    let continued = journal.until_trial(journal.now());
    assert_eq!(continued.task, original.task);
    assert_eq!(continued.profile, original.profile);
    assert_ne!(
        continued.cell, original.cell,
        "the filler already passed this cell"
    );
}

#[test]
fn an_abandoned_rewound_task_does_not_hide_the_original_mapping_step() {
    let mut journal = eight_core_night();
    let original = until_mapping_of_ccd0(&mut journal);
    journal.run(&original, Ending::Pass);
    // Replay a defective build's later task at CO 0. It must not replace the still-open -5 task.
    let mut rewound = original.clone();
    rewound.task.ordinal += 1;
    let Tested::Isolated { ccd, .. } = rewound.tested else {
        panic!("mapping must isolate a CCD");
    };
    rewound.tested = Tested::Isolated {
        ccd,
        offset: togi_machine::TargetOffset::new(0).unwrap(),
    };
    rewound.profile = togi_tuner::UnitProfile::of_ccd(&crate::fixture::zero());
    journal.run(&rewound, Ending::Pass);
    let continued = journal.until_trial(journal.now());
    assert_eq!(continued.task, original.task);
    assert_eq!(continued.profile, original.profile);
    assert_ne!(continued.cell, original.cell);
}

/// Drives a night of passing trials until its isolated CCD0 mapping step asks for a cell of `regime`, and
/// fails that trial on core 4, a CO-0 core of CCD1.
fn fail_first_isolated_ccd0_cell_of(journal: &mut Journal, regime: Regime) -> TrialPlan {
    for step in 0..5_000_i64 {
        let plan = journal.until_trial(at(100 + step));
        assert_ne!(
            plan.task.queue,
            Queue::Filler,
            "the night never mapped CCD0 under {regime:?}"
        );
        if is_isolated_ccd0_mapping(&plan) && plan.cell.regime() == regime {
            journal.run(&plan, Ending::Fail(Some(4)));
            return plan;
        }
        journal.run(&plan, Ending::Pass);
    }
    panic!("the night never mapped CCD0 under {regime:?}");
}

#[test_case(Regime::R1; "single core")]
#[test_case(Regime::R2; "single core of another regime")]
#[test_case(Regime::R7; "full CCD package")]
#[test_case(Regime::R8; "light neighbours")]
#[test_case(Regime::R9; "all threads with a serial CPU")]
fn a_co_zero_core_failure_of_any_regime_charges_the_tested_ccd_and_reruns_nothing(regime: Regime) {
    let mut journal = eight_core_night();
    fail_first_isolated_ccd0_cell_of(&mut journal, regime);
    assert_eq!(
        drive_without_zero(&mut journal, |_| Ending::Pass),
        EndReason::NoNonzeroWork
    );
    assert!(edge_observed(&journal, 0));
    assert!(!edge_observed(&journal, 1));
}

#[test]
fn a_trial_charges_its_queue_from_when_its_task_began() {
    let mut journal = Journal::ready();
    let plan = journal.until_trial(at(100));
    let night = plan.night;
    // Profile preparation and a backend retry wait took half an hour before the intent.
    journal.run_after_wait(&plan, 1_800);
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
    journal.until_trial(at(5_000));
    let closing = journal
        .events
        .iter()
        .filter_map(|event| {
            if let Payload::NightPlan(committed) = &event.payload
                && committed.night == Some(night)
            {
                Some(committed)
            } else {
                None
            }
        })
        .next_back()
        .unwrap();
    let consumed: u64 = closing.budgets.iter().map(|budget| budget.consumed_s).sum();
    assert!(consumed >= 1_800, "{closing:?}");
}
