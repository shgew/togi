use std::fmt::Write as _;

use togi_machine::{
    CoreId, CoreSet, KernelFault, KernelTail, MceCount, PerCore, Signal, TargetOffset, WorkloadId,
};

use super::domain::{
    Activity, Attribution, CorePhase, CycleEvent, DeadEndAction, Decision, HuntOutcome, HuntStage,
    Notice, Outcome, RoundEvent, ShutdownReason, SmuOp, WarningKind,
};
use super::session::{CarriedSource, ResetTarget};
use super::smu::SmuCommand;
use super::tuner::{Member, Rederived};
use super::{
    BackendRetry, BootLeaveReason, BootSavedEntry, CheckingChain, CheckingCycle, CheckingStep,
    Combination, CommandReset, ConfigLoaded, CoreTransition, CrashDetected, DayAnswer,
    DayBootStart, DayCheckpoint, DayOffsetsChanged, DayUncleanEnd, DeadEnd, DeepeningRound,
    DefectAnswered, DefectFound, Failure, FailureCarried, HostRanking, HuntEnd, HuntGroup,
    HuntSkipped, HuntStart, JournalTorn, Mce, Payload, PreflightCheck, ProfileApplied,
    ProfileChange, ProfileRestored, SessionArchived, SessionBaseline, SessionCarried,
    SessionContext, SessionNotice, SessionStart, SessionWarning, Shutdown, SmuError,
    SmuIntentRecord, SmuReadback, SmuWriteRecord, StateRebuilt, TrialCarried, TrialEnd,
    TrialIntent, TrialProgress, TrialSample, TrialSignal, TrialStart, TunerDecision, TunerWarning,
};
use crate::Build;

impl Payload {
    pub fn message(&self) -> String {
        match self {
            Self::SessionStart(p) => session_start(p),
            Self::SessionContext(p) => session_context(p),
            Self::SessionBaseline(p) => session_baseline(p),
            Self::SessionNotice(p) => session_notice(p),
            Self::SessionWarning(p) => session_warning(p),
            Self::SessionArchived(p) => session_archived(p),
            Self::SessionCarried(p) => session_carried(p),
            Self::ConfigLoaded(p) => config_loaded(p),
            Self::PreflightCheck(p) => preflight_check(p),
            Self::SmuIntent(p) => smu_intent(p),
            Self::SmuWrite(p) => smu_write(p),
            Self::SmuReadback(p) => smu_readback(p),
            Self::SmuError(p) => smu_error(p),
            Self::ProfileApplied(p) => profile_applied(p),
            Self::ProfileChange(p) => profile_change(p),
            Self::ProfileRestored(p) => profile_restored(p),
            Self::TrialIntent(p) => trial_intent(p),
            Self::TrialStart(p) => trial_start(p),
            Self::TrialProgress(p) => trial_progress(p),
            Self::TrialSignal(p) => trial_signal(p),
            Self::TrialSample(p) => trial_sample(p),
            Self::TrialEnd(p) => trial_end(p),
            Self::TrialCarried(p) => trial_carried(p),
            Self::Failure(p) => failure(p),
            Self::FailureCarried(p) => failure_carried(p),
            Self::Mce(p) => mce(p),
            Self::CrashDetected(p) => crash_detected(p),
            Self::TunerDecision(p) => tuner_decision(p),
            Self::CorePhase(p) => core_transition(p),
            Self::CheckingCycle(p) => checking_cycle(p),
            Self::CheckingStep(p) => checking_step(p),
            Self::CheckingChain(p) => checking_chain(p),
            Self::HostRanking(p) => host_ranking(p),
            Self::HuntStart(p) => hunt_start(p),
            Self::HuntGroup(p) => hunt_group(p),
            Self::HuntEnd(p) => hunt_end(p),
            Self::HuntSkipped(p) => hunt_skipped(p),
            Self::Combination(p) => combination(p),
            Self::DeepeningRound(p) => deepening_round(p),
            Self::TunerWarning(p) => tuner_warning(p),
            Self::BackendRetry(p) => backend_retry(p),
            Self::CommandReset(p) => command_reset(*p),
            Self::DefectFound(p) => defect_found(p),
            Self::DefectAnswered(p) => defect_answered(p),
            Self::DeadEnd(p) => dead_end(p),
            Self::BootSavedEntry(p) => boot_saved_entry(p),
            Self::BootLeaveReason(p) => boot_leave_reason(p),
            Self::Shutdown(p) => shutdown(p),
            Self::JournalTorn(p) => journal_torn(p),
            Self::StateRebuilt(p) => state_rebuilt(p),
            Self::DayBootStart(p) => day_boot_start(p),
            Self::DayCheckpoint(p) => day_checkpoint(*p),
            Self::DayOffsetsChanged(p) => day_offsets_changed(p),
            Self::DayCleanStop(_) => "day observer stopped on request".to_owned(),
            Self::DayUncleanEnd(p) => day_unclean_end(p),
            Self::DayAnswer(p) => day_answer(p),
        }
    }
}

fn core(core: CoreId) -> String {
    format!("{core:02}")
}

fn cores(set: CoreSet) -> String {
    list(set.iter())
}

fn list(ids: impl IntoIterator<Item = CoreId>) -> String {
    ids.into_iter().map(core).collect::<Vec<_>>().join(", ")
}

fn offsets<T: Copy + Default>(profile: &PerCore<T>, show: impl Fn(T) -> String) -> String {
    let parts: Vec<_> = profile
        .iter()
        .map(|(id, value)| format!("{}: {}", core(id), show(value)))
        .collect();
    format!("[{}]", parts.join(", "))
}

fn profile(profile: &PerCore<TargetOffset>) -> String {
    offsets(profile, |offset| offset.get().to_string())
}

fn members(members: &[Member]) -> String {
    members
        .iter()
        .map(|member| format!("core {} {}", core(member.core), member.offset.get()))
        .collect::<Vec<_>>()
        .join(" + ")
}

fn build_name(build: &Build) -> String {
    build.to_string()
}

fn short_boot(boot: &str) -> &str {
    boot.get(..8).unwrap_or(boot)
}

fn with_reason(mut text: String, reason: Option<&str>) -> String {
    if let Some(reason) = reason.filter(|reason| !reason.is_empty()) {
        text.push_str("; ");
        text.push_str(reason);
    }
    text
}

fn session_start(p: &SessionStart) -> String {
    format!(
        "session {} started by {} (schema {}, ruleset {}, evidence epoch {}, fixes {}, {} cores)",
        p.session,
        build_name(&p.build),
        p.build.schema,
        p.build.ruleset,
        p.build.evidence,
        p.build.fixes,
        p.cores.len()
    )
}

fn session_context(p: &SessionContext) -> String {
    format!(
        "BIOS {} on {}, {}, microcode {}, boost limit {} MHz",
        p.bios_version, p.board, p.cpu_model, p.microcode, p.boost_limit_mhz
    )
}

fn session_baseline(p: &SessionBaseline) -> String {
    format!(
        "baseline profile {}",
        offsets(&p.offsets, |offset| offset.0.to_string())
    )
}

fn session_notice(p: &SessionNotice) -> String {
    match p.notice {
        Notice::NonzeroBaseline => format!(
            "baseline has nonzero offsets on cores {}; BIOS CO 0 is recommended for tuning; starting from the baseline",
            cores(p.cores)
        ),
    }
}

fn session_warning(p: &SessionWarning) -> String {
    format!("{}: {}; continuing the session", p.operation, p.error)
}

fn session_archived(p: &SessionArchived) -> String {
    format!("session {} archived", p.session)
}

fn source_text(source: &CarriedSource) -> String {
    format!(
        "session {} (schema {}, ruleset {})",
        source.session, source.schema, source.ruleset
    )
}

fn session_carried(p: &SessionCarried) -> String {
    let from = p
        .sources
        .iter()
        .map(source_text)
        .collect::<Vec<_>>()
        .join(", ");
    let limits = p
        .carried
        .iter()
        .filter(|carried| carried.candidate_solo_limit.is_some())
        .count();
    let points = p
        .carried
        .iter()
        .filter(|carried| carried.failure_point.is_some())
        .count();
    let limit_noun = if limits == 1 { "limit" } else { "limits" };
    if p.failure_points {
        let point_noun = if points == 1 { "point" } else { "points" };
        format!(
            "carried {limits} candidate solo {limit_noun} and {points} failure {point_noun} from {from}"
        )
    } else {
        let detail = p.detail.as_deref().unwrap_or("no reason recorded");
        format!(
            "carried {limits} candidate solo {limit_noun} from {from}; failure points stay behind: {detail}"
        )
    }
}

fn config_loaded(p: &ConfigLoaded) -> String {
    let build = &p.build;
    let stamp = format!(
        "{} (schema {}, ruleset {}, fixes {})",
        build_name(build),
        build.schema,
        build.ruleset,
        build.fixes
    );
    if p.file {
        format!("config loaded from {} by {stamp}", p.path)
    } else {
        format!("no config at {}; using defaults with {stamp}", p.path)
    }
}

fn preflight_check(p: &PreflightCheck) -> String {
    if p.ok {
        format!("preflight {}: ok ({})", p.check, p.detail)
    } else {
        format!("preflight {}: FAILED ({})", p.check, p.detail)
    }
}

fn command_text(command: SmuCommand) -> String {
    match command {
        SmuCommand::Set { core: id, offset } => {
            format!("set core {} to offset {}", core(id), offset.get())
        }
        SmuCommand::SetAll { offset } => format!("set all cores to offset {}", offset.get()),
    }
}

fn smu_intent(p: &SmuIntentRecord) -> String {
    format!("SMU {}", command_text(p.command()))
}

fn smu_write(p: &SmuWriteRecord) -> String {
    match p.command() {
        SmuCommand::Set { core: id, offset } => {
            format!("SMU wrote core {} CO {}", core(id), offset.get())
        }
        SmuCommand::SetAll { offset } => format!("SMU wrote all cores CO {}", offset.get()),
    }
}

fn smu_readback(p: &SmuReadback) -> String {
    match p.expected {
        Some(expected) if expected.observed() != p.offset => format!(
            "SMU core {} reads CO {}, expected {}",
            core(p.core),
            p.offset.0,
            expected.get()
        ),
        _ => format!("SMU core {} reads CO {}", core(p.core), p.offset.0),
    }
}

fn smu_error(p: &SmuError) -> String {
    let what = match (p.op, p.core, p.offset) {
        (SmuOp::Set, Some(id), Some(offset)) => {
            format!("set core {} to offset {}", core(id), offset.get())
        }
        (SmuOp::SetAll, _, Some(offset)) => format!("set all cores to offset {}", offset.get()),
        (SmuOp::Read, Some(id), _) => format!("read core {}", core(id)),
        (op, _, _) => op.to_string(),
    };
    format!("SMU {what} failed: {}", p.error)
}

fn profile_applied(p: &ProfileApplied) -> String {
    use togi_machine::Condition;
    match p.condition {
        Condition::Alone => "profile applied for alone trials: every core at CO 0".to_owned(),
        Condition::Parked => format!("group profile applied: {}", profile(&p.offsets)),
        Condition::Together => format!("profile applied: {}", profile(&p.offsets)),
    }
}

fn profile_change(p: &ProfileChange) -> String {
    match &p.from {
        None => format!("profile for checking: {}", profile(&p.to)),
        Some(from) if *from == p.to => format!("profile unchanged at {}", profile(&p.to)),
        Some(from) => format!("profile changed {} -> {}", profile(from), profile(&p.to)),
    }
}

fn profile_restored(p: &ProfileRestored) -> String {
    format!(
        "offsets restored before stopping: {} (each core's baseline, or its current offset where that is shallower)",
        profile(&p.offsets)
    )
}

fn workload_label(id: WorkloadId) -> &'static str {
    id.workload().label
}

fn trial_intent(p: &TrialIntent) -> String {
    let mut text = format!("trial {}", p.trial);
    if let Some(id) = p.core {
        let _ = write!(text, " core {}", core(id));
    } else if p.cores.len() == 1 {
        let _ = write!(text, " core {}", cores(p.cores));
    } else if !p.cores.is_empty() {
        let _ = write!(text, " cores {}", cores(p.cores));
    }
    if let Some(offset) = p.offset {
        let _ = write!(text, " offset {}", offset.get());
    }
    let _ = write!(
        text,
        " {} {} {} s {}",
        p.regime,
        workload_label(p.workload),
        p.duration_s,
        p.condition
    );
    if p.retry {
        text.push_str(" (retry)");
    }
    if let Some(cycle) = p.cycle {
        let _ = write!(text, " cycle {cycle}");
    }
    if let Some(step) = p.step {
        let _ = write!(text, " step {step}");
    }
    if let Some(hunt) = p.hunt {
        let _ = write!(text, " hunt {hunt}");
        if let Some(group) = p.group {
            let _ = write!(text, " group {group}");
        }
    }
    if let Some(round) = p.round {
        let _ = write!(text, " round {round}");
    }
    if p.rerun {
        if p.condition == togi_machine::Condition::Parked && p.hunt.is_none() {
            text.push_str(" rerun with every core at offset 0");
        } else {
            text.push_str(" rerun");
        }
    }
    text
}

fn cpus(cpus: &[togi_machine::CpuId]) -> String {
    cpus.iter()
        .map(|cpu| cpu.get().to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn trial_start(p: &TrialStart) -> String {
    if p.instances.len() > 1 {
        return format!(
            "trial {} started {} instances in scopes {}-cNN on cpus {}",
            p.trial,
            p.instances.len(),
            p.scope,
            cpus(&p.cpus)
        );
    }
    let where_ = if p.cpus.len() > 1 { "cpus" } else { "cpu" };
    format!(
        "trial {} started pid {} in scope {} on {where_} {}",
        p.trial,
        p.pid,
        p.scope,
        cpus(&p.cpus)
    )
}

fn trial_progress(p: &TrialProgress) -> String {
    format!("trial {} {}", p.trial, p.detail)
}

fn trial_signal(p: &TrialSignal) -> String {
    match &p.schedule {
        Some(schedule) => format!("trial {} load steps: {schedule}", p.trial),
        None => format!(
            "trial {} load steps: {} stops, {} continues",
            p.trial, p.stops, p.conts
        ),
    }
}

fn trial_sample(p: &TrialSample) -> String {
    format!(
        "trial {} thread {} {} on cpu {}",
        p.trial,
        p.tid,
        p.warning,
        p.cpu.get()
    )
}

fn signal_text(signal: Option<Signal>) -> &'static str {
    signal.map_or("unknown signal", Signal::as_str)
}

fn trial_end(p: &TrialEnd) -> String {
    let mut conditions = String::new();
    if let Some(tctl) = p.tctl_max_c {
        let _ = write!(conditions, " | Tctl max {tctl}°C");
    }
    let mut voltage = String::new();
    if let (Some(median), Some(min)) = (p.voltage_request_median_v, p.voltage_request_min_v) {
        let _ = write!(
            voltage,
            " | loaded voltage request median {median:.3} V, min {min:.3} V"
        );
    }
    if !p.top_requesters.is_empty() {
        let top: Vec<_> = p
            .top_requesters
            .iter()
            .map(|id| match p.voltage_requests_v.get(id) {
                Some(volts) => format!("{} {volts:.3} V", core(id)),
                None => core(id),
            })
            .collect();
        let _ = write!(voltage, " | top requester {}", top.join(", "));
    }
    conditions.push_str(&voltage);
    let mut duration = if p.signal == Some(Signal::Crash) || p.last_evidence {
        format!(", last evidence {} s after start", p.duration_s)
    } else {
        format!(" after {} s", p.duration_s)
    };
    if let Some(sample) = p.last_sample_s {
        let _ = write!(duration, ", last sample {sample} s");
        let mut seen = Vec::new();
        if let Some(tctl) = p.last_sample_tctl_c {
            seen.push(format!("Tctl {tctl}°C"));
        }
        if let (Some(min), Some(max)) = (p.last_sample_min_mhz, p.last_sample_max_mhz) {
            seen.push(format!("{min}-{max} MHz"));
        }
        if !seen.is_empty() {
            let _ = write!(duration, ": {}", seen.join(", "));
        }
    }
    if let (Some(stalled), Some(ms)) = (p.stalled_core, p.worker_stalled_ms) {
        let _ = write!(
            duration,
            ", core {} worker CPU time stopped advancing at {ms} ms after start (evidence only)",
            core(stalled)
        );
    }
    match p.outcome {
        Outcome::Pass => format!("trial {} PASS {} s{conditions}", p.trial, p.duration_s),
        Outcome::Failure => match p.core {
            Some(id) => format!(
                "trial {} FAIL {} on core {}{duration}{conditions}",
                p.trial,
                signal_text(p.signal),
                core(id)
            ),
            None => format!(
                "trial {} FAIL {}{duration}{conditions}",
                p.trial,
                signal_text(p.signal)
            ),
        },
        Outcome::Inconclusive => format!(
            "trial {} INCONCLUSIVE{duration}: {}{voltage}",
            p.trial,
            p.reason.as_deref().unwrap_or("no reason recorded")
        ),
    }
}

fn failure_text(p: &Failure) -> String {
    use togi_machine::Condition;
    if p.known_failure.is_some() {
        return p
            .reason
            .clone()
            .unwrap_or_else(|| "known failure activated".to_owned());
    }
    let loaded = matches!(p.condition, Some(Condition::Together | Condition::Parked));
    let profile_name = if p.condition == Some(Condition::Parked) {
        "group"
    } else {
        "current"
    };
    let signal = p.signal;
    let trial = p.trial.as_ref();
    let in_trial = |trial: &super::TrialId| {
        let mut text = String::new();
        if let Some(condition) = p.condition {
            text.push_str(condition.as_str());
            text.push(' ');
        }
        if let Some(regime) = p.regime {
            let _ = write!(text, "{regime} ");
        }
        format!("in {text}trial {trial}")
    };
    match p.attribution {
        Attribution::Attributed => match (p.core, p.offset) {
            (Some(id), Some(offset)) => {
                let (id, offset) = (core(id), offset.get());
                match (trial, p.condition) {
                    (None, _) if loaded => format!(
                        "core {id} failure at CO {offset}: {signal} with the {profile_name} profile applied and no trial in flight, the only nonzero core"
                    ),
                    (None, _) => {
                        format!("core {id} failure at CO {offset}: {signal} with no trial recorded")
                    }
                    (Some(trial), Some(Condition::Alone)) => format!(
                        "core {id} failure at CO {offset}: {signal} {} (alone: attributed to the target)",
                        in_trial(trial)
                    ),
                    (Some(trial), _) => {
                        format!(
                            "core {id} failure at CO {offset}: {signal} {}",
                            in_trial(trial)
                        )
                    }
                }
            }
            _ => format!("attributed {signal} failure"),
        },
        Attribution::Unattributed => match (trial, p.condition) {
            (Some(trial), Some(Condition::Parked)) => format!(
                "unattributed {signal} failure {}: the group fails, no evidence names a single core",
                in_trial(trial)
            ),
            (Some(trial), _) => format!(
                "unattributed {signal} failure {}: no evidence names a single core",
                in_trial(trial)
            ),
            (None, _) if loaded => {
                let class = p
                    .regime
                    .map_or_else(String::new, |regime| format!("{regime} "));
                format!(
                    "unattributed {signal} failure with the {profile_name} profile applied and no trial in flight: counted as a {class}failure"
                )
            }
            (None, _) => format!("unattributed {signal} failure: no trial was in flight"),
        },
    }
}

fn failure(p: &Failure) -> String {
    failure_text(p)
}

fn trial_carried(p: &TrialCarried) -> String {
    let mut result = match p.outcome {
        Outcome::Failure => match p.signal {
            Some(signal) => format!("failed ({signal})"),
            None => "failed".to_owned(),
        },
        Outcome::Pass => "passed".to_owned(),
        Outcome::Inconclusive => "was inconclusive".to_owned(),
    };
    if !p.top_requesters.is_empty() {
        let _ = write!(result, "; top requester cores {}", cores(p.top_requesters));
    }
    format!(
        "carried {} of trial {} from session {} ({}): {} {} {} on cores {} {result} after {} s at {} (intended {} s, evidence epoch {})",
        p.outcome,
        p.source.trial.as_ref().map_or("-", |trial| trial.as_str()),
        p.source.session,
        build_name(&p.source.build),
        p.condition,
        p.class.regime,
        p.class.workload.map_or("any workload", workload_label),
        cores(p.class.cores),
        p.duration_s,
        profile(&p.profile),
        p.class.duration_s,
        p.source.build.evidence
    )
}

fn failure_carried(p: &FailureCarried) -> String {
    let profile_text = p
        .failure
        .profile
        .as_ref()
        .map_or_else(String::new, |profile_| format!(" at {}", profile(profile_)));
    format!(
        "carried idle failure from session {} ({}), source #{}: {}{profile_text} (evidence epoch {})",
        p.source.session,
        build_name(&p.source.build),
        p.source.seq,
        failure_text(&p.failure),
        p.source.build.evidence
    )
}

fn mce(p: &Mce) -> String {
    let kind = if p.corrected {
        "corrected"
    } else {
        "uncorrected"
    };
    let scope = if p.bank_type.core_local() {
        "core-local"
    } else {
        "shared"
    };
    let owner = p
        .core
        .map_or_else(|| "no core".to_owned(), |id| format!("core {}", core(id)));
    let mut text = format!(
        "{kind} MCE on cpu {} ({owner}), bank {} {} ({scope})",
        p.cpu.get(),
        p.bank,
        p.bank_type
    );
    if let Some(boot) = &p.from_boot {
        let _ = write!(text, " from boot {}", short_boot(boot.as_str()));
    }
    if p.between_trials {
        text.push_str(" between trials (recorded only)");
    }
    text
}

fn crash_detected(p: &CrashDetected) -> String {
    let mut text = format!(
        "crash: boot {} ended without a clean shutdown; ",
        short_boot(p.previous_boot.as_str())
    );
    match p.in_flight {
        Some(seq) => {
            let _ = write!(text, "in flight: seq {seq}");
        }
        None => text.push_str("nothing in flight"),
    }
    if p.stray {
        text.push_str("; stray: the profile was never applied in that boot");
    }
    if p.condition == Some(togi_machine::Condition::Together) {
        text.push_str("; the current profile was applied");
    }
    if let Some(raw) = p.reset_reason_raw.first() {
        let _ = write!(text, "; reset reason: {raw}");
    } else if let Some(kind) = p.reset_reason {
        let _ = write!(text, "; reset reason: {kind}");
    }
    if p.inconclusive {
        text.push_str("; inconclusive: reset does not establish a tuning failure");
    }
    if let Some(pstore) = &p.pstore {
        let _ = write!(text, "; pstore: {}", pstore.path);
    }
    text
}

fn tuner_decision(p: &TunerDecision) -> String {
    let id = core(p.core);
    let from = p.from_offset.get();
    let to = p.to_offset.get();
    let reason = &p.reason;
    match p.decision {
        Decision::StepDeeper => {
            format!("core {id} passed R1+R2 at {from}; searches {to} on its next turn ({reason})")
        }
        Decision::Backoff if p.activity == Activity::Search => {
            format!("core {id} failed at {from}; searches {to} on its next turn ({reason})")
        }
        Decision::CheckSoloLimit if from == to => {
            format!("core {id} checks candidate solo limit {to} ({reason})")
        }
        Decision::CheckSoloLimit => {
            format!("core {id} checks candidate solo limit {to} (from {from}; {reason})")
        }
        Decision::Deepen => format!("core {id} deepened from {from} to {to} ({reason})"),
        Decision::Yield => format!("core {id} yielded from {from} to {to} ({reason})"),
        Decision::Backoff => format!("core {id} backed off from {from} to {to} ({reason})"),
        Decision::Hold => format!("core {id} failure held; tuning stays at {from} ({reason})"),
    }
}

fn phase_text(phase: CorePhase) -> &'static str {
    match phase {
        CorePhase::Search => "search",
        CorePhase::HasRoom => "has room",
        CorePhase::AtLimit => "at its limit",
    }
}

fn core_transition(p: &CoreTransition) -> String {
    match p.from {
        None => format!(
            "core {} starts {} at {} ({})",
            core(p.core),
            phase_text(p.to),
            p.offset.get(),
            p.reason
        ),
        Some(from) => format!(
            "core {} {} -> {} at {} ({})",
            core(p.core),
            phase_text(from),
            phase_text(p.to),
            p.offset.get(),
            p.reason
        ),
    }
}

fn checking_cycle(p: &CheckingCycle) -> String {
    match p.event {
        CycleEvent::Start => {
            let steps: Vec<_> = p.steps.iter().map(ToString::to_string).collect();
            with_reason(
                format!("checking cycle {} start: {}", p.cycle, steps.join(" ")),
                p.reason.as_deref(),
            )
        }
        CycleEvent::End if p.passed => with_reason(
            format!("checking cycle {} end passed", p.cycle),
            p.reason.as_deref(),
        ),
        CycleEvent::End => format!(
            "checking cycle {} end, not passed: {}",
            p.cycle,
            p.reason.as_deref().unwrap_or("no reason recorded")
        ),
    }
}

fn checking_step(p: &CheckingStep) -> String {
    format!(
        "checking cycle {} step {} starts at current profile {}",
        p.cycle,
        p.step,
        profile(&p.profile)
    )
}

fn groups(groups: &[CoreSet]) -> String {
    let parts: Vec<_> = groups
        .iter()
        .map(|group| format!("[{}]", cores(*group)))
        .collect();
    format!("[{}]", parts.join(", "))
}

fn checking_chain(p: &CheckingChain) -> String {
    let source = if p.source_seqs.is_empty() {
        "offset fallback (no request telemetry)".to_owned()
    } else {
        let seqs: Vec<_> = p.source_seqs.iter().map(ToString::to_string).collect();
        format!("request measurements #{}", seqs.join(", #"))
    };
    let mut text = format!(
        "checking cycle {} R7 step {} CCD {} {}: request groups {} from {source}; ",
        p.cycle,
        p.step,
        p.ccd.get(),
        workload_label(p.workload),
        groups(&p.groups)
    );
    if p.cores.is_empty() {
        text.push_str(
            "partial chain ends because idling the next top-requester group leaves fewer than two loaded cores",
        );
    } else {
        let _ = write!(
            text,
            "partial {} loads cores {} at profile {}",
            p.part,
            cores(p.cores),
            profile(&p.profile)
        );
    }
    match p.rederived {
        Some(Rederived::Unchanged) => text.push_str(
            "; re-derived after a profile change, loaded set unchanged, so its earlier passes still count",
        ),
        Some(Rederived::Changed) => text.push_str(
            "; re-derived after a profile change, loaded set changed, so it needs passes of its own",
        ),
        None => {}
    }
    text
}

fn host_ranking(p: &HostRanking) -> String {
    match &p.detail {
        Some(detail) => format!("preferred-core ranking unavailable ({detail}); core-id order"),
        None => format!(
            "preferred cores {}",
            p.ranking
                .iter()
                .map(|id| core(*id))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    }
}

fn hunt_start(p: &HuntStart) -> String {
    let source = match &p.trial {
        Some(trial) => format!("together {} trial {trial}", p.regime),
        None => "idle".to_owned(),
    };
    let parked = if let Some(seq) = p.parked_seq {
        format!("parked offsets from cycle end #{seq}")
    } else if p.parked.iter().any(|(_, offset)| offset.get() != 0) {
        format!(
            "loaded cores {} held at their failing offsets, the rest parked at 0",
            cores(p.cores)
        )
    } else {
        "parked at 0".to_owned()
    };
    with_reason(
        format!(
            "hunt {}: unattributed failure in {source}; {parked}; candidates {}; groups of {} × {} s",
            p.hunt,
            cores(p.candidates),
            p.trials,
            p.trial_s
        ),
        p.reason.as_deref(),
    )
}

fn hunt_group(p: &HuntGroup) -> String {
    let mut prefix = format!(
        "hunt {} group {}: cores {}",
        p.hunt,
        p.group,
        cores(p.cores)
    );
    let mut running = "at failing offsets, the rest parked".to_owned();
    if p.stage == HuntStage::Locate {
        prefix = format!("hunt {} group {}: locate", p.hunt, p.group);
        "with every unloaded core at 0 and the loaded cores at their failing offsets"
            .clone_into(&mut running);
    }
    if let Some(probe) = &p.probe {
        prefix = format!(
            "hunt {} group {}: core {} at {} with {}",
            p.hunt,
            p.group,
            core(probe.core),
            probe.offset.get(),
            members(&p.held)
        );
        "the rest parked".clone_into(&mut running);
    }
    let reason = p.reason.as_deref().unwrap_or("");
    if p.skipped {
        return format!("{prefix} skipped: {reason}");
    }
    if let Some(inferred) = p.inferred {
        return format!("{prefix} {inferred} inferred: {reason}");
    }
    if p.probe.is_some() {
        prefix.push(',');
    }
    with_reason(
        format!("{prefix} {running}; trials of {} s", p.duration_s),
        p.reason.as_deref(),
    )
}

fn plural_groups(count: u32) -> String {
    if count == 1 {
        "1 group".to_owned()
    } else {
        format!("{count} groups")
    }
}

fn hunt_end(p: &HuntEnd) -> String {
    let hunt = p.hunt;
    let groups = plural_groups(p.groups);
    let reason = parenthesized(&p.reason);
    let named = named_cores(p.cores);
    match p.result {
        HuntOutcome::Culprit => format!("hunt {hunt} found {named} after {groups}{reason}"),
        HuntOutcome::Direct => {
            format!("hunt {hunt} ended after {groups}: {named} was attributed directly{reason}")
        }
        HuntOutcome::Loaded => format!(
            "hunt {hunt} ended after {groups}: the failure stays with loaded {named}{reason}"
        ),
        HuntOutcome::Combination => {
            let combination = if p.members.is_empty() {
                format!("{named} at their failing offsets")
            } else {
                members(&p.members)
            };
            format!(
                "hunt {hunt} ended after {groups}: failing offset combination {combination}, with the hunt's other cores parked{reason}"
            )
        }
        HuntOutcome::Fallback => format!(
            "hunt {hunt} ended after {groups} without isolating the failure: falling back to every candidate together, {named}{reason}"
        ),
        HuntOutcome::Cancelled => {
            let mut text = format!("hunt {hunt} cancelled after {groups}; no culprit established");
            if !p.cores.is_empty() {
                let _ = write!(text, ", {named} unresolved");
            }
            text.push_str(&reason);
            text
        }
    }
}

fn named_cores(set: CoreSet) -> String {
    match (set.len(), set.iter().next()) {
        (0, _) => "no recorded core".to_owned(),
        (1, Some(id)) => format!("core {}", core(id)),
        _ => format!("cores {}", cores(set)),
    }
}

fn parenthesized(reason: &str) -> String {
    if reason.is_empty() {
        String::new()
    } else {
        format!(" ({reason})")
    }
}

fn hunt_skipped(p: &HuntSkipped) -> String {
    format!("failure #{} is not hunted: {}", p.failure, p.reason)
}

fn combination(p: &Combination) -> String {
    let suffix = if p.fallback {
        format!("fallback over every candidate of hunt {}", p.hunt)
    } else {
        format!("observed in hunt {}", p.hunt)
    };
    format!(
        "combination C{}: {}, {suffix}",
        p.combination,
        members(&p.members)
    )
}

fn deepening_round(p: &DeepeningRound) -> String {
    match p.event {
        RoundEvent::Start => {
            let who = match (p.cores.len(), p.cores.iter().next()) {
                (1, Some(id)) => format!("core {}", core(id)),
                (count, _) => format!("{count} cores"),
            };
            with_reason(
                format!(
                    "deepening round {} start: {who} toward {}",
                    p.round,
                    profile(&p.target)
                ),
                p.reason.as_deref(),
            )
        }
        RoundEvent::End if p.passed => with_reason(
            format!("deepening round {} end: passed", p.round),
            p.reason.as_deref(),
        ),
        RoundEvent::End => format!(
            "deepening round {} end: {}",
            p.round,
            p.reason.as_deref().unwrap_or("not passed")
        ),
    }
}

fn tuner_warning(p: &TunerWarning) -> String {
    let WarningKind::Monotonicity = p.warning;
    if p.detail.is_empty() {
        match &p.trial {
            Some(trial) => format!(
                "{}: trial {trial} failed on a profile at least as shallow as {} passes in its class",
                p.warning,
                p.passes.len()
            ),
            None => format!(
                "{}: idle failure on a profile at least as shallow as {} passes in an all-core R6 class",
                p.warning,
                p.passes.len()
            ),
        }
    } else {
        format!("{}: {}", p.warning, p.detail)
    }
}

fn backend_retry(p: &BackendRetry) -> String {
    format!(
        "backend {}: retry {} of 3 after {} s: {}",
        p.backend, p.attempt, p.wait_s, p.reason
    )
}

fn command_reset(p: CommandReset) -> String {
    match p.target {
        ResetTarget::Core(id) => format!("reset core {}", core(id)),
        ResetTarget::All => "reset all: new session on next run".to_owned(),
    }
}

fn defect_found(p: &DefectFound) -> String {
    let decisions: Vec<_> = p.decisions.iter().map(|seq| format!("#{seq}")).collect();
    let text = format!(
        "defect {} ({}, fixed in pull request #{}): {} decisions {} affected cores {}",
        p.id,
        p.title,
        p.pr,
        p.direction,
        decisions.join(" "),
        cores(p.cores)
    );
    with_reason(text, Some(&p.detail))
}

fn defect_answered(p: &DefectAnswered) -> String {
    format!(
        "defect {}: answered {} to reset cores {}",
        p.id,
        p.answer,
        cores(p.cores)
    )
}

fn dead_end(p: &DeadEnd) -> String {
    let mut text = format!("dead end {}: {}", p.condition, p.detail);
    match p.action {
        Some(DeadEndAction::ClearSavedEntry) => text.push_str("; clearing GRUB's saved entry"),
        Some(DeadEndAction::ClearSavedEntryAndReboot) => {
            text.push_str("; clearing GRUB's saved entry and rebooting");
        }
        Some(DeadEndAction::Exit) | None => {}
    }
    text
}

fn boot_saved_entry(p: &BootSavedEntry) -> String {
    let mut text = match (&p.error, p.before.as_deref(), p.after.as_deref()) {
        (Some(error), _, _) => format!("GRUB saved entry not cleared: {error}"),
        (None, None, None) => "GRUB saved entry was already unset".to_owned(),
        (None, Some(before), None) => {
            format!("GRUB saved entry {before} cleared; the next boot selects the first menu entry")
        }
        (None, before, Some(after)) => {
            format!("GRUB saved entry {} -> {after}", before.unwrap_or("unset"))
        }
    };
    if let Some(error) = &p.reason_error {
        let _ = write!(text, "; leave reason not saved: {error}");
    }
    text
}

fn boot_leave_reason(p: &BootLeaveReason) -> String {
    format!(
        "previous tuning boot ended: {} (consecutive restart-limit boots: {})",
        p.reason, p.restart_limit_count
    )
}

fn shutdown(p: &Shutdown) -> String {
    match (p.reason, p.cycles.map(std::num::NonZeroU32::get)) {
        (ShutdownReason::Signal, _) => "stopped by signal".to_owned(),
        (ShutdownReason::DeadEnd, _) => "stopped at a dead end".to_owned(),
        (ShutdownReason::Cycles, Some(1)) => {
            "phase 2 is confirmed and the profile passed the requested clean cycle; stopping"
                .to_owned()
        }
        (ShutdownReason::Cycles, Some(count)) => format!(
            "phase 2 is confirmed and the profile passed the requested {count} clean cycles; stopping"
        ),
        (ShutdownReason::Cycles, None) => {
            "phase 2 is confirmed and the profile passed the requested clean cycles; stopping"
                .to_owned()
        }
        (ShutdownReason::FirstResult, _) => {
            "the BIOS profile is confirmed; stopping at the first result as requested".to_owned()
        }
        (ShutdownReason::Command, _) => "command finished".to_owned(),
    }
}

fn journal_torn(p: &JournalTorn) -> String {
    format!(
        "journal had {} torn bytes at offset {}; discarded",
        p.bytes_hex.len() / 2,
        p.offset
    )
}

fn state_rebuilt(p: &StateRebuilt) -> String {
    format!(
        "state.json rebuilt from the journal; differed in {}",
        p.fields.join(", ")
    )
}

fn day_boot_start(p: &DayBootStart) -> String {
    format!(
        "day observer started by {}: BIOS {} on {}, {}, microcode {}, boost limit {} MHz; offsets {}",
        build_name(&p.build),
        p.bios.bios_version,
        p.bios.board,
        p.bios.cpu_model,
        p.bios.microcode,
        p.bios.boost_limit_mhz,
        offsets(&p.offsets, |offset| offset.0.to_string())
    )
}

fn day_checkpoint(p: DayCheckpoint) -> String {
    format!(
        "day observer has been active {} s in total this boot",
        p.applied_secs
    )
}

fn day_offsets_changed(p: &DayOffsetsChanged) -> String {
    format!(
        "offsets read back changed to {}; the time since the previous record is not credited to any offset profile",
        offsets(&p.offsets, |offset| offset.0.to_string())
    )
}

pub(super) fn day_unclean_end(p: &DayUncleanEnd) -> String {
    let mut text = format!(
        "day observer of boot {} ended without a clean stop; ",
        short_boot(p.boot.as_str())
    );
    match p.last_checkpoint {
        Some(seq) => {
            let _ = write!(text, "last checkpoint seq {seq}");
        }
        None => text.push_str("it wrote no checkpoint"),
    }
    let _ = write!(
        text,
        "; last offsets {}",
        offsets(&p.offsets, |offset| offset.0.to_string())
    );
    if let Some(raw) = p.reset_reason.raw.first() {
        let _ = write!(text, "; reset reason: {raw}");
    } else if let Some(kind) = p.reset_reason.kind {
        let _ = write!(text, "; reset reason: {kind}");
    } else if !p.reset_reason.supported {
        text.push_str("; reset reason unavailable on this kernel");
    }
    let _ = write!(
        text,
        "; machine checks: {} in that boot, {} in the next; kernel log tail: {}",
        mce_count(p.evidence.crashed_mces),
        mce_count(p.evidence.successor_mces),
        kernel_tail(&p.evidence.tail)
    );
    text
}

fn mce_count(count: MceCount) -> String {
    match count {
        MceCount::Unknown {} => "unknown".to_owned(),
        MceCount::Read {
            corrected: 0,
            uncorrected: 0,
        } => "none".to_owned(),
        MceCount::Read {
            corrected,
            uncorrected,
        } => format!("{uncorrected} uncorrected and {corrected} corrected"),
    }
}

fn kernel_fault(out: &mut String, fault: KernelFault) {
    match fault {
        KernelFault::MachineCheck {} => out.push_str("machine check"),
        KernelFault::GraphicsTimeout { driver } => {
            let _ = write!(out, "{} timeout", driver.as_str());
        }
        KernelFault::GraphicsReset { driver } => {
            let _ = write!(out, "{} reset", driver.as_str());
        }
        KernelFault::NvidiaXid { code } => {
            let _ = write!(out, "NVIDIA Xid {code}");
        }
        KernelFault::Oops {} => out.push_str("kernel oops"),
        KernelFault::HungTask {} => out.push_str("hung task"),
    }
}

fn kernel_tail(tail: &KernelTail) -> String {
    match tail {
        KernelTail::Missing {} => "missing".to_owned(),
        KernelTail::Unreadable {} => "unreadable".to_owned(),
        KernelTail::Quiet {} => "no recognized errors".to_owned(),
        KernelTail::Faults {
            faults,
            logging_continued,
        } => {
            let mut text = String::new();
            for (index, fault) in faults.iter().enumerate() {
                if index > 0 {
                    text.push_str(", ");
                }
                kernel_fault(&mut text, *fault);
            }
            text.push_str(if *logging_continued {
                "; logging continued after the last error"
            } else {
                "; the log ended at the last error"
            });
            text
        }
    }
}

fn day_answer(p: &DayAnswer) -> String {
    format!(
        "uid {} answered that boot {} ended because {}; offsets {}",
        p.uid,
        short_boot(p.boot.as_str()),
        p.answer.meaning(),
        offsets(&p.offsets, |offset| offset.0.to_string())
    )
}
