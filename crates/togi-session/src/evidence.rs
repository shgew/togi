//! What a trial's evidence amounts to. Pure: no journal, no hardware, no clock.

use std::time::Duration;
use togi_config::DeadEndThreshold;

use togi_journal::{Outcome, Seq, TrialEnd};
use togi_machine::{
    BootId, CcdId, CoreId, CoreSet, CpuId, PerCore, Regime, Signal, TrialConditions, TrialResult,
};
use togi_requests::Telemetry;

/// The waits before the first, second and third retry of a backend or of the kernel log.
pub(crate) const RETRY_WAITS: [Duration; 3] = [
    Duration::from_mins(1),
    Duration::from_mins(5),
    Duration::from_mins(30),
];

/// A retry that is due: its number, how long to wait, and whether `backend.retry` still has to record it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RetryDue {
    pub attempt: u8,
    pub wait: Duration,
    pub record: bool,
}

fn retry(attempt: usize) -> Option<RetryDue> {
    let wait = *RETRY_WAITS.get(attempt.checked_sub(1)?)?;
    Some(RetryDue {
        attempt: u8::try_from(attempt).ok()?,
        wait,
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
    pub tctl_max_c: Option<i64>,
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

/// The outcome a trial's evidence adds up to. The first that holds wins: a worker outside its CPUs, containment
/// that could not be confirmed, a backend signal, a machine check, the reset after a crash, a missing observation
/// or an inconclusive diagnostic, a short run, and only then a pass.
pub(crate) fn adjudicate(evidence: &TrialEvidence<'_>) -> Adjudication {
    let result = evidence.result;
    let missing = present(evidence.missing);
    let mut end = Adjudication {
        outcome: Outcome::Pass,
        signal: None,
        core: None,
        reason: missing.map(str::to_owned),
        escaped: Vec::new(),
        containment_error: evidence.containment.map(str::to_owned),
        duration_s: u32::try_from(result.ran.as_secs()).unwrap_or(u32::MAX),
        tctl_max_c: result.tctl_max_c,
    };
    if !result.escaped.is_empty() {
        end.outcome = Outcome::Inconclusive;
        end.escaped.clone_from(&result.escaped);
        end.reason = join_diagnostic(Some("backend thread outside allowed cpus"), missing);
    } else if evidence.containment.is_some() {
        end.outcome = Outcome::Inconclusive;
    } else if let Some(failure) = result.failure {
        end.outcome = Outcome::Failure;
        end.signal = Some(failure.signal);
        end.core = Some(failure.core);
    } else if !evidence.mces.is_empty() {
        end.outcome = Outcome::Failure;
        end.signal = Some(mce_signal(evidence.mces));
    } else if let Some(reset) = evidence.reset {
        end.outcome = reset.outcome;
        end.signal = reset.signal;
        end.reason = join_diagnostic(reset.reason.as_deref(), missing);
    } else if missing.is_some() || result.inconclusive.is_some() {
        end.outcome = Outcome::Inconclusive;
        end.reason = join_diagnostic(result.inconclusive.as_deref(), missing);
    } else if result.ran < evidence.duration {
        end.outcome = Outcome::Inconclusive;
        end.reason = Some("trial ended before its full duration".to_owned());
    }
    end
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
            valid: loaded.len() >= 2 && regime != Regime::R6,
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
            core_mhz: sample.core_mhz,
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
    let telemetry = if !loaded.is_empty() && loaded.iter().all(|core| ccd_of(core).is_some()) {
        togi_requests::summarize(observed, loaded, |core| ccd_of(core).unwrap_or_default())
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

impl SampleSummary {
    /// Writes what the samples say into a trial's end: the requested voltage and request telemetry of every end, the
    /// stalled worker of a failure, and the last sample of a trial a crash cut short.
    pub(crate) fn fill(&self, end: &mut TrialEnd, crashed: bool) {
        if let Some(voltage) = self.voltage {
            end.voltage_request_median_v = Some(voltage.median_v);
            end.voltage_request_min_v = Some(voltage.min_v);
        }
        if let Some(telemetry) = &self.telemetry {
            end.voltage_requests_v = telemetry.requests;
            end.top_requesters = telemetry.top_requesters;
            end.ccd_mhz = telemetry
                .ccd_mhz
                .iter()
                .filter_map(|(&ccd, &mhz)| Some((ccd, u32::try_from(mhz).ok()?)))
                .collect();
        }
        if end.outcome == Outcome::Failure
            && let Some(stall) = self.stalled
        {
            end.stalled_core = Some(stall.core);
            end.worker_stalled_ms = u64::try_from(stall.since_ms).ok();
        }
        if crashed && let Some(last) = &self.last {
            end.last_sample_s = u32::try_from(last.elapsed_ms / 1000).ok();
            end.last_sample_tctl_c = last.tctl_c.and_then(|tctl| i32::try_from(tctl).ok());
            let mhz = || {
                last.core_mhz
                    .iter()
                    .filter_map(|(_, mhz)| u32::try_from(mhz).ok())
            };
            end.last_sample_min_mhz = mhz().min();
            end.last_sample_max_mhz = mhz().max();
        }
    }
}

#[cfg(test)]
mod tests;
