//! Scripted-policy correctness evidence, not a registered policy score or a fixture trajectory.

use std::collections::BTreeSet;
use std::time::Duration;

use togi_machine::{
    CancellationToken, CoreId, Machine, PerCore, Reporter, Sample, Signal, TargetOffset,
};
use togi_simrun::day_night::successor::{
    self, AllowanceLedger, Classification, FinishReason, FirmwareRestoration, LearningEvent,
    Period, Phase, PolicyApplications, PolicyReport, PolicyUnit, ProfileStep, Provenance,
    Qualification, RestorationOutcome, RestorationResult, RunConfig, RunProfile, Split,
    StepIdentity, StepKind, TruthCause, UnitFinish, UnitId, UnitMode, UnitPhase,
};
use togi_simrun::day_night::{
    CalendarConfig, DayClassification, DayObservation, Night, Policy, RunError,
};

use super::score::fixture::Lab;
use super::score::{Rules, Scope, learning, measure};

const SCENARIO: &str = "successor-firmware-change";
const SEED: u64 = 17;
const RESTORATION: &str = "scripted-firmware-restoration";
const FIRMWARE_EPOCH: &str = "firmware-1";
const STOCK_HOURS: Duration = Duration::from_hours(50);

fn unit(index: u16) -> UnitId {
    UnitId::new(index).expect("physical-core unit")
}

fn day(machine: &Machine<'_>) -> Result<f64, RunError> {
    Ok(1.0 + machine.clock.monotonic()?.as_secs_f64() / 86_400.0)
}

fn readback(machine: &mut Machine<'_>) -> Result<PerCore<u8>, RunError> {
    machine
        .host
        .topology()?
        .iter()
        .map(|info| {
            Ok((
                info.core,
                u8::try_from(-machine.smu.offset(info.core)?.0)
                    .expect("simulated offsets are in the physical write range"),
            ))
        })
        .collect()
}

#[expect(
    clippy::disallowed_methods,
    reason = "the scripted policy writes only simulated registers"
)]
fn apply(machine: &mut Machine<'_>, profile: &PerCore<u8>) -> Result<(), RunError> {
    for (core, depth) in profile.iter() {
        machine
            .smu
            .set_offset(core, TargetOffset::clamped(-i64::from(depth)))?;
    }
    assert_eq!(
        readback(machine)?,
        *profile,
        "every write has a full actual readback"
    );
    Ok(())
}

struct Silent;

impl Reporter for Silent {
    fn progress(&mut self, _detail: &str) {}
    fn sample(&mut self, _sample: Sample) {}
    fn signal(&mut self, _core: CoreId, _signal: Signal, _pid: Option<u32>, _detail: &str) {}
}

struct Witness {
    id: String,
    started: f64,
    finished: f64,
    context: String,
    profile: PerCore<u8>,
    intended: Duration,
    ran: Duration,
    passed: bool,
}

struct FirmwareWitness {
    day: f64,
    context: String,
    readback: PerCore<u8>,
    allowance_before: AllowanceLedger,
    allowance_after: AllowanceLedger,
}

/// This policy deliberately bypasses the ordinary initial climb: one application deploys the whole
/// previously qualified vector. The audit must expose that permission violation rather than repair
/// it. Its firmware restoration, trial results, readbacks, active holds and crash response are real.
struct Scripted {
    report: PolicyReport,
    applications: PolicyApplications,
    stock: PerCore<u8>,
    previous: PerCore<u8>,
    witnesses: Vec<Witness>,
    old_controls: Vec<String>,
    new_controls: Vec<String>,
    old_qualification: Option<Qualification>,
    new_qualification: Option<Qualification>,
    firmware: Option<FirmwareWitness>,
    clean_stock: Duration,
}

impl Scripted {
    fn new() -> Self {
        let mut report = PolicyReport::unavailable();
        report.allowance = Some(AllowanceLedger {
            limit: 2,
            consumed: 0,
            provisional: 0,
            reclassified: 0,
            exhausted_day: None,
        });
        Self {
            report,
            applications: PolicyApplications::default(),
            stock: PerCore::default(),
            previous: PerCore::default(),
            witnesses: Vec::new(),
            old_controls: Vec::new(),
            new_controls: Vec::new(),
            old_qualification: None,
            new_qualification: None,
            firmware: None,
            clean_stock: Duration::ZERO,
        }
    }

    fn initialize(&mut self, machine: &mut Machine<'_>) -> Result<(), RunError> {
        if self.report.unit_membership.is_empty() {
            let topology = machine.host.topology()?;
            self.stock = topology.iter().map(|info| (info.core, 0)).collect();
            // The frozen firmware curve loses eight counts at day181. Alternating21/22 is in A
            // before the change, but outside A afterwards, without collapsing a CCD to one value.
            self.previous = topology
                .iter()
                .map(|info| (info.core, 21 + info.core.get() % 2))
                .collect();
            self.report.unit_membership = topology
                .iter()
                .enumerate()
                .map(|(index, info)| PolicyUnit {
                    unit: unit(u16::try_from(index).expect("core index")),
                    cores: vec![info.core],
                    kind: UnitPhase::Core,
                    parent: Some(unit(u16::from(info.ccd.get()))),
                })
                .collect();
        }
        self.report.readback = Some(readback(machine)?);
        Ok(())
    }

    fn cells(
        &mut self,
        night: &Night,
        machine: &mut Machine<'_>,
        prefix: &str,
        profile: PerCore<u8>,
    ) -> Result<(Vec<String>, bool), RunError> {
        apply(machine, &profile)?;
        let mut passed = Vec::new();
        let mut all_passed = true;
        for (index, template) in night.trials.iter().enumerate() {
            // Keep every supplied placement, workload and intended duration. A failed cell is
            // adverse evidence, not permission to shorten or silently omit the qualification.
            let mut spec = template.clone();
            spec.id = format!("{prefix}-{index:02}");
            spec.offsets = profile
                .iter()
                .map(|(core, depth)| (core, TargetOffset::clamped(-i64::from(depth))))
                .collect();
            let id = spec.id.clone();
            let intended = spec.duration;
            let started = day(machine)?;
            let actual_profile = readback(machine)?;
            let context = machine.host.bios_context()?.bios_version;
            let cancel = CancellationToken::new();
            let stopped = machine.trials.sweep(&cancel)?.stopped;
            let mut running = machine
                .trials
                .start(&cancel, spec, stopped)
                .map_err(|partial| partial.error)?;
            let result = running.wait(&cancel, &mut Silent);
            let _stopped = running.stop()?;
            machine.trials.ended(&id)?;
            let result = result.map_err(|partial| partial.error)?;
            let finished = day(machine)?;
            let clean = result.failure.is_none() && result.inconclusive.is_none();
            assert!(result.ran <= intended);
            if clean {
                assert_eq!(
                    result.ran, intended,
                    "a qualification witness is a complete cell"
                );
                passed.push(id.clone());
            } else {
                all_passed = false;
                self.report.events.push(LearningEvent {
                    id: format!("failure-{id}"),
                    day: finished,
                    classification: Classification::CpuSuspect,
                    provisional: false,
                    reclassified_from: None,
                    charged: false,
                    phase: if self.report.finishes.is_empty() {
                        Phase::Learning
                    } else {
                        Phase::PostFinish
                    },
                    unit_phase: UnitPhase::Core,
                    newest_step: None,
                });
            }
            self.witnesses.push(Witness {
                id,
                started,
                finished,
                context,
                profile: actual_profile,
                intended,
                ran: result.ran,
                passed: clean,
            });
        }
        apply(machine, &self.stock)?;
        self.report.readback = Some(readback(machine)?);
        Ok((passed, all_passed))
    }

    fn deploy_previous(&mut self, machine: &mut Machine<'_>) -> Result<(), RunError> {
        let before = readback(machine)?;
        apply(machine, &self.previous)?;
        let at = day(machine)?;
        let qualification = self
            .old_qualification
            .clone()
            .expect("actual old-context qualification");
        let id = "deliberately-adverse-initial-application".to_owned();
        self.applications.steps.push(ProfileStep {
            id: id.clone(),
            day: at,
            before,
            after: readback(machine)?,
            kind: StepKind::Climb {
                unit: unit(0),
                ceiling_depth: 22,
                clean_hold_hours: 1800.0,
                qualification: qualification.clone(),
            },
        });
        self.report.newest_step = Some(StepIdentity::Unit {
            unit: unit(0),
            proposal: id,
        });
        self.report.finishes = self
            .report
            .unit_membership
            .iter()
            .map(|member| UnitFinish {
                unit: member.unit,
                reason: FinishReason::Ceiling,
                day: at,
                witnesses: qualification.fresh_witnesses.clone(),
            })
            .collect();
        self.report.readback = Some(readback(machine)?);
        Ok(())
    }

    fn restore(&mut self, machine: &mut Machine<'_>) -> Result<(), RunError> {
        assert_eq!(self.clean_stock, STOCK_HOURS);
        let before = readback(machine)?;
        assert_eq!(before, self.stock);
        apply(machine, &self.previous)?;
        let at = day(machine)?;
        let qualification = self
            .new_qualification
            .clone()
            .expect("actual fresh qualification");
        self.applications.steps.push(ProfileStep {
            id: "whole-vector-restoration".into(),
            day: at,
            before,
            after: readback(machine)?,
            kind: StepKind::Restoration {
                restoration: RESTORATION.into(),
            },
        });
        self.applications.restorations[0].outcome = RestorationOutcome::Applied {
            day: at,
            qualification,
            clean_stock_hours: self.clean_stock.as_secs_f64() / 3600.0,
            result: RestorationResult::Retained {},
        };
        self.report.newest_step = Some(StepIdentity::Restoration {
            restoration: RESTORATION.into(),
            units: self
                .report
                .unit_membership
                .iter()
                .map(|member| member.unit)
                .collect(),
        });
        self.report.readback = Some(readback(machine)?);
        Ok(())
    }
}

impl Policy for Scripted {
    fn firmware_changed(&mut self, machine: &mut Machine<'_>) -> Result<(), RunError> {
        let allowance_before = self.report.allowance.clone().expect("reported allowance");
        let stock = readback(machine)?;
        assert_eq!(
            stock, self.stock,
            "the runner, not the script, already reset the registers"
        );
        let at = day(machine)?;
        self.applications.steps.push(ProfileStep {
            id: "observed-firmware-stock-reset".into(),
            day: at,
            before: self.previous,
            after: stock,
            kind: StepKind::StockReset {
                epoch: FIRMWARE_EPOCH.into(),
            },
        });
        self.applications.restorations.push(FirmwareRestoration {
            id: RESTORATION.into(),
            epoch: FIRMWARE_EPOCH.into(),
            transition_day: at,
            previous_profile: self.previous,
            membership: self.report.unit_membership.clone(),
            stock_reset_day: at,
            stock_readback: stock,
            outcome: RestorationOutcome::Blocked {
                reason: "stock hold and fresh trials still pending".into(),
            },
        });
        self.report.readback = Some(stock);
        self.report.newest_step = None;
        self.firmware = Some(FirmwareWitness {
            day: at,
            context: machine.host.bios_context()?.bios_version,
            readback: stock,
            allowance_before,
            allowance_after: self.report.allowance.clone().expect("preserved allowance"),
        });
        Ok(())
    }

    fn start_day(&mut self, public_day: u32, machine: &mut Machine<'_>) -> Result<(), RunError> {
        self.initialize(machine)?;
        if public_day == 186 {
            self.restore(machine)?;
        }
        Ok(())
    }

    fn observe_day(
        &mut self,
        _public_day: u32,
        evidence: &DayObservation,
        machine: &mut Machine<'_>,
    ) -> Result<DayClassification, RunError> {
        let profile = readback(machine)?;
        match evidence {
            DayObservation::Completed { active } => {
                if self.firmware.is_some()
                    && profile == self.stock
                    && self.clean_stock < STOCK_HOURS
                {
                    self.clean_stock += *active;
                }
                self.report.readback = Some(profile);
                Ok(DayClassification::NotCpuSuspect)
            }
            DayObservation::Ended { .. } => {
                assert_eq!(
                    profile, self.previous,
                    "the only daily interruption follows restoration"
                );
                let at = day(machine)?;
                let event = "observed-restoration-crash".to_owned();
                self.report.events.push(LearningEvent {
                    id: event.clone(),
                    day: at,
                    classification: Classification::CpuSuspect,
                    provisional: false,
                    reclassified_from: None,
                    charged: true,
                    phase: Phase::PostFinish,
                    unit_phase: UnitPhase::Core,
                    newest_step: self.report.newest_step.clone(),
                });
                self.report
                    .allowance
                    .as_mut()
                    .expect("preserved allowance")
                    .consumed += 1;
                apply(machine, &self.stock)?;
                let stock = readback(machine)?;
                self.report.stopped_units = self
                    .report
                    .unit_membership
                    .iter()
                    .map(|member| member.unit)
                    .collect();
                self.applications.steps.push(ProfileStep {
                    id: "crash-stock-step-back".into(),
                    day: day(machine)?,
                    before: profile,
                    after: stock,
                    kind: StepKind::Retreat {
                        event: Some(event.clone()),
                        units: self.report.stopped_units.clone(),
                    },
                });
                let RestorationOutcome::Applied { result, .. } =
                    &mut self.applications.restorations[0].outcome
                else {
                    panic!("only an applied restoration can crash");
                };
                *result = RestorationResult::Crashed {
                    event: event.clone(),
                    stock_step_back_day: day(machine)?,
                    stock_readback: stock,
                    stopped_units: self.report.stopped_units.clone(),
                    witnesses: vec![event],
                };
                self.report.readback = Some(stock);
                Ok(DayClassification::CpuSuspect)
            }
        }
    }

    fn night(&mut self, night: &Night, machine: &mut Machine<'_>) -> Result<(), RunError> {
        match night.day {
            178 => {
                let (controls, passed) = self.cells(night, machine, "old-control", self.stock)?;
                assert!(passed, "stock controls cannot have CO failures");
                self.old_controls = controls;
            }
            179 => {
                let (fresh_witnesses, safety_checks_satisfied) =
                    self.cells(night, machine, "old-vector", self.previous)?;
                self.old_qualification = Some(Qualification {
                    id: "old-context-full-vector".into(),
                    day: day(machine)?,
                    epoch: "initial".into(),
                    profile: self.previous,
                    stock_controls: self.old_controls.clone(),
                    fresh_witnesses,
                    reused_witnesses: Vec::new(),
                    safety_checks_satisfied,
                });
            }
            180 => self.deploy_previous(machine)?,
            181 => {
                let (controls, passed) = self.cells(night, machine, "new-control", self.stock)?;
                assert!(passed);
                self.new_controls = controls;
            }
            182 => {
                let (fresh_witnesses, safety_checks_satisfied) =
                    self.cells(night, machine, "new-vector", self.previous)?;
                self.new_qualification = Some(Qualification {
                    id: "fresh-context-full-vector".into(),
                    day: day(machine)?,
                    epoch: FIRMWARE_EPOCH.into(),
                    profile: self.previous,
                    stock_controls: self.new_controls.clone(),
                    fresh_witnesses,
                    reused_witnesses: Vec::new(),
                    safety_checks_satisfied,
                });
            }
            _ => {}
        }
        Ok(())
    }

    fn report(&self) -> PolicyReport {
        self.report.clone()
    }

    fn applications(&self) -> PolicyApplications {
        self.applications.clone()
    }
}

fn provenance(lab: &Lab) -> Provenance {
    // The existing fixture supplies metadata only: no fixture intervals, events or truth are used.
    let registration = &lab.registration;
    let policy = registration
        .policy("successor")
        .expect("fixture policy metadata");
    let super::registration::SourceIdentity::Pinned { revision, sha256 } = &policy.source else {
        panic!("the correctness fixture pins its synthetic source");
    };
    Provenance {
        registration_id: registration.id.clone(),
        registration_sha256: lab.sha.clone(),
        source_revision: revision.clone(),
        runner_revision: registration.sources.runner_revision.clone(),
        simulator_revision: registration.sources.simulator_revision.clone(),
        report_revision: registration.sources.report_revision.clone(),
        binary_sha256: registration.sources.binary_sha256.clone(),
        input_sha256: registration.sources.input_sha256.clone(),
        policy_identity: "scripted-restoration-correctness-only".into(),
        policy_source_sha256: sha256.clone(),
        parameters_sha256: policy.parameters_sha256.clone(),
        slope: 2.0,
        split: Split::Dev,
        seed: SEED,
        scenario: SCENARIO.into(),
        unit_mode: UnitMode::PerCore,
        topology: Vec::new(),
        membership: Vec::new(),
        run_profile: RunProfile::ReducedDev,
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end trajectory pins runtime, representation and public recomputation"
)]
fn firmware_restoration_records_real_full_vector_crash_and_recomputes_without_grace() {
    let lab = Lab::new(&[SCENARIO], 1);
    let rules = Rules::new(&lab.registration);
    let mut policy = Scripted::new();
    let record = successor::run(
        RunConfig {
            calendar: CalendarConfig {
                active_hours: 10.0,
                night_weekdays: (0..7).collect(),
                ..CalendarConfig::default()
            },
            provenance: provenance(&lab),
            epochs: Vec::new(),
            daily: Vec::new(),
            jobs: 1,
            tolerance: 0.0,
            resource_class: "synthetic-runtime-correctness".into(),
            compilation_seconds: 0.0,
            deadline: None,
        },
        &mut policy,
    )
    .expect("real successor execution");
    record
        .validate()
        .expect("the real trajectory has valid representation and accounting");
    assert!(record.outcome.is_complete(), "{:?}", record.outcome);
    assert_eq!(record.provenance.unit_mode, UnitMode::PerCore);
    assert_eq!(record.provenance.membership.len(), 16);
    assert_eq!(
        policy
            .previous
            .iter()
            .map(|(_, depth)| depth)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([21, 22])
    );

    let firmware = policy.firmware.as_ref().expect("firmware callback ran");
    assert_eq!(firmware.day, 181.0);
    assert_eq!(firmware.readback, policy.stock);
    assert_eq!(firmware.allowance_before, firmware.allowance_after);
    assert_eq!(firmware.allowance_after.consumed, 0);
    assert_eq!(record.policy.finishes.len(), 16);
    assert!(
        record
            .policy
            .finishes
            .iter()
            .all(|finish| finish.day < 181.0)
    );
    assert_eq!(record.restorations.len(), 1);
    let restoration = &record.restorations[0];
    assert_eq!(restoration.previous_profile, policy.previous);
    assert_eq!(restoration.stock_readback, policy.stock);
    assert_eq!(restoration.transition_day, 181.0);
    assert_eq!(restoration.stock_reset_day, 181.0);
    assert_eq!(restoration.membership, record.provenance.membership);
    assert_eq!(
        record
            .steps
            .iter()
            .filter(|step| matches!(step.kind, StepKind::StockReset { .. }))
            .count(),
        1
    );
    let restoration_steps: Vec<_> = record
        .steps
        .iter()
        .filter(|step| matches!(step.kind, StepKind::Restoration { .. }))
        .collect();
    assert_eq!(restoration_steps.len(), 1);
    let applied = restoration_steps[0];
    assert_eq!(applied.day, 186.0);
    assert_eq!(applied.before, policy.stock);
    assert_eq!(applied.after, policy.previous);

    let RestorationOutcome::Applied {
        day: applied_day,
        qualification,
        clean_stock_hours,
        result,
    } = &restoration.outcome
    else {
        panic!("the real restoration was applied");
    };
    assert_eq!(*applied_day, applied.day);
    assert_eq!(*clean_stock_hours, 50.0);
    assert_eq!(policy.clean_stock, STOCK_HOURS);
    assert_eq!(qualification.profile, policy.previous);
    assert_eq!(qualification.epoch, FIRMWARE_EPOCH);
    assert!(qualification.reused_witnesses.is_empty());
    let old = policy
        .old_qualification
        .as_ref()
        .expect("old-context evidence");
    assert!(old.day < 181.0);
    assert!(qualification.day >= 181.0 && qualification.day < applied.day);
    let scenario = togi_sim::day_night::successor::Scenario::builtin(SCENARIO, SEED)
        .expect("frozen truth scenario");
    let expected_cells = scenario.night_classes().len();
    assert_eq!(qualification.stock_controls.len(), expected_cells);
    assert_eq!(policy.witnesses.len(), 4 * expected_cells);
    let new_vector: Vec<_> = policy
        .witnesses
        .iter()
        .filter(|witness| witness.id.starts_with("new-vector-"))
        .collect();
    assert_eq!(new_vector.len(), expected_cells);
    for (witness, class) in new_vector.iter().zip(scenario.night_classes()) {
        assert_eq!(witness.profile, policy.previous);
        assert_eq!(
            witness.intended,
            class.duration(),
            "every original qualification cell runs unshortened"
        );
    }
    assert_eq!(
        qualification.safety_checks_satisfied,
        new_vector.iter().all(|witness| witness.passed)
    );
    assert_eq!(
        qualification.fresh_witnesses.len(),
        new_vector.iter().filter(|witness| witness.passed).count()
    );
    let old_ids: BTreeSet<_> = old
        .stock_controls
        .iter()
        .chain(&old.fresh_witnesses)
        .collect();
    for id in qualification
        .stock_controls
        .iter()
        .chain(&qualification.fresh_witnesses)
    {
        assert!(
            !old_ids.contains(id),
            "old-context evidence was not reused: {id}"
        );
        let witness = policy
            .witnesses
            .iter()
            .find(|witness| &witness.id == id)
            .expect("actual completed trial witness");
        assert!(witness.started >= 181.0 && witness.finished <= qualification.day);
        assert_eq!(witness.context, firmware.context);
        assert_eq!(witness.ran, witness.intended);
        assert!(witness.passed);
        assert_eq!(
            witness.profile,
            if qualification.stock_controls.contains(id) {
                policy.stock
            } else {
                policy.previous
            }
        );
    }
    assert!(
        policy
            .witnesses
            .iter()
            .filter(|witness| witness.started < 181.0)
            .all(|witness| witness.context != firmware.context)
    );

    let RestorationResult::Crashed {
        event,
        stock_step_back_day,
        stock_readback,
        stopped_units,
        witnesses,
    } = result
    else {
        panic!("the unsafe post-firmware vector must produce a truth-side crash");
    };
    let crash = record
        .events
        .iter()
        .find(|item| &item.policy_event.id == event)
        .expect("observed truth event");
    assert_eq!(crash.period, Period::Day);
    assert!(matches!(crash.truth, TruthCause::Co { .. }));
    assert_eq!(crash.profile, policy.previous);
    assert_eq!(crash.phase, Phase::PostFinish);
    assert_eq!(crash.policy_event.phase, Phase::PostFinish);
    assert_eq!(crash.restoration_learning.as_deref(), Some(RESTORATION));
    assert!(crash.policy_event.charged);
    assert_eq!(
        crash.policy_event.newest_step,
        Some(StepIdentity::Restoration {
            restoration: RESTORATION.into(),
            units: stopped_units.clone(),
        })
    );
    assert_eq!(*stock_step_back_day, crash.policy_event.day);
    assert_eq!(*stock_readback, policy.stock);
    assert_eq!(witnesses, &vec![event.clone()]);
    assert_eq!(stopped_units.len(), 16);
    assert_eq!(*stopped_units, record.policy.stopped_units);
    assert_eq!(record.policy.readback, Some(policy.stock));
    let allowance = record
        .policy
        .allowance
        .as_ref()
        .expect("policy supplied its ledger");
    assert_eq!(allowance.limit, firmware.allowance_before.limit);
    assert_eq!(allowance.consumed, firmware.allowance_before.consumed + 1);
    assert_eq!(allowance.remaining(), 1);
    assert_eq!(record.policy.events.len(), 1);
    assert_eq!(
        record
            .events
            .iter()
            .map(|item| &item.policy_event.id)
            .collect::<BTreeSet<_>>()
            .len(),
        record.events.len()
    );
    assert_eq!(
        record
            .events
            .iter()
            .filter(|item| item.period == Period::Day)
            .count(),
        1
    );
    assert_eq!(
        record
            .events
            .iter()
            .filter(|item| item.period == Period::Night)
            .count(),
        policy
            .witnesses
            .iter()
            .filter(|witness| !witness.passed)
            .count()
    );
    assert!(
        record
            .intervals
            .iter()
            .filter(|interval| interval.start_day < 181.0)
            .all(|interval| interval.admissibility.member),
        "the previous vector was safe in its original context"
    );
    let restored = record
        .intervals
        .iter()
        .find(|interval| interval.start_day == applied.day)
        .expect("application splits the trajectory");
    assert_eq!(restored.profile, policy.previous);
    assert_eq!(restored.period, Period::Day);
    assert_eq!(restored.phase, Phase::PostFinish);
    assert_eq!(restored.restoration_learning.as_deref(), Some(RESTORATION));
    assert!(!restored.admissibility.member);
    assert!(restored.verified_daily_active_hours > 0.0);

    // Only public record recomputation: no registered score, policy selection or acceptance grace.
    let audit = learning::audit(&record, &rules);
    assert_eq!(audit.restorations.len(), 1);
    let stock_hours = audit.restorations[0]
        .stock_hours_recomputed
        .expect("recomputed actual stock exposure");
    assert!(
        (stock_hours - 50.0).abs() <= rules.day_tolerance,
        "{stock_hours} active stock hours"
    );
    assert_eq!(
        audit.restorations[0].previous_depths,
        policy
            .previous
            .iter()
            .map(|(_, depth)| depth)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        audit.restorations[0].status,
        learning::RestorationStatus::Crashed {
            event: event.clone()
        }
    );
    assert!(
        audit
            .violations
            .iter()
            .any(|violation| violation.rule == "promotion-single-unit"),
        "the scripted initial application is not silently repaired"
    );
    assert_eq!(
        audit
            .violations
            .iter()
            .any(|violation| violation.rule == "restoration-safety"),
        !qualification.safety_checks_satisfied
    );
    assert!(
        audit
            .violations
            .iter()
            .all(|violation| !violation.rule.starts_with("restoration-")
                || violation.rule == "restoration-safety"),
        "{:?}",
        audit.violations
    );
    let metrics = measure::measure(&record, &rules, Scope::default())
        .expect("public scorer recomputes the complete record");
    assert_eq!(metrics.learning, audit);
    assert_eq!(
        metrics.first_finish_day,
        record.policy.finishes.first().map(|finish| finish.day)
    );
    assert!(!metrics.post_finish.all_in_a);
    assert_eq!(
        metrics.post_finish.first_violation,
        Some(applied.day),
        "no firmware or restoration grace interval"
    );
    assert!(metrics.post_finish.violating_hours > 0.0);
    assert_eq!(metrics.events.day_learning.total, 0);
    assert_eq!(metrics.events.day_post_finish.total, 1);
    assert_eq!(metrics.events.day_post_finish.co, 1);
    assert_eq!(
        metrics.events.night.total,
        u64::try_from(record.events.len() - 1).expect("night count")
    );
    assert_eq!(metrics.daily_co_crashes, 1);
    assert_eq!(metrics.risk.k, 1);
    assert_eq!(metrics.risk.learning_k, 0);
    assert_eq!(metrics.risk.tagged.k, 1);
    assert!(metrics.risk.tagged.hours > 0.0);
    assert!(metrics.risk.tagged.h > 0.0);
    assert!(
        metrics.risk.tagged.hours <= metrics.risk.t,
        "restoration exposure overlaps rather than adds"
    );
    let ledger = metrics
        .ledger
        .expect("scripted policy reports its machine allowance");
    assert_eq!(ledger.charged_post_finish, 1);
    assert_eq!(ledger.charged_learning, 0);
    assert_eq!(ledger.charged_restoration_tagged, 1);
    assert_eq!(ledger.consumed, 1);
    assert_eq!(metrics.policy_evidence.stopped_units, *stopped_units);
}
