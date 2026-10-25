use std::collections::HashSet;
use std::num::NonZeroU32;

use togi_machine::{Regime, WorkloadId};
use togi_tuner::Secs;
use togi_tuner::night::{
    AttemptLog, AttemptNight, AttemptScope, Budgets, Cell, Consumed, ControlCause, Inputs,
    NightTask, Purpose, Queue, Reason, Reproduction, ReproductionStage, Reuse, Roles, Stock,
    WallSecs, Work, Workloads, next_task, reproduction_cells, reproduction_stage,
};

use crate::fixture::{core, machine, profile, seq, set};

fn cell(secs: u32) -> Cell {
    Cell {
        workloads: Workloads::One {
            workload: WorkloadId::new(Regime::R1, 0).unwrap(),
        },
        loaded: set(&[0]),
        roles: Roles::SingleCore { core: core(0) },
        duration: Secs::new(NonZeroU32::new(secs).unwrap()),
        purpose: Purpose::Exact,
        sustained: false,
    }
}

fn work(reason: Reason, secs: u32) -> Work {
    Work {
        profile: profile(&[-5]),
        cells: vec![cell(secs); 3],
        reason,
    }
}

fn reproduction_work(secs: u32) -> Reproduction {
    Reproduction {
        event: seq(4),
        stage: ReproductionStage::Open(work(
            Reason::Reproduction {
                event: seq(4),
                attempt: AttemptScope::Exact,
            },
            secs,
        )),
    }
}

fn inputs() -> Inputs {
    Inputs {
        budgets: Budgets::default(),
        consumed: Consumed::default(),
        stock: Stock::Cleared { rerun: None },
        reproduction: None,
        qualification: None,
        mapping: None,
        requalification: None,
    }
}

fn qualification(secs: u32) -> Work {
    work(Reason::Qualification, secs)
}

fn mapping(secs: u32) -> Work {
    let target = togi_tuner::night::MappingTarget {
        ccd: crate::fixture::ccd(0),
        offset: crate::fixture::offset(0),
        kind: togi_tuner::night::MappingKind::Coarse,
    };
    work(Reason::Mapping { target }, secs)
}

fn requalification(secs: u32) -> Work {
    work(
        Reason::Requalification {
            due: togi_tuner::night::RequalificationDue::Elapsed,
        },
        secs,
    )
}

fn rerun(secs: u32) -> Stock {
    Stock::Cleared {
        rerun: Some(Work::co_zero_rerun(&cell(secs), &profile(&[-5]))),
    }
}

fn queue_of(task: &NightTask) -> Queue {
    match task {
        NightTask::Run { queue, .. } => *queue,
        NightTask::Unreproduced { .. } | NightTask::StockUnstable { .. } | NightTask::Idle => {
            panic!("expected a run")
        }
    }
}

fn secs(value: u64) -> WallSecs {
    WallSecs::new(value)
}

#[test]
fn queues_are_visited_in_priority_order() {
    let mut all = inputs();
    all.reproduction = Some(reproduction_work(100));
    all.qualification = Some(qualification(100));
    all.mapping = Some(mapping(100));
    all.stock = rerun(100);
    all.requalification = Some(requalification(100));
    assert_eq!(queue_of(&next_task(&all)), Queue::Reproduction);
    all.reproduction = None;
    assert_eq!(queue_of(&next_task(&all)), Queue::Qualification);
    all.qualification = None;
    assert_eq!(queue_of(&next_task(&all)), Queue::Mapping);
    all.mapping = None;
    assert_eq!(queue_of(&next_task(&all)), Queue::Controls);
    all.stock = Stock::Cleared { rerun: None };
    assert_eq!(queue_of(&next_task(&all)), Queue::Requalification);
    all.requalification = None;
    assert_eq!(next_task(&all), NightTask::Idle);
}

#[test]
fn a_task_carries_its_cell_the_full_profile_and_the_reason() {
    let mut input = inputs();
    input.qualification = Some(qualification(90));
    assert_eq!(
        next_task(&input),
        NightTask::Run {
            queue: Queue::Qualification,
            cell: cell(90),
            profile: profile(&[-5]),
            reason: Reason::Qualification,
        }
    );
}

#[test]
fn qualification_over_its_share_yields_only_to_a_queue_under_its_own() {
    let mut input = inputs();
    input.qualification = Some(qualification(100));
    input.mapping = Some(mapping(100));
    // E = 1,000 s: qualification at 640 s is under its 75%.
    input.consumed.qualification = secs(640);
    input.consumed.mapping = secs(360);
    assert_eq!(queue_of(&next_task(&input)), Queue::Qualification);
    // At its share, qualification yields while mapping is under its 10%.
    input.consumed.qualification = secs(950);
    input.consumed.mapping = secs(50);
    assert_eq!(queue_of(&next_task(&input)), Queue::Mapping);
    // With nothing else due, qualification takes the time, far past its 75%.
    input.mapping = None;
    assert_eq!(queue_of(&next_task(&input)), Queue::Qualification);
}

#[test]
fn reproduction_yields_past_a_quarter_of_the_elapsed_night() {
    let mut input = inputs();
    input.reproduction = Some(reproduction_work(100));
    input.qualification = Some(qualification(100));
    input.consumed.filler = secs(150);
    input.consumed.reproduction = secs(240);
    input.consumed.qualification = secs(610);
    assert_eq!(queue_of(&next_task(&input)), Queue::Reproduction);
    input.consumed.reproduction = secs(250);
    input.consumed.qualification = secs(600);
    assert_eq!(queue_of(&next_task(&input)), Queue::Qualification);
    // Alone, reproduction runs past its quarter.
    input.qualification = None;
    assert_eq!(queue_of(&next_task(&input)), Queue::Reproduction);
}

#[test]
fn a_due_queue_under_its_share_runs_ahead_of_earlier_queues_over_theirs() {
    let mut input = inputs();
    input.qualification = Some(qualification(100));
    input.mapping = Some(mapping(100));
    input.stock = rerun(100);
    input.requalification = Some(requalification(100));
    // Shares of E: qualification 75%, mapping 10%, controls 5%, re-qualification 10%.
    let at = |qualification, mapping, controls, requalification| Consumed {
        qualification: secs(qualification),
        mapping: secs(mapping),
        controls: secs(controls),
        requalification: secs(requalification),
        ..Consumed::default()
    };
    input.consumed = at(740, 100, 60, 100);
    assert_eq!(queue_of(&next_task(&input)), Queue::Qualification);
    // E = 1,040 s: qualification is at its 75%, mapping under its 10%.
    input.consumed = at(780, 100, 60, 100);
    assert_eq!(queue_of(&next_task(&input)), Queue::Mapping);
    input.consumed = at(780, 110, 50, 100);
    assert_eq!(queue_of(&next_task(&input)), Queue::Controls);
    input.consumed = at(1_000, 200, 70, 50);
    assert_eq!(queue_of(&next_task(&input)), Queue::Requalification);
}

#[test]
fn each_share_measures_only_its_own_queue_against_e() {
    let mut input = inputs();
    input.reproduction = Some(reproduction_work(100));
    input.qualification = Some(qualification(100));
    input.mapping = Some(mapping(100));
    // E = 1,050 s: reproduction is over its 25%; qualification's 640 s are under its 75% whatever
    // reproduction and mapping consumed, so no pool is left to share.
    input.consumed = Consumed {
        reproduction: secs(300),
        qualification: secs(640),
        mapping: secs(110),
        ..Consumed::default()
    };
    assert_eq!(queue_of(&next_task(&input)), Queue::Qualification);
    // E = 950 s: reproduction's 200 s are under its quarter, and it comes first.
    input.consumed.reproduction = secs(200);
    assert_eq!(queue_of(&next_task(&input)), Queue::Reproduction);
}

#[test]
fn with_every_queue_over_its_share_the_first_due_queue_runs() {
    let mut input = inputs();
    input.qualification = Some(qualification(100));
    input.mapping = Some(mapping(100));
    input.consumed = Consumed {
        qualification: secs(800),
        mapping: secs(150),
        filler: secs(50),
        ..Consumed::default()
    };
    assert_eq!(queue_of(&next_task(&input)), Queue::Qualification);
}

#[test]
fn the_first_task_of_a_night_runs_with_no_time_elapsed() {
    let mut input = inputs();
    input.mapping = Some(mapping(100));
    input.requalification = Some(requalification(100));
    assert_eq!(queue_of(&next_task(&input)), Queue::Mapping);
}

#[test]
fn a_cell_of_any_length_runs_first_in_its_queue() {
    let mut input = inputs();
    let mut long_first = qualification(9_000);
    long_first.cells.push(cell(90));
    input.qualification = Some(long_first);
    assert!(matches!(
        next_task(&input),
        NightTask::Run { cell: ran, .. } if ran == cell(9_000)
    ));
}

#[test]
fn first_night_controls_run_before_every_nonzero_queue_whatever_they_consumed() {
    let mut input = inputs();
    input.stock = Stock::Pending {
        cause: ControlCause::FirstNight,
        work: work(
            Reason::StockControls {
                cause: ControlCause::FirstNight,
            },
            100,
        ),
    };
    input.reproduction = Some(reproduction_work(100));
    input.qualification = Some(qualification(100));
    input.mapping = Some(mapping(100));
    input.requalification = Some(requalification(100));
    let task = next_task(&input);
    assert_eq!(queue_of(&task), Queue::Controls);
    assert!(matches!(
        task,
        NightTask::Run {
            reason: Reason::StockControls {
                cause: ControlCause::FirstNight
            },
            ..
        }
    ));
    // Controls run far beyond their own 5%.
    input.consumed.controls = secs(9_800);
    assert_eq!(queue_of(&next_task(&input)), Queue::Controls);
    // Once complete, the nonzero queues run in order.
    input.stock = Stock::Cleared { rerun: None };
    input.consumed.controls = secs(500);
    assert_eq!(queue_of(&next_task(&input)), Queue::Reproduction);
}

#[test]
fn changed_firmware_controls_are_a_prerequisite_too() {
    let mut input = inputs();
    input.stock = Stock::Pending {
        cause: ControlCause::FirmwareChange,
        work: work(
            Reason::StockControls {
                cause: ControlCause::FirmwareChange,
            },
            100,
        ),
    };
    input.mapping = Some(mapping(100));
    let NightTask::Run {
        reason, profile, ..
    } = next_task(&input)
    else {
        panic!("controls should run");
    };
    assert_eq!(
        reason,
        Reason::StockControls {
            cause: ControlCause::FirmwareChange
        }
    );
    assert_eq!(profile, crate::fixture::profile(&[-5]));
}

#[test]
fn a_conclusive_all_zero_failure_stops_the_night_as_a_dead_end() {
    let mut input = inputs();
    input.qualification = Some(qualification(100));
    input.reproduction = Some(reproduction_work(100));
    input.stock = Stock::Unstable { class: cell(120) };
    assert_eq!(
        next_task(&input),
        NightTask::StockUnstable { class: cell(120) }
    );
}

#[test]
fn a_nonzero_failure_naming_a_co_zero_core_reruns_the_same_cell_at_all_zero_once() {
    let failed = cell(120);
    let rerun = Work::co_zero_rerun(&failed, &profile(&[-10, -5]));
    assert_eq!(rerun.profile, profile(&[0, 0]));
    assert_eq!(rerun.cells, [failed]);
    assert_eq!(rerun.reason, Reason::CoZeroRerun);
    let mut input = inputs();
    input.stock = Stock::Cleared { rerun: Some(rerun) };
    let NightTask::Run {
        queue,
        cell,
        profile,
        reason,
    } = next_task(&input)
    else {
        panic!("the rerun should run");
    };
    assert_eq!(
        (queue, cell, reason),
        (Queue::Controls, failed, Reason::CoZeroRerun)
    );
    assert_eq!(profile, crate::fixture::profile(&[0, 0]));
}

#[test]
fn an_event_whose_one_night_has_passed_is_closed_unreproduced_before_other_work() {
    let mut input = inputs();
    input.qualification = Some(qualification(100));
    input.reproduction = Some(Reproduction {
        event: seq(9),
        stage: ReproductionStage::Lapsed,
    });
    assert_eq!(next_task(&input), NightTask::Unreproduced { event: seq(9) });
    input.reproduction = Some(Reproduction {
        event: seq(9),
        stage: ReproductionStage::Concluded,
    });
    assert_eq!(queue_of(&next_task(&input)), Queue::Qualification);
}

#[test]
fn reproduction_stops_an_attempt_at_its_first_failure_and_moves_to_the_next_profile() {
    let machine = machine(2, 4);
    let attempts = reproduction_cells(&machine.layout(), &profile(&[-10, -5]), 0).unwrap();
    let event = seq(3);
    let mut log = AttemptLog::default();
    let stage = |log: &AttemptLog, night| reproduction_stage(event, &attempts, log, night).stage;
    let ReproductionStage::Open(first) = stage(&log, AttemptNight::This) else {
        panic!("the exact profile is first");
    };
    assert_eq!(
        first.reason,
        Reason::Reproduction {
            event,
            attempt: AttemptScope::Exact
        }
    );
    assert_eq!(first.profile, profile(&[-10, -5]));
    assert_eq!(first.cells.len(), attempts[0].matrix.fresh_launches());
    // One clean cell is not outstanding any more.
    log.clean.insert((AttemptScope::Exact, first.cells[0]));
    let ReproductionStage::Open(resumed) = stage(&log, AttemptNight::This) else {
        panic!("still open");
    };
    assert_eq!(resumed.cells.len(), first.cells.len() - 1);
    // The exact profile fails: stop it, isolate CCD 0 alone.
    log.failed.insert(AttemptScope::Exact);
    let ReproductionStage::Open(alone) = stage(&log, AttemptNight::This) else {
        panic!("CCD 0 is next");
    };
    assert_eq!(
        alone.reason,
        Reason::Reproduction {
            event,
            attempt: AttemptScope::Ccd {
                ccd: crate::fixture::ccd(0)
            }
        }
    );
    assert_eq!(alone.profile, profile(&[-10, 0]));
    log.failed.insert(AttemptScope::Ccd {
        ccd: crate::fixture::ccd(0),
    });
    log.failed.insert(AttemptScope::Ccd {
        ccd: crate::fixture::ccd(1),
    });
    assert_eq!(
        stage(&log, AttemptNight::This),
        ReproductionStage::Concluded
    );
}

#[test]
fn outstanding_cells_on_an_earlier_night_lapse_the_event() {
    let machine = machine(1, 4);
    let attempts = reproduction_cells(&machine.layout(), &profile(&[-10]), 0).unwrap();
    let log = AttemptLog {
        clean: HashSet::new(),
        failed: HashSet::new(),
    };
    let stage = reproduction_stage(seq(3), &attempts, &log, AttemptNight::Earlier).stage;
    assert_eq!(stage, ReproductionStage::Lapsed);
    let done = AttemptLog {
        clean: attempts
            .iter()
            .flat_map(|a| a.matrix.fresh().map(|c| (a.scope, *c)).collect::<Vec<_>>())
            .collect(),
        failed: HashSet::new(),
    };
    assert_eq!(
        reproduction_stage(seq(3), &attempts, &done, AttemptNight::Earlier).stage,
        ReproductionStage::Concluded
    );
    let _ = Reuse::AllFresh;
}

#[test]
fn an_empty_pending_control_queue_does_not_block_nonzero_work() {
    let mut input = inputs();
    input.stock = Stock::Pending {
        cause: ControlCause::FirstNight,
        work: Work {
            profile: profile(&[0]),
            cells: Vec::new(),
            reason: Reason::StockControls {
                cause: ControlCause::FirstNight,
            },
        },
    };
    input.qualification = Some(qualification(100));
    assert_eq!(queue_of(&next_task(&input)), Queue::Qualification);
}
