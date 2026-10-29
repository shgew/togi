//! Synthetic registrations and run records for the scorer tests.
//!
//! A [`Spec`] describes one cell's trajectory in a few knobs: every unit climbs in one promotion to its
//! depth (at most five counts, ceiling twice the depth), the profile then holds for the horizon at a
//! constant hazard ratio, optional events happen, and a firmware scenario may restore, be blocked or
//! crash. [`Spec::build`] derives the intervals, accounts, ledger and exposure so that the record passes
//! [`Record::validate`], and tests then mutate public fields to break one rule at a time. The synthetic
//! trajectories are arithmetic stand-ins, not generated truth.

use togi_machine::{CcdId, CoreId, CoreInfo, CpuId, PerCore};
use togi_simrun::day_night::successor::{
    Admissibility, AllowanceLedger, Classification, DepthReference, Epoch, Exposure, FinishReason,
    FirmwareRestoration, InvocationTiming, LearningEvent, MachineEvent, Outcome, PartialCounts,
    Period, Phase, PolicyReport, PolicyUnit, ProfileInterval, ProfileStep, Provenance,
    Qualification, Record, RestorationOutcome, RestorationResult, Retreats, RiskAccounts,
    RiskSource, RunProfile, Runtime, SourceAccount, SourceRisk, Split, StepIdentity, StepKind,
    TruthCause, UnitFinish, UnitId, UnitMode, UnitPhase, UnitSummary,
};

use super::super::registration::{self, Cell, Registration, SeedSelection, SourceIdentity};
use super::ScoreSelection;

pub(crate) const HORIZON_END: f64 = 731.0;
pub(crate) const ACTIVE_HOURS: f64 = 8.0;

pub(crate) fn hex(digit: char) -> String {
    digit.to_string().repeat(64)
}

pub(crate) fn uid(index: usize) -> UnitId {
    UnitId::new(u16::try_from(index).expect("unit index")).expect("unit id")
}

/// A registration restricted to a few scenarios and seeds, with pinned binary and input hashes.
pub(crate) struct Lab {
    pub(crate) registration: Registration,
    pub(crate) sha: String,
}

impl Lab {
    pub(crate) fn new(scenarios: &[&str], seeds: u32) -> Self {
        let mut registration = registration::tests::registration();
        registration
            .scenarios
            .retain(|scenario| scenarios.contains(&scenario.name.as_str()));
        assert_eq!(
            registration.scenarios.len(),
            scenarios.len(),
            "fixture scenario names"
        );
        registration.seeds.dev.count = seeds;
        registration.seeds.holdout.count = seeds;
        for spec in &mut registration.run_profiles {
            if let SeedSelection::First { count } = &mut spec.seeds {
                *count = seeds;
            }
        }
        registration.sources.binary_sha256 = Some(hex('b'));
        registration.sources.input_sha256 = Some(hex('c'));
        Self {
            registration,
            sha: hex('a'),
        }
    }

    /// The full registration fixture: every scenario, the registered seed counts.
    pub(crate) fn full() -> Self {
        let mut lab = Self::new(&["successor-edge-deep"], 0);
        lab.registration = registration::tests::registration();
        lab.registration.sources.binary_sha256 = Some(hex('b'));
        lab.registration.sources.input_sha256 = Some(hex('c'));
        lab
    }

    pub(crate) fn selection(&self, split: Split, run_profile: RunProfile) -> ScoreSelection {
        ScoreSelection {
            split,
            run_profile,
            policy: self
                .registration
                .sources
                .policies
                .iter()
                .find(|policy| policy.role == super::registration::PolicyRole::Candidate)
                .expect("synthetic registration candidate")
                .name
                .clone(),
            slopes: None,
        }
    }

    pub(crate) fn cells(&self, split: Split, run_profile: RunProfile) -> Vec<Cell> {
        self.registration.cells(split, run_profile).expect("cells")
    }

    /// One record per expected cell, each built from the spec the closure returns.
    pub(crate) fn records(
        &self,
        split: Split,
        run_profile: RunProfile,
        shape: impl Fn(Spec) -> Spec,
    ) -> Vec<Record> {
        self.cells(split, run_profile)
            .iter()
            .map(|cell| {
                let mut spec = Spec::new(&cell.scenario, cell.seed, cell.slope);
                spec.mode = cell.unit_mode;
                spec.split = split;
                spec.run_profile = run_profile;
                shape(spec).build(self)
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Restore {
    Retained,
    Blocked,
    Crashed,
}

/// One synthetic daily or night interruption.
#[derive(Clone, Debug)]
pub(crate) struct Ev {
    pub(crate) id: String,
    pub(crate) day: f64,
    pub(crate) period: Period,
    pub(crate) truth: TruthCause,
    pub(crate) classification: Classification,
    pub(crate) charged: bool,
    pub(crate) provisional: bool,
    pub(crate) reclassified_from: Option<Classification>,
    pub(crate) newest_step: Option<StepIdentity>,
}

impl Ev {
    /// A daily true CO crash on one unit, classified CPU-suspect and charged.
    pub(crate) fn co(id: &str, day: f64, unit: usize) -> Self {
        Self {
            id: id.into(),
            day,
            period: Period::Day,
            truth: TruthCause::Co {
                source: RiskSource::Unit { unit: uid(unit) },
                implicated_units: vec![uid(unit)],
            },
            classification: Classification::CpuSuspect,
            charged: true,
            provisional: false,
            reclassified_from: None,
            newest_step: None,
        }
    }

    pub(crate) fn background(id: &str, day: f64) -> Self {
        Self {
            truth: TruthCause::Background {},
            classification: Classification::NonCpu,
            charged: false,
            ..Self::co(id, day, 0)
        }
    }

    pub(crate) fn night(id: &str, day: f64) -> Self {
        Self {
            period: Period::Night,
            truth: TruthCause::NonCpu {},
            classification: Classification::NonCpu,
            charged: false,
            ..Self::co(id, day, 0)
        }
    }
}

/// The knobs of one cell's synthetic trajectory.
#[derive(Clone, Debug)]
pub(crate) struct Spec {
    pub(crate) scenario: String,
    pub(crate) seed: u64,
    pub(crate) slope: f64,
    pub(crate) mode: UnitMode,
    pub(crate) split: Split,
    pub(crate) run_profile: RunProfile,
    /// Final depth per unit, each at most 5; zero keeps the unit at stock, finished by inheritance.
    pub(crate) depths: Vec<u8>,
    /// Hazard ratio of every interval at a nonzero profile; stock carries none.
    pub(crate) ratio: f64,
    pub(crate) events: Vec<Ev>,
    pub(crate) restore: Option<Restore>,
    /// Override the derived outcome.
    pub(crate) outcome: Option<Outcome>,
    /// Stop observing at this day: a truncated run, or a harness error when `harness_error` is set.
    pub(crate) observed_until: Option<f64>,
    pub(crate) harness_error: bool,
}

impl Spec {
    pub(crate) fn new(scenario: &str, seed: u64, slope: f64) -> Self {
        Self {
            scenario: scenario.into(),
            seed,
            slope,
            mode: UnitMode::CcdUniform,
            split: Split::Dev,
            run_profile: RunProfile::Registered,
            depths: Vec::new(),
            ratio: 0.5,
            events: Vec::new(),
            restore: None,
            outcome: None,
            observed_until: None,
            harness_error: false,
        }
    }

    pub(crate) fn depths(mut self, depths: &[u8]) -> Self {
        self.depths = depths.to_vec();
        self
    }

    /// Stock forever: no promotion, every unit finished at day 1 by inheritance.
    pub(crate) fn stock(mut self) -> Self {
        self.depths = vec![0];
        self
    }

    pub(crate) fn ratio(mut self, ratio: f64) -> Self {
        self.ratio = ratio;
        self
    }

    pub(crate) fn event(mut self, event: Ev) -> Self {
        self.events.push(event);
        self
    }

    pub(crate) fn restore(mut self, restore: Restore) -> Self {
        self.restore = Some(restore);
        self
    }

    pub(crate) fn mode(mut self, mode: UnitMode) -> Self {
        self.mode = mode;
        self
    }
}

fn zero_profile(topology: &[CoreInfo]) -> PerCore<u8> {
    topology.iter().map(|core| (core.core, 0)).collect()
}

fn expand(membership: &[PolicyUnit], topology: &[CoreInfo], depths: &[u8]) -> PerCore<u8> {
    let mut profile = zero_profile(topology);
    for unit in membership {
        for core in &unit.cores {
            profile.set(*core, depths[unit.unit.index()]);
        }
    }
    profile
}

fn qualification(
    id: String,
    day: f64,
    epoch: &str,
    profile: PerCore<u8>,
    tag: &str,
) -> Qualification {
    Qualification {
        id,
        day,
        epoch: epoch.into(),
        profile,
        stock_controls: vec![format!("control-{tag}")],
        fresh_witnesses: vec![format!("fresh-{tag}")],
        reused_witnesses: vec![],
        safety_checks_satisfied: true,
    }
}

impl Spec {
    /// Builds a record that passes [`Record::validate`] for the lab's registration.
    #[expect(
        clippy::too_many_lines,
        reason = "one linear construction of every record section"
    )]
    pub(crate) fn build(&self, lab: &Lab) -> Record {
        let registration = &lab.registration;
        let scenario = registration
            .scenario(&self.scenario)
            .expect("scenario in the lab");
        let policy = registration.policy("successor").expect("successor policy");
        let SourceIdentity::Pinned {
            revision: source_revision,
            sha256: source_sha256,
        } = policy.source.clone()
        else {
            panic!("fixture policy source must be pinned")
        };
        let ccds = usize::from(scenario.topology.ccds);
        let per_ccd = usize::from(scenario.topology.cores_per_ccd);
        let topology: Vec<CoreInfo> = (0..ccds * per_ccd)
            .map(|index| CoreInfo {
                core: CoreId::new(i64::try_from(index).expect("core")).expect("core id"),
                ccd: CcdId::new(i64::try_from(index / per_ccd).expect("ccd")).expect("ccd id"),
                cpus: vec![CpuId::from(u16::try_from(index).expect("cpu"))],
            })
            .collect();
        let membership: Vec<PolicyUnit> = match self.mode {
            UnitMode::CcdUniform => (0..ccds)
                .map(|ccd| PolicyUnit {
                    unit: uid(ccd),
                    cores: topology
                        .iter()
                        .filter(|core| usize::from(core.ccd.get()) == ccd)
                        .map(|core| core.core)
                        .collect(),
                    kind: UnitPhase::Coarse,
                    parent: None,
                })
                .collect(),
            UnitMode::PerCore => topology
                .iter()
                .enumerate()
                .map(|(index, core)| PolicyUnit {
                    unit: uid(index),
                    cores: vec![core.core],
                    kind: UnitPhase::Core,
                    parent: Some(uid(usize::from(core.ccd.get()))),
                })
                .collect(),
        };
        let n = membership.len();
        let depths: Vec<u8> = (0..n)
            .map(|index| {
                self.depths
                    .get(index)
                    .or(self.depths.last())
                    .copied()
                    .unwrap_or(5)
            })
            .collect();
        let first_epoch = scenario.epochs[0].id.clone();
        let lambda = registration.lambda();

        // Applications: one climb per unit, then the optional firmware sequence.
        let mut steps: Vec<ProfileStep> = Vec::new();
        let mut current = zero_profile(&topology);
        let mut now = depths.iter().map(|_| 0_u8).collect::<Vec<_>>();
        let mut climb_day = 8.0;
        let mut finish_day: Vec<f64> = vec![1.0; n];
        for (index, depth) in depths.iter().enumerate().filter(|(_, depth)| **depth > 0) {
            now[index] = *depth;
            let after = expand(&membership, &topology, &now);
            steps.push(ProfileStep {
                id: format!("climb-{index}"),
                day: climb_day,
                before: current,
                after,
                kind: StepKind::Climb {
                    unit: uid(index),
                    ceiling_depth: 2 * depth,
                    clean_hold_hours: 50.0,
                    qualification: qualification(
                        format!("q-{index}"),
                        climb_day,
                        &first_epoch,
                        after,
                        &index.to_string(),
                    ),
                },
            });
            finish_day[index] = climb_day;
            current = after;
            climb_day += 7.0;
        }
        let pre = current;
        let mut restorations = Vec::new();
        let mut events: Vec<Ev> = self.events.clone();
        let mut stopped: Vec<UnitId> = (0..n)
            .filter(|index| depths[*index] == 0)
            .map(uid)
            .collect();
        let mut span: Option<(f64, f64)> = None;
        if let Some(restore) = self.restore {
            let epoch = scenario
                .epochs
                .get(1)
                .expect("a restoration scenario has a second epoch");
            let transition = f64::from(epoch.start_day);
            let zero = zero_profile(&topology);
            steps.push(ProfileStep {
                id: "reset".into(),
                day: transition,
                before: pre,
                after: zero,
                kind: StepKind::StockReset {
                    epoch: epoch.id.clone(),
                },
            });
            let applied = transition + 8.0;
            let restored_units: Vec<UnitId> =
                (0..n).filter(|index| depths[*index] > 0).map(uid).collect();
            let outcome = match restore {
                Restore::Blocked => RestorationOutcome::Blocked {
                    reason: "synthetic: the previous vector is not qualified".into(),
                },
                Restore::Retained | Restore::Crashed => {
                    steps.push(ProfileStep {
                        id: "restore".into(),
                        day: applied,
                        before: zero,
                        after: pre,
                        kind: StepKind::Restoration {
                            restoration: "fw".into(),
                        },
                    });
                    span = Some((
                        applied,
                        if restore == Restore::Crashed {
                            applied + 2.0
                        } else {
                            f64::INFINITY
                        },
                    ));
                    let result = if restore == Restore::Retained {
                        RestorationResult::Retained {}
                    } else {
                        let step_back = applied + 2.0;
                        events.push(Ev {
                            id: "crash".into(),
                            newest_step: Some(StepIdentity::Restoration {
                                restoration: "fw".into(),
                                units: restored_units.clone(),
                            }),
                            ..Ev::co(
                                "crash",
                                applied + 1.0,
                                restored_units.first().map_or(0, |unit| unit.index()),
                            )
                        });
                        steps.push(ProfileStep {
                            id: "step-back".into(),
                            day: step_back,
                            before: pre,
                            after: zero,
                            kind: StepKind::Retreat {
                                event: Some("crash".into()),
                                units: restored_units.clone(),
                            },
                        });
                        stopped.extend(restored_units.iter().copied());
                        RestorationResult::Crashed {
                            event: "crash".into(),
                            stock_step_back_day: step_back,
                            stock_readback: zero,
                            stopped_units: restored_units.clone(),
                            witnesses: vec!["crash-witness".into()],
                        }
                    };
                    RestorationOutcome::Applied {
                        day: applied,
                        qualification: qualification(
                            "rq".into(),
                            applied,
                            &epoch.id,
                            pre,
                            "restore",
                        ),
                        clean_stock_hours: 8.0 * ACTIVE_HOURS,
                        result,
                    }
                }
            };
            restorations.push(FirmwareRestoration {
                id: "fw".into(),
                epoch: epoch.id.clone(),
                transition_day: transition,
                previous_profile: pre,
                membership: membership.clone(),
                stock_reset_day: transition,
                stock_readback: zero,
                outcome,
            });
        }
        steps.sort_by(|a, b| a.day.total_cmp(&b.day));
        let in_span = |day: f64| span.is_some_and(|(applied, end)| day >= applied && day < end);
        let first_finish = finish_day.iter().copied().fold(1.0, f64::max);

        // Epochs.
        let epochs: Vec<Epoch> = scenario
            .epochs
            .iter()
            .enumerate()
            .map(|(index, epoch)| Epoch {
                id: epoch.id.clone(),
                start_day: f64::from(epoch.start_day),
                end_day: scenario
                    .epochs
                    .get(index + 1)
                    .map_or(HORIZON_END, |next| f64::from(next.start_day)),
                context: epoch.context.clone(),
                reference: DepthReference {
                    operational_max: 6 * u32::try_from(n).expect("n"),
                    certified_upper_bound: 6 * u32::try_from(n).expect("n"),
                    physical_max: 7 * u32::try_from(n).expect("n"),
                    per_unit_max_feasible: vec![7; n],
                    tolerance: 1e-9,
                    certificate: "analytic".into(),
                },
            })
            .collect();

        // Intervals split at epochs, applications and the original first finish.
        let until = self.observed_until.unwrap_or(HORIZON_END);
        steps.retain(|step| step.day < until);
        let mut cuts: Vec<f64> = vec![1.0, first_finish.min(until), until];
        cuts.extend(epochs.iter().map(|epoch| epoch.start_day));
        cuts.extend(steps.iter().map(|step| step.day));
        cuts.retain(|cut| *cut <= until);
        cuts.sort_by(f64::total_cmp);
        cuts.dedup();
        let profile_at = |day: f64| {
            steps
                .iter()
                .rfind(|step| step.day <= day)
                .map_or_else(|| zero_profile(&topology), |step| step.after)
        };
        let mut intervals = Vec::new();
        for pair in cuts.windows(2) {
            let (start, end) = (pair[0], pair[1]);
            let profile = profile_at(start);
            let hours = (end - start) * ACTIVE_HOURS;
            let nonzero = profile.iter().any(|(_, depth)| depth > 0);
            let ratio = if nonzero { self.ratio } else { 0.0 };
            let epoch = epochs
                .iter()
                .find(|epoch| epoch.start_day <= start && end <= epoch.end_day)
                .expect("interval epoch");
            intervals.push(ProfileInterval {
                start_day: start,
                end_day: end,
                profile,
                membership: membership.clone(),
                epoch: epoch.id.clone(),
                verified_daily_active_hours: hours,
                phase: if start >= first_finish {
                    Phase::PostFinish
                } else {
                    Phase::Learning
                },
                period: Period::Day,
                restoration_learning: in_span(start).then(|| "fw".to_string()),
                admissibility: Admissibility {
                    joint_ratio: ratio,
                    unit_ratios: vec![ratio; n],
                    member: ratio <= 1.0,
                },
                sources: (0..n)
                    .map(|index| SourceRisk {
                        source: RiskSource::Unit { unit: uid(index) },
                        expected_failures: ratio * lambda * hours
                            / f64::from(u32::try_from(n).expect("n")),
                    })
                    .collect(),
            });
        }

        // A night event gets a physical Night interval of one day either side of it inside the
        // Day interval that holds it: same profile and epoch, no verified daily hours. The
        // displaced hours and expected failures move into the adjoining Day interval, so the
        // daily T and H are those of the baseline.
        for event in events.iter().filter(|event| event.period == Period::Night) {
            let Some(at) = intervals.iter().position(|interval| {
                interval.period == Period::Day
                    && interval.start_day <= event.day
                    && event.day < interval.end_day
            }) else {
                continue;
            };
            let whole = intervals[at].clone();
            let lo = (event.day - 1.0).max(whole.start_day);
            let hi = (event.day + 1.0).min(whole.end_day);
            let length = whole.end_day - whole.start_day;
            let part = |from: f64, to: f64, period: Period, hours_from: f64, hours_to: f64| {
                let scale = (hours_to - hours_from) / length;
                let day = period == Period::Day;
                ProfileInterval {
                    start_day: from,
                    end_day: to,
                    period,
                    verified_daily_active_hours: if day {
                        whole.verified_daily_active_hours * scale
                    } else {
                        0.0
                    },
                    restoration_learning: if day {
                        whole.restoration_learning.clone()
                    } else {
                        None
                    },
                    sources: whole
                        .sources
                        .iter()
                        .map(|risk| SourceRisk {
                            source: risk.source.clone(),
                            expected_failures: if day {
                                risk.expected_failures * scale
                            } else {
                                0.0
                            },
                        })
                        .collect(),
                    ..whole.clone()
                }
            };
            let mut pieces = Vec::new();
            if lo > whole.start_day {
                // The before piece also carries the displaced middle.
                pieces.push(part(whole.start_day, lo, Period::Day, whole.start_day, hi));
                pieces.push(part(lo, hi, Period::Night, lo, hi));
                if hi < whole.end_day {
                    pieces.push(part(hi, whole.end_day, Period::Day, hi, whole.end_day));
                }
            } else {
                assert!(
                    hi < whole.end_day,
                    "a night event cannot displace a whole daily interval"
                );
                pieces.push(part(lo, hi, Period::Night, lo, hi));
                pieces.push(part(hi, whole.end_day, Period::Day, lo, whole.end_day));
            }
            intervals.splice(at..=at, pieces);
        }

        // Events with the deployed profile and original-boundary phase.
        events.sort_by(|a, b| a.day.total_cmp(&b.day));
        let machine_events: Vec<MachineEvent> = events
            .iter()
            .map(|event| {
                let phase = if event.day >= first_finish {
                    Phase::PostFinish
                } else {
                    Phase::Learning
                };
                let profile = intervals
                    .iter()
                    .find(|interval| {
                        interval.start_day <= event.day && event.day < interval.end_day
                    })
                    .expect("event interval")
                    .profile;
                MachineEvent {
                    policy_event: LearningEvent {
                        id: event.id.clone(),
                        day: event.day,
                        classification: event.classification,
                        provisional: event.provisional,
                        reclassified_from: event.reclassified_from,
                        charged: event.charged,
                        phase,
                        unit_phase: if self.mode == UnitMode::PerCore {
                            UnitPhase::Core
                        } else {
                            UnitPhase::Coarse
                        },
                        newest_step: event.newest_step.clone(),
                    },
                    period: event.period,
                    truth: event.truth.clone(),
                    phase,
                    restoration_learning: (event.period == Period::Day && in_span(event.day))
                        .then(|| "fw".to_string()),
                    profile,
                }
            })
            .collect();

        let units: Vec<UnitSummary> = (0..n)
            .map(|index| {
                let reason = if depths[index] == 0 {
                    FinishReason::InheritedStop
                } else {
                    FinishReason::Ceiling
                };
                UnitSummary {
                    unit: uid(index),
                    finish: Some(UnitFinish {
                        unit: uid(index),
                        reason,
                        day: finish_day[index],
                        witnesses: vec![format!("finish-{index}")],
                    }),
                    promotions: u64::from(depths[index] > 0),
                    retreats: Retreats {
                        co_counts: 0,
                        background_counts: 0,
                        non_cpu_counts: 0,
                        unattributed_counts: 0,
                        steps: u64::from(
                            self.restore == Some(Restore::Crashed) && depths[index] > 0,
                        ),
                    },
                    hold_hours: 50.0,
                    qualification_launches: 1,
                    qualification_hours: 1.0,
                    night_hours: 8.0,
                    ceiling_evidence: vec![],
                }
            })
            .collect();

        let allowance = ledger_for(&machine_events);
        let accounts = accounts_for(&intervals, &machine_events, n);
        let hours: f64 = intervals
            .iter()
            .map(|interval| interval.verified_daily_active_hours)
            .sum();
        let seconds = hours * 3600.0;
        let final_profile = intervals.last().expect("intervals").profile;
        let daily_co = machine_events
            .iter()
            .filter(|event| {
                event.period == Period::Day && matches!(event.truth, TruthCause::Co { .. })
            })
            .count();

        // Derived outcome: settled iff the final stretch is post-finish in A and, in a benign scenario, deep enough.
        let required = registration
            .criteria
            .useful_depth_of_reference
            .ceil_of(6 * u32::try_from(n).expect("n"));
        let final_depths: Vec<u8> = membership
            .iter()
            .map(|unit| final_profile.get(unit.cores[0]).expect("final depth"))
            .collect();
        let total: u64 = final_depths.iter().map(|depth| u64::from(*depth)).sum();
        let floor = final_depths.iter().all(|depth| {
            *depth
                >= registration
                    .criteria
                    .min_unit_depth
                    .min(7)
                    .max(scenario.unit_depth_floor.unwrap_or(0))
        });
        let final_ok = intervals
            .last()
            .is_some_and(|interval| interval.admissibility.member);
        let settled = final_ok && (!scenario.benign || (total >= required && floor));
        let observed = until;
        let outcome = self.outcome.clone().unwrap_or(
            if self.observed_until.is_some() && self.harness_error {
                Outcome::HarnessError {
                    observed_until_day: observed,
                    error: "synthetic harness failure".into(),
                }
            } else if self.observed_until.is_some() {
                let daily = machine_events
                    .iter()
                    .filter(|event| event.period == Period::Day)
                    .count();
                Outcome::Truncated {
                    observed_until_day: observed,
                    reason: "synthetic wall limit".into(),
                    lower_bound: PartialCounts {
                        daily_interruptions: daily as u64,
                        daily_co_crashes: daily_co as u64,
                        active_hours: hours,
                    },
                }
            } else if settled {
                Outcome::CompleteConverged {
                    observed_until_day: observed,
                }
            } else if daily_co > 0 {
                Outcome::CompleteUnconvergedWithCoCrashes {
                    observed_until_day: observed,
                }
            } else {
                Outcome::CompleteUnconvergedCrashFree {
                    observed_until_day: observed,
                }
            },
        );

        Record {
            provenance: Provenance {
                registration_id: registration.id.clone(),
                registration_sha256: lab.sha.clone(),
                runner_revision: registration.sources.runner_revision.clone(),
                source_revision,
                simulator_revision: registration.sources.simulator_revision.clone(),
                report_revision: registration.sources.report_revision.clone(),
                binary_sha256: registration.sources.binary_sha256.clone(),
                input_sha256: registration.sources.input_sha256.clone(),
                policy_identity: policy.identity.clone(),
                policy_source_sha256: source_sha256,
                parameters_sha256: policy.parameters_sha256.clone(),
                slope: self.slope,
                split: self.split,
                seed: self.seed,
                scenario: self.scenario.clone(),
                unit_mode: self.mode,
                topology,
                membership: membership.clone(),
                run_profile: self.run_profile,
            },
            epochs,
            intervals,
            events: machine_events.clone(),
            units: units.clone(),
            policy: PolicyReport {
                finishes: units
                    .iter()
                    .filter_map(|unit| unit.finish.clone())
                    .collect(),
                allowance: Some(allowance),
                events: machine_events
                    .iter()
                    .filter(|event| event.period == Period::Day)
                    .map(|event| event.policy_event.clone())
                    .collect(),
                stopped_units: stopped,
                inherited_stops: vec![],
                newest_step: None,
                readback: Some(final_profile),
                unit_membership: membership,
            },
            steps,
            restorations,
            accounts,
            exposure: Exposure {
                intended_seconds: seconds,
                observed_seconds: seconds,
                eligible_seconds: seconds,
                onset_active_seconds: seconds,
            },
            outcome,
            runtime: Runtime {
                wall_seconds: 1.0,
                compilation_seconds: 0.5,
                jobs: registration.resources.jobs,
                resource_class: registration.resources.resource_class.clone(),
                trial_launches: 4,
                invocation: Some(InvocationTiming {
                    id: format!("synthetic-{}", self.slope),
                    elapsed_seconds: 1_000.0,
                }),
            },
        }
    }
}

fn ledger_for(events: &[MachineEvent]) -> AllowanceLedger {
    let charged: Vec<&MachineEvent> = events
        .iter()
        .filter(|event| event.policy_event.charged)
        .collect();
    AllowanceLedger {
        limit: 2,
        consumed: charged.len() as u64,
        provisional: charged
            .iter()
            .filter(|event| event.policy_event.provisional)
            .count() as u64,
        reclassified: charged
            .iter()
            .filter(|event| event.policy_event.reclassified_from.is_some())
            .count() as u64,
        exhausted_day: charged.get(1).map(|event| event.policy_event.day),
    }
}

fn accounts_for(
    intervals: &[ProfileInterval],
    events: &[MachineEvent],
    units: usize,
) -> RiskAccounts {
    let mut sources: Vec<RiskSource> = (0..units)
        .map(|index| RiskSource::Unit { unit: uid(index) })
        .collect();
    for event in events {
        if let TruthCause::Co { source, .. } = &event.truth
            && !sources.contains(source)
        {
            sources.push(source.clone());
        }
    }
    let account = |phase: Phase| -> Vec<SourceAccount> {
        let hours: f64 = intervals
            .iter()
            .filter(|interval| interval.phase == phase)
            .map(|interval| interval.verified_daily_active_hours)
            .sum();
        sources
            .iter()
            .map(|source| SourceAccount {
                source: source.clone(),
                k: events.iter().filter(|event| event.period == Period::Day && event.phase == phase && matches!(&event.truth, TruthCause::Co { source: found, .. } if found == source)).count() as u64,
                active_hours: hours,
                expected_failures: intervals.iter().filter(|interval| interval.phase == phase).flat_map(|interval| &interval.sources).filter(|risk| &risk.source == source).map(|risk| risk.expected_failures).sum(),
            })
            .collect()
    };
    RiskAccounts {
        learning: account(Phase::Learning),
        post_finish: account(Phase::PostFinish),
    }
}

/// Re-derives the ledger, K/T/H accounts and exposure after a test edits intervals or events.
pub(crate) fn resync(record: &mut Record) {
    if record.policy.allowance.is_some() {
        record.policy.allowance = Some(ledger_for(&record.events));
    }
    record.accounts = accounts_for(&record.intervals, &record.events, record.units.len());
    let seconds = record
        .intervals
        .iter()
        .map(|interval| interval.verified_daily_active_hours)
        .sum::<f64>()
        * 3600.0;
    record.exposure = Exposure {
        intended_seconds: seconds,
        observed_seconds: seconds,
        eligible_seconds: seconds,
        onset_active_seconds: seconds,
    };
    record.policy.events = record
        .events
        .iter()
        .filter(|event| event.period == Period::Day)
        .map(|event| event.policy_event.clone())
        .collect();
}

/// A `n`-unit registration scenario clone with a different topology, for unit-generic checks.
pub(crate) fn retopologize(lab: &mut Lab, scenario: &str, ccds: u8, cores_per_ccd: u8) {
    let scenario = lab
        .registration
        .scenarios
        .iter_mut()
        .find(|item| item.name == scenario)
        .expect("scenario in the lab");
    scenario.topology.ccds = ccds;
    scenario.topology.cores_per_ccd = cores_per_ccd;
}
