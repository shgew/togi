use std::collections::BTreeMap;
use std::num::NonZeroU32;

use jiff::Timestamp;
use togi_machine::{
    BankType, BiosContext, BootId, CcdId, CcdProfile, Condition, ContentId, CoreId, CoreInfo,
    CoreSet, CpuId, Digest, EndEvidence, GraphicsDriver, KernelCursor, KernelFault, KernelTail,
    LoadSchedule, MceCount, ObservedOffset, PerCore, PstoreRecord, Regime, ResetKind, ResetReason,
    Signal, TargetOffset, WatchdogState, WorkloadId,
};

use crate::*;

pub(crate) fn config() -> ConfigSnapshot {
    ConfigSnapshot {
        start_offsets: Some([(core(3), target(-10))].into_iter().collect()),
        backends: ConfigBackends {
            mprime: Some("/nix/store/x-mprime".into()),
            ycruncher: None,
        },
        ..bare_config()
    }
}

pub(crate) fn bare_config() -> ConfigSnapshot {
    ConfigSnapshot {
        start_offsets: Some(PerCore::default()),
        candidate_solo_limits: Some(PerCore::default()),
        durations: Some(ConfigDurations {
            search_trial_s: 90,
            short_trial_s: 120,
            checking_trial_s: 120,
            checking_idle_s: 900,
            checking_all_core_s: 1200,
        }),
        evidence: Some(ConfigEvidence {
            miss: 0.05,
            rate: 0.5,
        }),
        checking: Some(ConfigChecking {
            cycle: vec![Regime::R7, Regime::R2, Regime::R1],
        }),
        dead_ends: ConfigDeadEnds {
            inconclusive_in_a_row: 3,
            stray_crashes_in_a_row: 3,
        },
        backends: ConfigBackends::default(),
        backend_user: None,
    }
}

pub(crate) fn workload(id: &str) -> WorkloadId {
    id.parse().unwrap()
}

pub(crate) fn core(id: u8) -> CoreId {
    CoreId::new(i64::from(id)).unwrap()
}

pub(crate) fn target(offset: i64) -> TargetOffset {
    TargetOffset::new(offset).unwrap()
}

pub(crate) fn set(ids: &[u8]) -> CoreSet {
    ids.iter().map(|id| core(*id)).collect()
}

pub(crate) fn seq(value: u64) -> Seq {
    Seq::new(value).unwrap()
}

pub(crate) fn ord(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap()
}

pub(crate) fn profile(offsets: &[i64]) -> PerCore<TargetOffset> {
    offsets
        .iter()
        .enumerate()
        .map(|(index, offset)| (core(u8::try_from(index).unwrap()), target(*offset)))
        .collect()
}

pub(crate) fn build() -> Build {
    Build {
        version: "0.1.0".into(),
        rev: Some("abc1234".into()),
        ruleset: 10,
        schema: SCHEMA,
        fixes: FixStamp::through(defect(2)),
        evidence: 1,
    }
}

pub(crate) fn defect(value: u32) -> DefectId {
    DefectId::new(value).unwrap()
}

pub(crate) fn session() -> SessionId {
    "20261002T011407Z".parse().unwrap()
}

pub(crate) fn trial() -> TrialId {
    TrialId::from("0413".to_owned())
}

pub(crate) fn boot() -> BootId {
    BootId::from("e8f9aaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned())
}

pub(crate) fn failure() -> Failure {
    Failure {
        signal: Signal::Crash,
        attribution: Attribution::Attributed,
        core: Some(core(3)),
        offset: Some(target(-30)),
        trial: Some(trial()),
        regime: Some(Regime::R2),
        condition: Some(Condition::Alone),
        profile: Some(profile(&[0, 0, 0, -30])),
        known_failure: None,
        reason: None,
        round: None,
    }
}

fn fact_source() -> FactSource {
    FactSource {
        session: session(),
        seq: seq(819),
        build: build(),
        trial: Some(trial()),
        time: "2026-10-02T01:15:37.460Z".parse::<Timestamp>().unwrap(),
        boot: boot(),
        backend: Some(BackendIdentity {
            backend: togi_machine::Backend::Mprime,
            path: "/nix/store/x-mprime".into(),
        }),
    }
}

fn class() -> TrialClass {
    TrialClass {
        regime: Regime::R2,
        workload: Some(workload("mprime-avx2-36k-248k")),
        cores: set(&[3]),
        duration_s: 90,
    }
}

fn trial_end() -> TrialEnd {
    TrialEnd {
        trial: trial(),
        kernel_cursor: Some(KernelCursor::from("s=1;i=2".to_owned())),
        kernel_error: None,
        excluded: Vec::new(),
        outcome: Outcome::Failure,
        signal: Some(Signal::ComputationError),
        core: Some(core(3)),
        duration_s: 41,
        tctl_max_c: Some(71),
        voltage_request_median_v: Some(1.1),
        voltage_request_min_v: Some(1.05),
        voltage_requests_v: [(core(3), 1.125_f64), (core(5), 1.1)].into_iter().collect(),
        top_requesters: set(&[3]),
        ccd_mhz: BTreeMap::from([
            (CcdId::new(0).unwrap(), 5420),
            (CcdId::new(1).unwrap(), 5380),
        ]),
        last_sample_s: Some(40),
        last_sample_tctl_c: Some(70),
        last_sample_min_mhz: Some(5400),
        last_sample_max_mhz: Some(5600),
        stalled_core: Some(core(3)),
        worker_stalled_ms: Some(30_500),
        reason: None,
        interrupted: false,
        last_evidence: true,
        escaped: vec![CpuId::from(7)],
        backend_missing: false,
        containment_error: Some("scope still has a process".into()),
    }
}

pub(crate) fn member(id: u8, offset: i64) -> Member {
    Member {
        core: core(id),
        offset: target(offset),
    }
}

pub(crate) fn samples() -> Vec<Payload> {
    let cores = vec![
        CoreInfo {
            core: core(0),
            ccd: CcdId::new(0).unwrap(),
            cpus: vec![CpuId::from(0), CpuId::from(8)],
        },
        CoreInfo {
            core: core(1),
            ccd: CcdId::new(0).unwrap(),
            cpus: vec![CpuId::from(1), CpuId::from(9)],
        },
    ];
    let command = SmuCommand::Set {
        core: core(3),
        offset: target(-20),
    };
    vec![
        SessionStart {
            build: build(),
            session: session(),
            cores,
        }
        .into(),
        SessionContext::from(BiosContext {
            bios_version: "F31".into(),
            board: "X570".into(),
            cpu_model: "5950X".into(),
            microcode: "0xa201016".into(),
            boost_limit_mhz: 5000,
        })
        .into(),
        SessionBaseline {
            offsets: [(core(0), ObservedOffset(0)), (core(1), ObservedOffset(-5))]
                .into_iter()
                .collect(),
        }
        .into(),
        SessionNotice {
            notice: Notice::NonzeroBaseline,
            cores: set(&[1]),
        }
        .into(),
        SessionWarning {
            operation: "write state projection".into(),
            trial: Some(trial()),
            error: "disk full".into(),
        }
        .into(),
        SessionArchived { session: session() }.into(),
        SessionCarried {
            sources: vec![CarriedSource {
                session: session(),
                schema: 1,
                ruleset: 9,
            }],
            failure_points: true,
            detail: None,
            carried: vec![CarriedCore {
                core: core(3),
                candidate_solo_limit: Some(CarriedLimit {
                    offset: target(-25),
                    from: Provenance {
                        session: session(),
                        seq: seq(40),
                    },
                }),
                failure_point: Some(CarriedFailurePoint {
                    offset: target(-30),
                    from: Provenance {
                        session: session(),
                        seq: seq(44),
                    },
                    signal: Signal::Crash,
                }),
            }],
        }
        .into(),
        ConfigLoaded {
            build: build(),
            config: config(),
            path: "/etc/togi/config.json".into(),
            file: true,
            kernel_cursor: Some(KernelCursor::from("s=1".to_owned())),
            kernel_error: None,
        }
        .into(),
        PreflightCheck {
            check: "pm_table".into(),
            detail: "present".into(),
            ok: true,
        }
        .into(),
        Payload::SmuIntent(SmuIntentRecord::new(command)),
        Payload::SmuWrite(SmuWriteRecord::new(SmuCommand::SetAll {
            offset: target(0),
        })),
        SmuReadback {
            core: core(3),
            offset: ObservedOffset(-20),
            expected: Some(target(-20)),
        }
        .into(),
        SmuError {
            op: SmuOp::Read,
            core: Some(core(3)),
            offset: None,
            error: "mailbox timeout".into(),
        }
        .into(),
        ProfileApplied {
            offsets: profile(&[0, -5, -10]),
            condition: Condition::Together,
        }
        .into(),
        ProfileChange {
            from: None,
            to: profile(&[0, -5]),
        }
        .into(),
        ProfileRestored {
            offsets: profile(&[0, -5]),
        }
        .into(),
        TrialIntent {
            task_start: None,
            trial: trial(),
            kernel_cursor: None,
            kernel_error: Some("cursor lost".into()),
            profile: profile(&[0, 0, 0, -32]),
            core: Some(core(3)),
            cores: CoreSet::default(),
            offset: Some(target(-32)),
            regime: Regime::R2,
            workload: workload("mprime-avx2-36k-248k"),
            duration_s: 90,
            condition: Condition::Alone,
            activity: Some(Activity::Mapping),
            night: Some(night()),
            task: Some(TaskId {
                queue: Queue::Mapping,
                ordinal: 4,
            }),
            content: vec![workload_content()],
            placement: Some(Placement::Isolated),
            tested: Some(Tested::Isolated {
                ccd: ccd(0),
                offset: target(-32),
            }),
            retry: true,
            cycle: Some(ord(2)),
            step: Some(ord(1)),
            hunt: Some(ord(3)),
            group: Some(ord(4)),
            round: Some(ord(5)),
            rerun: true,
        }
        .into(),
        TrialStart {
            trial: trial(),
            window_start_ns: Some(12_345),
            scope: "togi-trial-0413".into(),
            pid: 48211,
            cpus: vec![CpuId::from(7)],
            argv: vec!["mprime".into(), "-t".into()],
            files: vec!["samples.jsonl".into()],
            instances: vec![TrialInstance {
                core: core(3),
                cpus: vec![CpuId::from(3), CpuId::from(11)],
                pid: 48211,
                scope: "togi-trial-0413-c03".into(),
            }],
            roles: Vec::new(),
            controller: None,
        }
        .into(),
        TrialProgress {
            trial: trial(),
            detail: "core 03 backend exited early".into(),
            signal: Some(Signal::UnexpectedExit),
            core: Some(core(3)),
            pid: None,
            fault: None,
        }
        .into(),
        TrialSignal {
            trial: trial(),
            schedule: Some(LoadSchedule::Random { seed: 99 }),
            stops: 4,
            conts: 3,
        }
        .into(),
        TrialSample {
            trial: trial(),
            warning: "escaped the scope".into(),
            pid: 1,
            tid: 2,
            cpu: CpuId::from(3),
        }
        .into(),
        trial_end().into(),
        TrialCarried {
            source: fact_source(),
            class: class(),
            condition: Condition::Alone,
            activity: Some(Activity::Search),
            rerun: false,
            profile: profile(&[0, 0, 0, -30]),
            outcome: Outcome::Pass,
            signal: None,
            duration_s: 90,
            core: None,
            stalled_core: None,
            voltage_requests_v: [(core(3), 1.1)].into_iter().collect(),
            top_requesters: set(&[3]),
            ccd_mhz: BTreeMap::from([(CcdId::new(0).unwrap(), 5400)]),
        }
        .into(),
        failure().into(),
        FailureCarried {
            source: FactSource {
                trial: None,
                ..fact_source()
            },
            class: TrialClass {
                regime: Regime::R6,
                workload: None,
                cores: set(&[0, 1, 2, 3]),
                duration_s: 0,
            },
            failure: Failure {
                attribution: Attribution::Unattributed,
                core: None,
                offset: None,
                trial: None,
                ..failure()
            },
        }
        .into(),
        Mce {
            cpu: CpuId::from(3),
            core: Some(core(3)),
            bank: 5,
            bank_type: BankType::ExecutionUnit,
            corrected: false,
            monotonic_ns: Some(0),
            from_boot: Some(boot()),
            trial: Some(trial()),
            between_trials: false,
            lines: vec!["raw line".into()],
        }
        .into(),
        CrashDetected {
            previous_boot: boot(),
            in_flight: Some(seq(12)),
            stray: true,
            condition: Some(Condition::Together),
            reset_reason: Some(ResetKind::Watchdog),
            reset_reason_raw: vec!["watchdog".into()],
            inconclusive: true,
            pstore: Some(PstoreRecord {
                path: "/sys/fs/pstore/dmesg-1".into(),
                lines: vec!["oops".into()],
            }),
        }
        .into(),
        TunerDecision {
            core: core(7),
            activity: Activity::Search,
            decision: Decision::StepDeeper,
            from_offset: target(-32),
            to_offset: target(-37),
            pass: Some(target(-32)),
            failure_point: None,
            workloads: vec![workload("mprime-sse-4k-21k")],
            reason: "coarse, no failure point yet".into(),
        }
        .into(),
        CoreTransition {
            core: core(7),
            from: Some(CorePhase::Search),
            to: CorePhase::HasRoom,
            offset: target(-32),
            pass: None,
            failure_point: Some(target(-40)),
            check_solo_limit: true,
            workloads: vec![],
            cleared_combination: vec![ord(1)],
            reason: "passed".into(),
        }
        .into(),
        CheckingCycle {
            cycle: ord(1),
            event: CycleEvent::Start,
            passed: false,
            full: true,
            missing: vec!["R3".into()],
            steps: vec![Regime::R1, Regime::R7],
            reason: Some("phase 1".into()),
        }
        .into(),
        CheckingStep {
            cycle: ord(1),
            step: ord(1),
            profile: profile(&[0, -5]),
        }
        .into(),
        CheckingChain {
            cycle: ord(1),
            step: ord(1),
            ccd: CcdId::new(0).unwrap(),
            workload: workload("mprime-avx2-36k-248k"),
            groups: vec![set(&[1, 2]), set(&[3])],
            cores: set(&[3, 4]),
            source_seqs: vec![seq(5), seq(6)],
            profile: profile(&[0, -5]),
            part: ord(2),
            rederived: Some(Rederived::Changed),
        }
        .into(),
        HostRanking {
            ranking: vec![core(3), core(1)],
            values: [(core(3), 200_i64), (core(1), 190)].into_iter().collect(),
            detail: None,
        }
        .into(),
        HuntStart {
            hunt: ord(3),
            failure: seq(904),
            trial: Some(trial()),
            regime: Regime::R7,
            workload: workload("mprime-avx2-36k-248k"),
            cores: set(&[0, 1, 2, 3]),
            duration_s: 120,
            failing: profile(&[-10, -20, -30, -40]),
            parked: profile(&[0, 0, 0, 0]),
            parked_seq: Some(seq(811)),
            candidates: set(&[0, 1]),
            trials: 5,
            trial_s: 120,
            miss: 0.01,
            rate: 0.5,
            ranking: vec![core(1), core(0)],
            reason: Some("known failure".into()),
        }
        .into(),
        HuntGroup {
            hunt: ord(3),
            group: ord(4),
            cores: set(&[0, 1]),
            profile: profile(&[-10, -20]),
            set: set(&[0, 1]),
            granularity: 2,
            stage: HuntStage::Probe,
            index: 1,
            duration_s: 120,
            escalated: true,
            full_checked: true,
            any_failed: true,
            probe: Some(member(1, -22)),
            held: vec![member(0, -40)],
            inferred: Some(Inference::Failure),
            skipped: false,
            reason: Some("inferred".into()),
        }
        .into(),
        HuntEnd {
            hunt: ord(3),
            result: HuntOutcome::Combination,
            cores: set(&[0, 1]),
            members: vec![member(0, -40), member(1, -30)],
            groups: 4,
            reason: "probed".into(),
        }
        .into(),
        HuntSkipped {
            failure: seq(904),
            reason: "held".into(),
        }
        .into(),
        Combination {
            combination: ord(2),
            members: vec![member(3, -40), member(11, -30)],
            fallback: true,
            hunt: ord(4),
            reason: "observed".into(),
        }
        .into(),
        DeepeningRound {
            round: ord(2),
            event: RoundEvent::Start,
            base: profile(&[0, 0]),
            base_seq: Some(seq(900)),
            target: profile(&[-10, -20]),
            profile: profile(&[-5, -10]),
            cores: set(&[0, 1]),
            ranking: vec![core(1), core(0)],
            trials: Some(3),
            trial_s: Some(60),
            passed: false,
            reason: Some("5 cores toward their solo limits".into()),
        }
        .into(),
        TunerWarning {
            warning: WarningKind::Monotonicity,
            trial: Some(trial()),
            passes: vec![seq(5), seq(9)],
            detail: "failed at -30".into(),
        }
        .into(),
        BackendRetry {
            backend: RetryTarget::KernelLog,
            attempt: 2,
            wait_s: 300,
            reason: "setup failed".into(),
        }
        .into(),
        CommandReset {
            target: ResetTarget::Core(core(4)),
        }
        .into(),
        DefectFound {
            id: defect(1),
            title: "false failure on power-off".into(),
            detail: "clean exit".into(),
            pr: 14,
            direction: DefectDirection::TooCautious,
            cores: set(&[2]),
            decisions: vec![seq(20)],
        }
        .into(),
        DefectAnswered {
            id: defect(1),
            cores: set(&[2]),
            answer: DefectAnswer::Yes,
        }
        .into(),
        DeadEnd {
            condition: DeadEndCondition::FailureAtZero,
            core: Some(core(2)),
            detail: "failed at CO 0".into(),
            action: Some(DeadEndAction::ClearSavedEntryAndReboot),
        }
        .into(),
        BootSavedEntry {
            before: Some("0".into()),
            after: None,
            error: None,
            reason_error: Some("write failed".into()),
        }
        .into(),
        BootLeaveReason {
            reason_id: "r1".into(),
            restart_limit_count: 3,
            reason: "restart limit".into(),
        }
        .into(),
        Shutdown {
            reason: ShutdownReason::Cycles,
            kernel_cursor: None,
            kernel_error: None,
            cycles: Some(ord(2)),
        }
        .into(),
        JournalTorn {
            offset: 1024,
            bytes_hex: "7b22".into(),
        }
        .into(),
        StateRebuilt {
            fields: vec!["last_seq".into()],
        }
        .into(),
        DayBootStart {
            build: build(),
            bios: bios(),
            offsets: day_offsets(0),
            watchdog: Some(WatchdogState::Armed {
                detail: "watchdog0 (SP5100 TCO timer) active, held by PID 1".into(),
            }),
        }
        .into(),
        DayCheckpoint {
            applied_secs: 3600,
            load_mix: LoadMix::default(),
            watchdog: None,
        }
        .into(),
        DayOffsetsChanged {
            offsets: day_offsets(-10),
        }
        .into(),
        DayCleanStop {}.into(),
        DayUncleanEnd {
            boot: BootId::from("7a1b2c3d-0000-0000-0000-000000000000".to_owned()),
            last_checkpoint: Some(seq(7)),
            offsets: day_offsets(-10),
            reset_reason: ResetReason {
                kind: Some(ResetKind::Watchdog),
                raw: vec!["watchdog".into()],
                supported: true,
            },
            evidence: EndEvidence {
                crashed_mces: MceCount::Read {
                    corrected: 0,
                    uncorrected: 0,
                },
                successor_mces: MceCount::Unknown {},
                tail: KernelTail::Faults {
                    faults: vec![KernelFault::GraphicsTimeout {
                        driver: GraphicsDriver::Amdgpu,
                    }],
                    logging_continued: true,
                },
                asleep: None,
            },
        }
        .into(),
        DayAnswer {
            boot: BootId::from("7a1b2c3d-0000-0000-0000-000000000000".to_owned()),
            offsets: day_offsets(-10),
            answer: DayAnswerKind::Graphics,
            uid: 1000,
        }
        .into(),
        DayRecord {
            boot: CrashedBoot::Unobserved {
                at: "2026-10-19T11:00:00Z".parse().unwrap(),
            },
            offsets: RecordedOffsets::new(day_offsets(-25)).unwrap(),
            source: OffsetSource::Entered {
                at: "2026-10-19T12:00:00Z".parse().unwrap(),
            },
            running: Running::Build,
            note: Some("a Nix build".into()),
        }
        .into(),
        crate::DayFault {
            fault: crate::LiveFault::MachineCheck {
                cpu: togi_machine::CpuId::from(2_u16),
                core: Some(core(1)),
                bank: 0,
                bank_type: togi_machine::BankType::LoadStore,
                corrected: false,
            },
            monotonic_ns: 1_500_000_000,
            offsets: day_offsets(-10),
            lines: vec!["[Hardware Error]: Uncorrected".into()],
        }
        .into(),
        ExposureObserved {
            source: OriginalSource {
                session: session(),
                seq: seq(819),
                boot: boot(),
                trial: Some(trial()),
            },
            origin: ExposureOrigin::Live {},
            firmware: bios(),
            epoch: 1,
            class: exposure_class(),
            profile: ccd_vector(&[-20, 0]),
            tested: Tested::Isolated {
                ccd: ccd(0),
                offset: target(-20),
            },
            activity: Activity::Mapping,
            outcome: Outcome::Pass,
            failures: Vec::new(),
            eligible_s: 120,
            uninterrupted_s: 120,
            exclusions: Vec::new(),
        }
        .into(),
        CcdEdge {
            ccd: ccd(0),
            edge: Edge::Observed {
                offset: target(-25),
                witness: seq(40),
            },
            old: Edge::Unknown {},
            reason: "first failure while mapping".into(),
        }
        .into(),
        CcdCap {
            ccd: ccd(1),
            old: target(-10),
            new: target(-5),
            budget: Rate::new(1.0 / 6000.0).unwrap(),
            edge: Edge::Censored {
                limit: target(-50),
                witnesses: vec![seq(30), seq(31)],
            },
            adequate: vec![exposure_class()],
            uncovered: vec![UncoveredClass {
                class: ExposureClass {
                    regime: Regime::R9,
                    ..exposure_class()
                },
                reason: UncoveredReason::MappingInadequate,
            }],
            cells: vec![RateCell {
                class: exposure_class(),
                offset: target(-20),
                eligible_s: 3600,
                failures: 0,
                bound: RateBound::Finite {
                    per_hour: Rate::new(0.003).unwrap(),
                },
            }],
            slope: Slope::Fallback {
                beta: Finite::new(1.5_f64.ln()).unwrap(),
            },
            candidates: vec![Candidate {
                class: exposure_class(),
                offset: target(-5),
            }],
            binding: Binding::Rate {
                class: exposure_class(),
            },
        }
        .into(),
        NightPlan {
            night: Some(night()),
            budgets: vec![Budget {
                queue: Queue::Mapping,
                share_pct: 10,
                consumed_s: 120,
            }],
            tasks: vec![PlannedTask {
                task: TaskId {
                    queue: Queue::Mapping,
                    ordinal: 4,
                },
                status: TaskStatus::Running,
            }],
            unavailable: vec![UnavailableBackend {
                backend: togi_machine::Backend::Ycruncher,
                causes: vec![Seq::new(40).unwrap(), Seq::new(41).unwrap()],
            }],
        }
        .into(),
        TaskStart {
            night: night(),
            task: TaskId {
                queue: Queue::Mapping,
                ordinal: 4,
            },
        }
        .into(),
        NightQualification {
            profile: ccd_vector(&[-20, -10]),
            firmware: bios(),
            epoch: 1,
            contents: vec![workload_content()],
            matrix: Digest::from_bytes([7; 32]),
            purpose: QualificationPurpose::NextStep,
            state: QualificationState::Complete {
                witnesses: vec![CellWitness {
                    cell: exposure_class(),
                    evidence: CellEvidence::Fresh { exposure: seq(60) },
                }],
            },
        }
        .into(),
        NightReproduction {
            source: DayEventId::Boot {
                boot: BootId::from("7a1b2c3d-0000-0000-0000-000000000000".to_owned()),
            },
            target: Some(ReproductionTarget::Isolated {
                ccd: ccd(1),
                offset: target(-10),
            }),
            night: night(),
            consumed_s: 1800,
            remaining: vec![exposure_class()],
            failures: vec![seq(70)],
            state: ReproductionState::Pending,
        }
        .into(),
        DayStep {
            decision: DecisionId(3),
            cold_boot: boot(),
            old: ccd_vector(&[-10, -10]),
            proposed: ccd_vector(&[-15, -10]),
            step: StepKind::Promote { changed: ccd(0) },
            predecessor: Some(DecisionId(2)),
            hours: vec![seq(90), seq(91)],
            hold_s: 180_000,
            causes: StepCauses {
                caps: vec![seq(80)],
                qualification: Some(seq(60)),
                vetoes: Vec::new(),
                events: Vec::new(),
            },
            reason: "50 applied hours at the current profile".into(),
        }
        .into(),
        DayApplied {
            application: ApplicationId(5),
            cause: ApplyCause::Step {
                decision: DecisionId(3),
            },
            target: ccd_vector(&[-15, -10]),
            readbacks: day_offsets(-15),
            transactions: vec![seq(100), seq(101), seq(102)],
            boot: boot(),
            mono_ns: 9_000_000_000,
        }
        .into(),
        DayEvent {
            event: DayEventId::Boot {
                boot: BootId::from("7a1b2c3d-0000-0000-0000-000000000000".to_owned()),
            },
            profile: observed_ccd(&ccd_vector(&[-15, -10])),
            boot: Some(BootId::from(
                "7a1b2c3d-0000-0000-0000-000000000000".to_owned(),
            )),
            from: "2026-10-01T08:00:00Z".parse::<Timestamp>().unwrap(),
            to: "2026-10-01T09:00:00Z".parse::<Timestamp>().unwrap(),
            witnesses: vec![seq(7), seq(8)],
            classification: Classification::CpuSuspect {
                certainty: Certainty::Provisional,
            },
            resolution: Resolution::Unresolved {},
            reason: "unexplained end at a nonzero day profile".into(),
            predecessor: None,
            escalation: Some(Escalation::RevertNewest {
                reverted: DecisionId(3),
            }),
            cursors: vec![CcdCursor {
                ccd: ccd(0),
                depth: 1,
            }],
            consumed: vec![DecisionId(3)],
            reproduction: ReproductionState::Pending,
        }
        .into(),
        DayVeto {
            event: DayEventId::Record {
                session: session(),
                seq: seq(12),
            },
            profile: ObservedProfile::PerCore {
                cores: day_offsets(-15),
            },
            state: VetoState::Active,
            evidence: vec![seq(7)],
            reason: "CPU-suspect event at this exact profile".into(),
        }
        .into(),
        DayHold {
            profile: ccd_vector(&[-15, -10]),
            application: ApplicationId(5),
            credited: vec![seq(90), seq(91)],
            eligible_s: 7200,
            change: HoldChange::Credited {},
        }
        .into(),
        DayFreeze {
            choice: FreezeChoice::Freeze,
            source: FreezeSource::Command { uid: 1000 },
        }
        .into(),
        DayInterval {
            boot: boot(),
            start_ns: 1_000_000_000,
            end_ns: 601_000_000_000,
            profile: ObservedProfile::PerCore {
                cores: day_offsets(-15),
            },
            application: Some(ApplicationId(5)),
            state: IntervalState::Applied,
            checkpoints: vec![seq(90), seq(91)],
            reason: "checkpointed with unchanged readbacks".into(),
        }
        .into(),
        DayImported {
            original: Provenance {
                session: session(),
                seq: seq(25),
            },
            original_kind: EventKind::DayCheckpoint,
            raw: r#"{"applied_secs":3600}"#.into(),
            time: "2026-09-30T10:00:00Z".parse::<Timestamp>().unwrap(),
            boot: boot(),
            build: None,
            firmware: None,
            profile: None,
            class: ImportClass::Checkpoint,
            missing: vec![
                MissingFact::Build,
                MissingFact::Firmware,
                MissingFact::Profile,
            ],
        }
        .into(),
        SessionImported {
            sources: vec![CarriedSource {
                session: session(),
                schema: 1,
                ruleset: 12,
            }],
            selected: vec![Provenance {
                session: session(),
                seq: seq(25),
            }],
            counts: ImportCounts {
                selected: 1,
                imported: 1,
                duplicates: 0,
                unknown: 1,
                excluded: 1,
            },
            unknown: vec![ImportedRecordRef {
                original: Provenance {
                    session: session(),
                    seq: seq(26),
                },
                kind: "day.future".into(),
            }],
            excluded: vec![ImportedRecordRef {
                original: Provenance {
                    session: session(),
                    seq: seq(27),
                },
                kind: "trial.end".into(),
            }],
            reason: "fresh Ruleset 13 transition; no tuning carry".into(),
        }
        .into(),
        DayPause {
            state: PauseState::Until {
                deadline: "2026-10-02T03:00:00Z".parse::<Timestamp>().unwrap(),
            },
            source: PauseSource::Command { uid: 1000 },
        }
        .into(),
        StockAttested { firmware: bios() }.into(),
    ]
    .into_iter()
    .chain(super::ruleset14::samples())
    .collect()
}

fn ccd(id: u8) -> CcdId {
    CcdId::new(i64::from(id)).unwrap()
}

fn night() -> NightId {
    NightId(Seq::new(1).unwrap())
}

pub(crate) fn ccd_vector(offsets: &[i64]) -> CcdVector {
    let topology: Vec<_> = (0..u8::try_from(offsets.len()).unwrap())
        .map(|id| CoreInfo {
            core: core(id),
            ccd: ccd(id),
            cpus: vec![CpuId::from(u16::from(id))],
        })
        .collect();
    let profile = CcdProfile::try_from(
        offsets
            .iter()
            .enumerate()
            .map(|(id, offset)| (ccd(u8::try_from(id).unwrap()), target(*offset)))
            .collect::<BTreeMap<_, _>>(),
    )
    .unwrap();
    CcdVector::expand(profile, &topology).unwrap()
}

/// The day observer's record of `vector` applied exactly.
fn observed_ccd(vector: &CcdVector) -> ObservedProfile {
    ObservedProfile::Ccd {
        ccd: vector.ccd().clone(),
        cores: vector
            .cores()
            .iter()
            .map(|(core, offset)| (core, offset.observed()))
            .collect(),
    }
}

fn workload_content() -> WorkloadContent {
    WorkloadContent {
        workload: workload("mprime-avx2-36k-248k"),
        content: ContentId {
            backend: togi_machine::Backend::Mprime,
            executable: Digest::from_bytes([1; 32]),
            configuration: Digest::from_bytes([2; 32]),
        },
    }
}

fn exposure_class() -> ExposureClass {
    ExposureClass {
        regime: Regime::R8,
        workloads: vec![workload_content()],
        cores: set(&[3]),
        position: Some(Position::Light { core: core(3) }),
        duration_s: 120,
        placement: Placement::Isolated,
    }
}

pub(crate) fn bios() -> BiosContext {
    BiosContext {
        bios_version: "F31".into(),
        board: "X570".into(),
        cpu_model: "5950X".into(),
        microcode: "0xa201016".into(),
        boost_limit_mhz: 5000,
    }
}

fn day_offsets(core_zero: i64) -> PerCore<ObservedOffset> {
    [
        (core(0), ObservedOffset(core_zero)),
        (core(1), ObservedOffset(-5)),
    ]
    .into_iter()
    .collect()
}

pub(crate) fn sample(kind: EventKind) -> Payload {
    samples()
        .into_iter()
        .find(|payload| payload.kind() == kind)
        .unwrap()
}

pub(crate) fn at(
    seq_no: u64,
    boot_name: &str,
    seconds: i64,
    cause: &[u64],
    payload: Payload,
) -> Event {
    let base: Timestamp = "2026-10-02T01:00:00Z".parse().unwrap();
    let time = base + jiff::SignedDuration::from_secs(seconds);
    Event::new(
        seq(seq_no),
        Stamp { time, mono: None },
        BootId::from(boot_name.to_owned()),
        cause.iter().copied().map(seq).collect(),
        payload,
    )
}

pub(crate) fn intent(id: &str, core_id: Option<u8>, loaded: &[u8]) -> Payload {
    let Payload::TrialIntent(mut intent) = sample(EventKind::TrialIntent) else {
        unreachable!()
    };
    intent.trial = TrialId::from(id.to_owned());
    intent.core = core_id.map(core);
    intent.cores = set(loaded);
    intent.into()
}

pub(crate) fn end(id: &str, core_id: Option<u8>) -> Payload {
    let Payload::TrialEnd(mut end) = sample(EventKind::TrialEnd) else {
        unreachable!()
    };
    end.trial = TrialId::from(id.to_owned());
    end.core = core_id.map(core);
    end.into()
}
