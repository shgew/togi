//! What a trial's evidence amounts to. Pure: no journal, no hardware, no clock.

use std::time::Duration;
use togi_config::DeadEndThreshold;

use togi_journal::{Outcome, Seq, TrialEnd, TrialId};
use togi_machine::{
    BootId, CcdId, CoreId, CoreSet, CpuId, PerCore, Regime, Signal, TrialConditions, TrialResult,
};
use togi_requests::Telemetry;

/// The waits before the first, second and third retry of a backend or of the kernel log.
pub(crate) const RETRY_WAITS: [u32; 3] = [60, 300, 1800];

/// A retry that is due: its number, how long to wait, and whether `backend.retry` still has to record it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RetryDue {
    pub attempt: u8,
    pub wait: Duration,
    pub wait_s: u32,
    pub record: bool,
}

fn retry(attempt: usize) -> Option<RetryDue> {
    let wait_s = *RETRY_WAITS.get(attempt.checked_sub(1)?)?;
    Some(RetryDue {
        attempt: u8::try_from(attempt).ok()?,
        wait: Duration::from_secs(u64::from(wait_s)),
        wait_s,
        record: true,
    })
}

/// The retry a backend is due after `streak` inconclusive trials in a row: none until `threshold` of them, then
/// three waits, one per further inconclusive trial.
pub(crate) fn backend_retry(streak: usize, threshold: DeadEndThreshold) -> Option<RetryDue> {
    let threshold = usize::try_from(threshold.get()).ok()?;
    retry(streak.checked_sub(threshold)? + 1)
}

/// Whether the streak outlasted the threshold and all three retries: no evidence can be had from the backend.
pub(crate) fn backend_exhausted(streak: usize, threshold: DeadEndThreshold) -> bool {
    usize::try_from(threshold.get()).is_ok_and(|threshold| streak >= threshold + RETRY_WAITS.len())
}

/// The kernel-log retry that follows `recorded` retries since the last crash was classified.
pub(crate) fn kernel_retry(recorded: usize) -> Option<RetryDue> {
    retry(recorded + 1)
}

/// The monotonic span a trial's machine checks fall in, both ends inclusive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Window {
    pub since: Duration,
    pub until: Duration,
}

impl Window {
    pub(crate) fn contains(self, mono: Duration) -> bool {
        self.since <= mono && mono <= self.until
    }
}

/// Where the journal files a machine check the kernel log yielded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Placement {
    /// Evidence of the trial whose window it is in.
    Trial,
    /// An uncorrected one a crashed boot's recovery found in the current boot's log: it belongs to that crash.
    FromBoot,
    /// Outside every trial: recorded, never decisive.
    BetweenTrials,
}

pub(crate) fn place_kernel_mce(
    mono: Duration,
    corrected: bool,
    trial: Option<Window>,
    recovering: bool,
    crashed_boots: bool,
) -> Placement {
    if trial.is_some_and(|window| window.contains(mono)) {
        Placement::Trial
    } else if recovering && !corrected && crashed_boots {
        Placement::FromBoot
    } else {
        Placement::BetweenTrials
    }
}

/// Whether a machine check at `mono` of a trial's boot is inside the trial's window; one without a time is.
pub(crate) fn mce_in_window(mono: Option<Duration>, since: Duration) -> bool {
    mono.is_none_or(|mono| mono >= since)
}

/// Whether the crash of `boot` owns a machine check recorded from `from_boot`: the boot's own, and the uncorrected
/// ones its successor's kernel retained in its banks. A corrected one of the successor is the successor's.
pub(crate) fn crash_owns(
    from_boot: &BootId,
    corrected: bool,
    boot: &BootId,
    next: &BootId,
) -> bool {
    from_boot == boot || (from_boot == next && !corrected)
}

pub(crate) fn join_diagnostic(reason: Option<&str>, diagnostic: Option<&str>) -> Option<String> {
    match (present(reason), present(diagnostic)) {
        (Some(reason), Some(diagnostic)) => Some(format!("{reason}; {diagnostic}")),
        (Some(text), None) | (None, Some(text)) => Some(text.to_owned()),
        (None, None) => None,
    }
}

fn present(text: Option<&str>) -> Option<&str> {
    text.filter(|text| !text.is_empty())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RecordedMce {
    pub seq: Seq,
    pub corrected: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct FailureSignal {
    pub signal: Signal,
    pub core: Option<CoreId>,
    pub pid: Option<u32>,
}

impl From<togi_machine::TrialFailure> for FailureSignal {
    fn from(failure: togi_machine::TrialFailure) -> Self {
        Self {
            signal: failure.signal,
            core: Some(failure.core),
            pid: failure.pid.map(std::num::NonZeroU32::get),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RecordedProcessFault {
    pub seq: Seq,
    pub pid: u32,
    pub kind: togi_machine::ProcessFaultKind,
    pub monotonic_ns: u64,
    pub process_pid: u32,
    pub cpu: Option<CpuId>,
    pub core: Option<CoreId>,
    pub worker_core: Option<CoreId>,
}

impl RecordedProcessFault {
    pub(crate) const fn new(seq: Seq, fault: &togi_journal::ProcessFaultEvidence) -> Self {
        Self {
            seq,
            pid: fault.pid,
            kind: fault.kind,
            monotonic_ns: fault.monotonic_ns,
            process_pid: fault.process_pid,
            cpu: fault.cpu,
            core: fault.core,
            worker_core: fault.worker_core,
        }
    }
}

/// What the reset after a crashed trial says of it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResetVerdict {
    pub outcome: Outcome,
    pub signal: Option<Signal>,
    pub reason: Option<String>,
}

pub(crate) struct TrialEvidence<'a> {
    pub result: &'a TrialResult,
    pub mces: &'a [RecordedMce],
    pub reset: Option<&'a ResetVerdict>,
    /// A diagnostic that something that could have been evidence was not observed.
    pub missing: Option<&'a str>,
    pub duration: Duration,
    pub containment: Option<&'a str>,
    /// The kernel's process faults of a trial whose controller was recorded; they place or withdraw the backend's
    /// failure.
    pub process: Option<ProcessFaults<'a>>,
}

/// The process faults a trial's kernel log recorded and the CPUs the trial tested.
#[derive(Clone, Copy)]
pub(crate) struct ProcessFaults<'a> {
    pub recorded: &'a [RecordedProcessFault],
    pub tested: &'a [CpuId],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Adjudication {
    pub outcome: Outcome,
    pub signal: Option<Signal>,
    pub core: Option<CoreId>,
    pub reason: Option<String>,
    pub escaped: Vec<CpuId>,
    pub containment_error: Option<String>,
    pub duration_s: u32,
    pub tctl_max_c: Option<i32>,
}

pub(crate) fn mce_signal(mces: &[RecordedMce]) -> Signal {
    machine_check_signal(mces.iter().any(|mce| mce.corrected))
}

pub(crate) const fn machine_check_signal(any_corrected: bool) -> Signal {
    if any_corrected {
        Signal::CorrectedMce
    } else {
        Signal::UncorrectedMce
    }
}

pub(crate) fn process_faults_failure(
    previous: Option<FailureSignal>,
    faults: &[RecordedProcessFault],
    tested: &[CpuId],
) -> Option<FailureSignal> {
    // A later coredump summary carries no execution CPU. It must not erase the trap's
    // location, nor turn an observed out-of-test fault into an unlocated tested failure.
    let located = |fault: &RecordedProcessFault| {
        fault.cpu.is_some()
            || !faults
                .iter()
                .any(|other| other.process_pid == fault.process_pid && other.cpu.is_some())
    };
    let outside =
        |fault: &RecordedProcessFault| fault.cpu.is_some_and(|cpu| !tested.contains(&cpu));
    // An out-of-test execution withdraws only the exit of the worker that ran there, so the backend's
    // evidence is resolved against all of them before another process's tested fault is considered.
    let mut failure = previous;
    for fault in faults
        .iter()
        .filter(|fault| located(fault) && outside(fault))
    {
        failure = failure.filter(|failure| !withdraws(fault, failure));
    }
    for (index, fault) in faults.iter().enumerate() {
        if !located(fault) || outside(fault) {
            continue;
        }
        // The process's own later out-of-test execution withdraws its tested one.
        let withdrawn = faults[index + 1..].iter().any(|later| {
            later.process_pid == fault.process_pid && located(later) && outside(later)
        });
        if !withdrawn {
            failure = Some(adopt_fault(failure, fault));
        }
    }
    failure
}

fn withdraws(fault: &RecordedProcessFault, failure: &FailureSignal) -> bool {
    let same_process = match failure.pid {
        Some(pid) => pid == fault.process_pid,
        None => fault.worker_core.is_some() && fault.worker_core == failure.core,
    };
    failure.signal == Signal::UnexpectedExit && same_process
}

/// A kernel location replaces the assigned worker location for an unexpected exit, never independent evidence.
fn adopt_fault(previous: Option<FailureSignal>, fault: &RecordedProcessFault) -> FailureSignal {
    if let Some(previous) = previous
        && (previous.signal != Signal::UnexpectedExit
            || previous.pid.is_some_and(|pid| pid != fault.process_pid))
    {
        return previous;
    }
    FailureSignal {
        signal: Signal::UnexpectedExit,
        core: fault.core,
        pid: Some(fault.process_pid),
    }
}

/// What each kind of evidence a trial left says, weighed by [`Ladder::decide`]. Live trials and recovered ones fill
/// the rungs they can know and leave the rest unset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Ladder<'a> {
    /// The CPUs outside its allowed ones a worker ran on.
    pub escaped: &'a [CpuId],
    /// Why containment of the trial's workload could not be confirmed.
    pub uncontained: Option<&'a str>,
    /// The backend's failure, as the kernel's process faults left it; the core is not always known.
    pub failure: Option<FailureSignal>,
    /// The signal of the machine checks of the trial, when it had any.
    pub machine_checks: Option<Signal>,
    pub reset: Option<ResetRuling>,
    /// An observation that could have been evidence is missing, or the backend reported an inconclusive diagnostic.
    pub inconclusive: bool,
    /// The trial did not run its full duration.
    pub short: bool,
}

/// What the reset after a crashed trial rules, without its wording.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResetRuling {
    pub outcome: Outcome,
    pub signal: Option<Signal>,
}

/// The evidence that decided a trial. The ladder's order is the declaration order: a worker outside its CPUs,
/// containment that could not be confirmed, a backend failure, a machine check, the reset after a crash, a missing
/// observation or an inconclusive diagnostic, a short run, and only then a pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Verdict {
    Escaped,
    Uncontained,
    Failure(FailureSignal),
    MachineChecks(Signal),
    Reset(ResetRuling),
    Inconclusive,
    Short,
    Passed,
}

impl Verdict {
    pub(crate) const fn outcome(self) -> Outcome {
        match self {
            Self::Escaped | Self::Uncontained | Self::Inconclusive | Self::Short => {
                Outcome::Inconclusive
            }
            Self::Failure(_) | Self::MachineChecks(_) => Outcome::Failure,
            Self::Reset(reset) => reset.outcome,
            Self::Passed => Outcome::Pass,
        }
    }

    pub(crate) const fn signal(self) -> Option<Signal> {
        match self {
            Self::Failure(failure) => Some(failure.signal),
            Self::MachineChecks(signal) => Some(signal),
            Self::Reset(reset) => reset.signal,
            Self::Escaped | Self::Uncontained | Self::Inconclusive | Self::Short | Self::Passed => {
                None
            }
        }
    }

    pub(crate) const fn core(self) -> Option<CoreId> {
        match self {
            Self::Failure(failure) => failure.core,
            Self::MachineChecks(_)
            | Self::Reset(_)
            | Self::Escaped
            | Self::Uncontained
            | Self::Inconclusive
            | Self::Short
            | Self::Passed => None,
        }
    }
}

impl Ladder<'_> {
    /// The first rung that holds decides.
    pub(crate) const fn decide(&self) -> Verdict {
        if !self.escaped.is_empty() {
            Verdict::Escaped
        } else if self.uncontained.is_some() {
            Verdict::Uncontained
        } else if let Some(failure) = self.failure {
            Verdict::Failure(failure)
        } else if let Some(signal) = self.machine_checks {
            Verdict::MachineChecks(signal)
        } else if let Some(reset) = self.reset {
            Verdict::Reset(reset)
        } else if self.inconclusive {
            Verdict::Inconclusive
        } else if self.short {
            Verdict::Short
        } else {
            Verdict::Passed
        }
    }
}

/// The outcome a trial's evidence adds up to, by the order of [`Ladder`].
pub(crate) fn adjudicate(
    evidence: &TrialEvidence<'_>,
) -> Result<Adjudication, crate::SessionError> {
    let result = evidence.result;
    let missing = present(evidence.missing);
    let reported = result.failure.map(FailureSignal::from);
    let failure = match evidence.process {
        Some(process) => process_faults_failure(reported, process.recorded, process.tested),
        None => reported,
    };
    // The kernel placed the only failure the backend reported outside the tested CPUs.
    let inconclusive = if reported.is_some() && failure.is_none() {
        Some("worker fault executed outside tested CPUs")
    } else {
        result.inconclusive.as_deref()
    };
    let ladder = Ladder {
        escaped: &result.escaped,
        uncontained: evidence.containment,
        failure,
        machine_checks: (!evidence.mces.is_empty()).then(|| mce_signal(evidence.mces)),
        reset: evidence.reset.map(|reset| ResetRuling {
            outcome: reset.outcome,
            signal: reset.signal,
        }),
        inconclusive: missing.is_some() || inconclusive.is_some(),
        short: result.ran < evidence.duration,
    };
    let verdict = ladder.decide();
    // A failure whose core the kernel did not place still ends the trial, and words it as the evidence below it would.
    let worded = if let Verdict::Failure(FailureSignal { core: None, .. }) = verdict {
        Ladder {
            failure: None,
            ..ladder
        }
        .decide()
    } else {
        verdict
    };
    let reason = match worded {
        Verdict::Escaped => join_diagnostic(Some("backend thread outside allowed cpus"), missing),
        Verdict::Reset(_) => join_diagnostic(
            evidence.reset.and_then(|reset| reset.reason.as_deref()),
            missing,
        ),
        Verdict::Inconclusive => join_diagnostic(inconclusive, missing),
        Verdict::Short => Some("trial ended before its full duration".to_owned()),
        Verdict::Uncontained
        | Verdict::Failure(_)
        | Verdict::MachineChecks(_)
        | Verdict::Passed => missing.map(str::to_owned),
    };
    Ok(Adjudication {
        outcome: verdict.outcome(),
        signal: verdict.signal(),
        core: verdict.core(),
        reason,
        escaped: result.escaped.clone(),
        containment_error: evidence.containment.map(str::to_owned),
        duration_s: u32::try_from(result.ran.as_secs())
            .map_err(crate::SessionError::TrialDuration)?,
        tctl_max_c: result
            .tctl_max_c
            .map(i32::try_from)
            .transpose()
            .map_err(crate::SessionError::TrialTemperature)?,
    })
}

/// The last sample of a trial, as much of it as a crash recovery or a trial end reports.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LastSample {
    pub elapsed_ms: i64,
    pub tctl_c: Option<i64>,
    pub core_mhz: PerCore<i64>,
}

/// A loaded core whose worker stopped advancing, and the elapsed time of the first sample that showed it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Stall {
    pub core: CoreId,
    pub since_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Voltage {
    pub median_v: f64,
    pub min_v: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SampleSummary {
    pub last: Option<LastSample>,
    pub stalled: Option<Stall>,
    pub voltage: Option<Voltage>,
    pub telemetry: Option<Telemetry>,
}

#[derive(Clone, Copy)]
struct Worker {
    cpu: i64,
    stall: Option<i64>,
}

struct Observer {
    loaded: Vec<CoreId>,
    workers: Vec<Worker>,
    last: Option<LastSample>,
    valid: bool,
    count: usize,
    maxima: Vec<f64>,
}

impl Observer {
    fn new(loaded: CoreSet, regime: Regime) -> Self {
        let loaded: Vec<CoreId> = loaded.iter().collect();
        Self {
            workers: vec![
                Worker {
                    cpu: 0,
                    stall: None
                };
                loaded.len()
            ],
            valid: loaded.len() >= 2 && regime != Regime::R6 && !regime.has_roles(),
            loaded,
            last: None,
            count: 0,
            maxima: Vec::new(),
        }
    }

    fn observe(&mut self, sample: &TrialConditions) {
        self.request_voltage(sample);
        let after = self.last.map(|last| last.elapsed_ms);
        for (worker, &core) in self.workers.iter_mut().zip(&self.loaded) {
            let reading = sample.worker_cpu_ms.get(core);
            let cpu = reading.unwrap_or(0);
            let regressed =
                after.is_some_and(|after| cpu < worker.cpu || sample.elapsed_ms <= after);
            if reading.is_none() || cpu < 0 || regressed {
                self.valid = false;
            }
            if after.is_some() && cpu == worker.cpu {
                worker.stall.get_or_insert(sample.elapsed_ms);
            } else {
                worker.stall = None;
            }
            worker.cpu = cpu;
        }
        self.last = Some(LastSample {
            elapsed_ms: sample.elapsed_ms,
            tctl_c: sample.tctl_c,
            core_mhz: self
                .loaded
                .iter()
                .filter_map(|&core| sample.core_mhz.get(core).map(|mhz| (core, mhz)))
                .collect(),
        });
        self.count += 1;
    }

    fn request_voltage(&mut self, sample: &TrialConditions) {
        let Some(table) = &sample.pm_table else {
            return;
        };
        if self.loaded.is_empty() {
            return;
        }
        let mut highest = f32::NEG_INFINITY;
        for core in &self.loaded {
            let request = table.voltage_request_v[core.index()];
            if !request.is_finite() {
                return;
            }
            highest = highest.max(request);
        }
        self.maxima.push(f64::from(highest));
    }

    fn voltage(&mut self) -> Option<Voltage> {
        self.maxima.sort_by(f64::total_cmp);
        let min_v = *self.maxima.first()?;
        let middle = self.maxima.len() / 2;
        let median_v = if self.maxima.len().is_multiple_of(2) {
            self.maxima[middle - 1].midpoint(self.maxima[middle])
        } else {
            self.maxima[middle]
        };
        Some(Voltage { median_v, min_v })
    }

    /// The loaded core that stalled first, if one did uniquely and every reading was valid.
    fn stalled(&self) -> Option<Stall> {
        if !self.valid || self.count < 2 {
            return None;
        }
        let mut first: Option<Stall> = None;
        let mut tie = false;
        for (worker, &core) in self.workers.iter().zip(&self.loaded) {
            let Some(since_ms) = worker.stall else {
                continue;
            };
            match first {
                Some(held) if since_ms == held.since_ms => tie = true,
                Some(held) if since_ms > held.since_ms => {}
                _ => {
                    first = Some(Stall { core, since_ms });
                    tie = false;
                }
            }
        }
        first.filter(|_| !tie)
    }
}

/// Reads a trial's samples once: the last one, which loaded core's worker stopped first (a unique one, over valid
/// readings of two or more cores outside R6, where the workers are suspended on purpose), the loaded cores'
/// requested voltage, and the request telemetry, which needs every loaded core on a known CCD.
/// The cores a trial's intent loads: the cores it lists and the one core it names.
pub(crate) fn loaded_cores(intent: &togi_journal::TrialIntent) -> CoreSet {
    intent.cores.iter().chain(intent.core).collect()
}

pub(crate) fn summarize_samples(
    samples: impl IntoIterator<Item = Result<TrialConditions, togi_machine::SampleError>>,
    loaded: CoreSet,
    regime: Regime,
    ccd_of: impl Fn(CoreId) -> Option<CcdId>,
) -> Result<SampleSummary, togi_machine::SampleError> {
    let mut observer = Observer::new(loaded, regime);
    let mut error = None;
    let observed = samples
        .into_iter()
        .map_while(|sample| match sample {
            Ok(sample) => Some(sample),
            Err(failure) => {
                error = Some(failure);
                None
            }
        })
        .inspect(|sample| observer.observe(sample));
    let placed: Option<PerCore<Option<CcdId>>> = loaded
        .iter()
        .map(|core| ccd_of(core).map(|ccd| (core, Some(ccd))))
        .collect();
    let telemetry = if let Some(placed) = placed.filter(|placed| !placed.is_empty()) {
        togi_requests::summarize(
            observed.filter(|sample| {
                regime.warm_up().is_none_or(|warm_up| {
                    sample.elapsed_ms >= 0
                        && Duration::from_millis(sample.elapsed_ms.unsigned_abs()) >= warm_up
                })
            }),
            &placed,
        )
    } else {
        observed.for_each(drop);
        None
    };
    match error {
        Some(error) => Err(error),
        None => Ok(SampleSummary {
            stalled: observer.stalled(),
            voltage: observer.voltage(),
            last: observer.last,
            telemetry,
        }),
    }
}

/// The end of `trial` with `outcome` after `duration_s` and nothing else recorded: the run or the recovery that ends
/// the trial sets what it adjudged on top, and [`SampleSummary::fill`] writes the fields the samples give.
pub(crate) fn trial_end(trial: TrialId, outcome: Outcome, duration_s: u32) -> TrialEnd {
    TrialEnd {
        trial,
        kernel_cursor: None,
        kernel_error: None,
        excluded: Vec::new(),
        outcome,
        signal: None,
        core: None,
        duration_s,
        tctl_max_c: None,
        voltage_request_median_v: None,
        voltage_request_min_v: None,
        voltage_requests_v: PerCore::default(),
        top_requesters: CoreSet::default(),
        ccd_mhz: std::collections::BTreeMap::new(),
        last_sample_s: None,
        last_sample_tctl_c: None,
        last_sample_min_mhz: None,
        last_sample_max_mhz: None,
        stalled_core: None,
        worker_stalled_ms: None,
        reason: None,
        interrupted: false,
        last_evidence: false,
        escaped: Vec::new(),
        backend_missing: false,
        containment_error: None,
    }
}

impl SampleSummary {
    /// Writes what the samples say into a trial's end: the requested voltage and request telemetry of every end, the
    /// stalled worker of a failure, and the last sample of a trial a crash cut short.
    pub(crate) fn fill(
        &self,
        end: &mut TrialEnd,
        crashed: bool,
    ) -> Result<(), togi_machine::SampleError> {
        let stalled_ms = self
            .stalled
            .filter(|_| end.outcome == Outcome::Failure)
            .map(|stall| u64::try_from(stall.since_ms))
            .transpose()
            .map_err(togi_machine::SampleError::ElapsedMillis)?;
        let last_s = self
            .last
            .filter(|_| crashed)
            .map(|last| {
                let elapsed = u64::try_from(last.elapsed_ms)
                    .map_err(togi_machine::SampleError::ElapsedMillis)?;
                u32::try_from(elapsed / 1000).map_err(togi_machine::SampleError::ElapsedSeconds)
            })
            .transpose()?;
        let ccd_mhz = self
            .telemetry
            .as_ref()
            .map(|telemetry| {
                telemetry
                    .ccd_mhz
                    .iter()
                    .map(|(&ccd, &mhz)| {
                        u32::try_from(mhz)
                            .map(|mhz| (ccd, mhz))
                            .map_err(togi_machine::SampleError::Clock)
                    })
                    .collect::<Result<_, _>>()
            })
            .transpose()?;
        let last = self
            .last
            .as_ref()
            .filter(|_| crashed)
            .map(|last| {
                let temperature = last
                    .tctl_c
                    .map(i32::try_from)
                    .transpose()
                    .map_err(togi_machine::SampleError::Temperature)?;
                let (min_mhz, max_mhz) = last.core_mhz.iter().try_fold(
                    (None::<u32>, None::<u32>),
                    |(min, max), (_, mhz)| {
                        let mhz = u32::try_from(mhz).map_err(togi_machine::SampleError::Clock)?;
                        Ok::<_, togi_machine::SampleError>((
                            Some(min.map_or(mhz, |min| min.min(mhz))),
                            Some(max.map_or(mhz, |max| max.max(mhz))),
                        ))
                    },
                )?;
                Ok::<_, togi_machine::SampleError>((temperature, min_mhz, max_mhz))
            })
            .transpose()?;
        if let Some(voltage) = self.voltage {
            end.voltage_request_median_v = Some(voltage.median_v);
            end.voltage_request_min_v = Some(voltage.min_v);
        }
        if let Some(telemetry) = &self.telemetry {
            end.voltage_requests_v = telemetry.requests;
            end.top_requesters = telemetry.top_requesters;
            if let Some(ccd_mhz) = ccd_mhz {
                end.ccd_mhz = ccd_mhz;
            }
        }
        if end.outcome == Outcome::Failure
            && let Some(stall) = self.stalled
        {
            end.stalled_core = Some(stall.core);
            end.worker_stalled_ms = stalled_ms;
        }
        if let Some((temperature, min_mhz, max_mhz)) = last {
            end.last_sample_s = last_s;
            end.last_sample_tctl_c = temperature;
            end.last_sample_min_mhz = min_mhz;
            end.last_sample_max_mhz = max_mhz;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
