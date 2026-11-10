use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use test_case::test_case;

use togi_journal::{
    MemFs, Outcome, Payload, TrialId, TrialProgress, TrialSample,
    fs::{Fault, Op},
};
use togi_machine::{
    BootId, CancellationToken, CpuId, FrequencyAction, FrequencyDiscarded, FrequencyEvent,
    FrequencyIntent, FrequencyJournal, FrequencyPolicyId, FrequencyReadback, Reporter, Sample,
    Signal,
};
use togi_sim::model::Script;

use super::{FrequencySink, Journaled};
use crate::error::{AppendFailure, JournalCause};
use crate::testing::{Bench, Rig, core};
use crate::{Ended, run};

fn trial() -> TrialId {
    TrialId::from("0042".to_owned())
}

fn payloads(rig: &Rig, from: usize) -> Vec<Payload> {
    rig.events()[from..]
        .iter()
        .map(|event| event.payload.clone())
        .collect()
}

#[test]
fn a_sample_is_journaled_whole_citing_the_trial_start() {
    let mut rig = Rig::new(2);
    let start = rig.events().last().unwrap().seq;
    let before = rig.events().len();
    let cancel = CancellationToken::new();
    let trial = trial();
    Journaled::new(&mut rig.appender, &trial, start, &cancel).sample(Sample {
        warning: "slow sample".to_owned(),
        pid: 4101,
        tid: 4107,
        cpu: CpuId::from(13),
    });
    assert_eq!(
        payloads(&rig, before),
        [Payload::TrialSample(TrialSample {
            trial: trial.clone(),
            warning: "slow sample".to_owned(),
            pid: 4101,
            tid: 4107,
            cpu: CpuId::from(13),
        })]
    );
    assert_eq!(rig.events().last().unwrap().cause, [start]);
    assert!(!cancel.is_cancelled());
}

#[test]
fn the_first_append_failure_is_kept_cancels_the_trial_and_silences_later_reports() {
    let mut rig = Rig::new(2);
    let start = rig.events().last().unwrap().seq;
    let before = rig.events().len();
    let cancel = CancellationToken::new();
    let trial = trial();
    rig.fail_journal_write(1);
    let mut reporter = Journaled::new(&mut rig.appender, &trial, start, &cancel);
    reporter.sample(Sample {
        warning: String::new(),
        pid: 1,
        tid: 2,
        cpu: CpuId::from(3),
    });
    assert!(cancel.is_cancelled());
    reporter.progress("after the failure");
    reporter.signal(core(0), Signal::Stall, None, "after the failure");
    let Some(AppendFailure::Journal(failure)) = reporter.into_failure() else {
        panic!("the first failure was not kept");
    };
    assert!(matches!(failure.cause(), JournalCause::Append(_)));
    assert_eq!(rig.events().len(), before);
}

#[test_case(Signal::ComputationError, "core 07 computation error: bad sum", true; "computation error")]
#[test_case(Signal::UnexpectedExit, "core 07 backend exited early: bad sum", true; "unexpected exit")]
#[test_case(Signal::Stall, "core 07 backend stalled: bad sum", true; "stall")]
#[test_case(Signal::CorrectedMce, "core 07 corrected_mce: bad sum", false; "corrected MCE")]
#[test_case(Signal::UncorrectedMce, "core 07 uncorrected_mce: bad sum", false; "uncorrected MCE")]
#[test_case(Signal::Crash, "core 07 crash: bad sum", true; "crash")]
fn every_signal_is_journaled_with_its_label_and_a_core_only_where_one_is_named(
    signal: Signal,
    detail: &str,
    named: bool,
) {
    let mut rig = Rig::new(8);
    let start = rig.events().last().unwrap().seq;
    let before = rig.events().len();
    let cancel = CancellationToken::new();
    let trial = trial();
    Journaled::new(&mut rig.appender, &trial, start, &cancel).signal(
        core(7),
        signal,
        Some(991),
        "bad sum",
    );
    assert_eq!(
        payloads(&rig, before),
        [Payload::TrialProgress(TrialProgress {
            trial: trial.clone(),
            detail: detail.to_owned(),
            signal: Some(signal),
            core: named.then(|| core(7)),
            pid: Some(991),
            fault: None,
        })],
        "{signal:?}"
    );
    assert_eq!(rig.events().last().unwrap().cause, [start], "{signal:?}");
}

fn crashed_after(bench: &Bench, signal: Signal) -> Ended {
    bench
        .sim
        .script_trial(
            "0001",
            Script {
                signal: Some(signal),
                at_s: 1.0,
                core: core(0),
                reset: None,
                then_crash: true,
            },
        )
        .unwrap();
    run(bench.input_ending_at(bench.short_night()))
}

#[test]
fn a_stall_journaled_before_a_crash_is_recovered_as_the_failure_of_the_interrupted_trial() {
    let bench = Bench::new(2);
    let crashed = crashed_after(&bench, Signal::Stall);
    assert!(crashed.errors.iter().any(crate::SessionError::is_crash));
    let progress = crashed
        .journal
        .events()
        .iter()
        .find_map(|event| {
            if let Payload::TrialProgress(progress) = &event.payload
                && progress.signal == Some(Signal::Stall)
            {
                Some(progress.clone())
            } else {
                None
            }
        })
        .expect("the stall was not journaled");
    assert_eq!(progress.core, Some(core(0)));
    assert!(progress.detail.starts_with("core 00 backend stalled: "));
    drop(crashed);
    bench.sim.reboot().unwrap();

    let input = bench.input();
    input.cancel.cancel();
    let resumed = run(input);
    let end = resumed
        .journal
        .events()
        .iter()
        .find_map(|event| {
            if let Payload::TrialEnd(end) = &event.payload
                && end.trial.as_str() == "0001"
            {
                Some(end)
            } else {
                None
            }
        })
        .expect("the interrupted trial was not ended");
    assert_eq!(end.outcome, Outcome::Failure);
    assert_eq!(end.signal, Some(Signal::Stall));
    assert_eq!(end.core, Some(core(0)));
    assert!(end.interrupted);
}

fn frequency_intent() -> FrequencyIntent {
    FrequencyIntent {
        policy: FrequencyPolicyId::new(0),
        related_cpus: vec![CpuId::from(0), CpuId::from(1)],
        original_khz: 5_000_000,
        cap_khz: 3_000_000,
        action: FrequencyAction::Cap,
    }
}

fn frequency_readback() -> FrequencyReadback {
    FrequencyReadback {
        intent: frequency_intent(),
        observed_khz: 3_000_000,
    }
}

fn frequency_discarded() -> FrequencyDiscarded {
    FrequencyDiscarded {
        boot: BootId::from("prior-boot".to_owned()),
        reason: "pending record belongs to another boot".to_owned(),
    }
}

#[test_case(FrequencyEvent::Intent(frequency_intent()), Payload::FrequencyIntent(frequency_intent()); "intent")]
#[test_case(FrequencyEvent::Readback(frequency_readback()), Payload::FrequencyReadback(frequency_readback()); "readback")]
#[test_case(FrequencyEvent::Discarded(frequency_discarded()), Payload::FrequencyDiscarded(frequency_discarded()); "discarded")]
fn frequency_sink_acknowledges_only_after_the_typed_event_is_durable(
    event: FrequencyEvent,
    payload: Payload,
) {
    let mut rig = Rig::new(2);
    let before = rig.events().len();
    let cancel = CancellationToken::new();
    rig.mem.record_history();
    let mut sink = FrequencySink::new(&mut rig.appender, &cancel);
    sink.record(event).unwrap();
    assert!(sink.into_failure().is_none());
    assert_eq!(payloads(&rig, before), [payload]);
    assert_eq!(
        rig.mem.log().iter().map(|(op, _)| *op).collect::<Vec<_>>(),
        [Op::Write, Op::Sync],
    );
    assert!(!cancel.is_cancelled());
}

#[test_case(Fault::before(Op::Write, 1); "write refused")]
#[test_case(Fault::before(Op::Sync, 1); "durability refused")]
fn frequency_sink_refuses_failed_ack_keeps_the_original_failure_and_never_appends_again(
    fault: Fault,
) {
    let mut rig = Rig::new(2);
    let before = rig.events().len();
    let cancel = CancellationToken::new();
    rig.mem.inject(fault);
    let mut sink = FrequencySink::new(&mut rig.appender, &cancel);
    assert!(
        sink.record(FrequencyEvent::Intent(frequency_intent()))
            .is_err()
    );
    assert!(cancel.is_cancelled());
    let mutations = rig.mem.mutations();
    assert!(
        sink.record(FrequencyEvent::Readback(frequency_readback()))
            .is_err()
    );
    assert_eq!(rig.mem.mutations(), mutations);
    let Some(AppendFailure::Journal(failure)) = sink.into_failure() else {
        panic!("the original append failure was not retained");
    };
    assert!(matches!(failure.cause(), JournalCause::Append(_)));
    assert_eq!(rig.events().len(), before);
}

#[test_case(FrequencyEvent::Intent(frequency_intent()), Payload::FrequencyIntent(frequency_intent()); "intent")]
#[test_case(FrequencyEvent::Readback(frequency_readback()), Payload::FrequencyReadback(frequency_readback()); "readback")]
fn a_frequency_report_acknowledges_the_durable_typed_event_citing_the_trial_start(
    event: FrequencyEvent,
    payload: Payload,
) {
    let mut rig = Rig::new(2);
    let start = rig.events().last().unwrap().seq;
    let before = rig.events().len();
    let cancel = CancellationToken::new();
    let trial = trial();
    rig.mem.record_history();
    let mut reporter = Journaled::new(&mut rig.appender, &trial, start, &cancel);
    reporter.frequency(event).unwrap();
    assert!(reporter.into_failure().is_none());
    assert_eq!(payloads(&rig, before), [payload]);
    assert_eq!(rig.events().last().unwrap().cause, [start]);
    assert_eq!(
        rig.mem.log().iter().map(|(op, _)| *op).collect::<Vec<_>>(),
        [Op::Write, Op::Sync],
    );
    assert_eq!(cancel.is_cancelled(), false);
}

#[test]
fn a_failed_frequency_report_cancels_the_trial_and_refuses_later_write_acknowledgements() {
    let mut rig = Rig::new(2);
    let start = rig.events().last().unwrap().seq;
    let before = rig.events().len();
    let cancel = CancellationToken::new();
    let trial = trial();
    rig.mem.inject(Fault::before(Op::Sync, 1));
    let mut reporter = Journaled::new(&mut rig.appender, &trial, start, &cancel);
    assert!(
        reporter
            .frequency(FrequencyEvent::Intent(frequency_intent()))
            .is_err()
    );
    assert!(cancel.is_cancelled());
    let mutations = rig.mem.mutations();
    reporter.progress("must not continue");
    assert!(
        reporter
            .frequency(FrequencyEvent::Readback(frequency_readback()))
            .is_err()
    );
    assert_eq!(rig.mem.mutations(), mutations);
    let Some(AppendFailure::Journal(failure)) = reporter.into_failure() else {
        panic!("the original append failure was not retained");
    };
    assert!(matches!(failure.cause(), JournalCause::Append(_)));
    assert_eq!(rig.events().len(), before);
}

enum FrequencyFailureAt {
    Start,
    Recovery,
}

struct FrequencyFailure {
    inner: Box<dyn togi_machine::Trials>,
    mem: MemFs,
    at: FrequencyFailureAt,
    refused: Arc<AtomicBool>,
}

impl togi_machine::Trials for FrequencyFailure {
    fn controller_pid(&self) -> u32 {
        self.inner.controller_pid()
    }

    fn sweep(
        &mut self,
        cancel: &CancellationToken,
    ) -> Result<togi_machine::Swept, togi_machine::Error> {
        self.inner.sweep(cancel)
    }

    fn recover_frequency(
        &mut self,
        stopped: &togi_machine::Stopped,
        journal: &mut dyn FrequencyJournal,
    ) -> Result<String, togi_machine::Error> {
        match self.at {
            FrequencyFailureAt::Start => self.inner.recover_frequency(stopped, journal),
            FrequencyFailureAt::Recovery => {
                self.mem.inject(Fault::before(Op::Sync, 1));
                // Even a runner that incorrectly ignores a refused acknowledgement must not
                // make the session continue into profile writes or trials.
                let result = journal.record(FrequencyEvent::Intent(frequency_intent()));
                self.refused.store(result.is_err(), Ordering::SeqCst);
                Ok("frequency recovery incorrectly reported success".to_owned())
            }
        }
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: togi_machine::TrialSpec,
        ready: togi_machine::Stopped,
        journal: &mut dyn FrequencyJournal,
    ) -> Result<Box<dyn togi_machine::Running>, togi_machine::Partial<Option<togi_machine::Stopped>>>
    {
        if let FrequencyFailureAt::Recovery = self.at {
            return self.inner.start(cancel, spec, ready, journal);
        }
        self.mem.inject(Fault::before(Op::Sync, 1));
        let result = journal.record(FrequencyEvent::Intent(frequency_intent()));
        self.refused.store(result.is_err(), Ordering::SeqCst);
        let error = result.unwrap_err();
        assert!(cancel.is_cancelled());
        // A runner may classify its setup failure as inconclusive; the session must retain the
        // journal failure instead of retrying that trial.
        Err(togi_machine::Partial {
            value: Some(ready),
            error: togi_machine::Error::Other(Box::new(error)),
        })
    }

    fn ended(&mut self, id: &str) -> Result<(), togi_machine::Error> {
        self.inner.ended(id)
    }

    fn content(
        &mut self,
        workload: togi_machine::WorkloadId,
    ) -> Result<togi_machine::ContentId, togi_machine::Error> {
        self.inner.content(workload)
    }

    fn samples<'a>(
        &'a self,
        id: &'a str,
    ) -> Box<
        dyn Iterator<Item = Result<togi_machine::TrialConditions, togi_machine::SampleError>> + 'a,
    > {
        self.inner.samples(id)
    }
}

#[test_case(FrequencyFailureAt::Start; "start")]
#[test_case(FrequencyFailureAt::Recovery; "startup recovery")]
fn a_failed_frequency_ack_ends_the_session_without_starting_or_retrying_the_trial(
    at: FrequencyFailureAt,
) {
    let bench = Bench::new(2);
    let mut input = bench.input_ending_at(bench.short_night());
    let refused = Arc::new(AtomicBool::new(false));
    input.machine.trials = Box::new(FrequencyFailure {
        inner: input.machine.trials,
        mem: bench.mem.clone(),
        at,
        refused: refused.clone(),
    });
    let ended = run(input);
    assert!(
        refused.load(Ordering::SeqCst),
        "the frequency append must be refused at its own seam"
    );
    assert!(ended.errors.iter().any(|error| matches!(
        error,
        crate::SessionError::Journal(failure) if matches!(failure.cause(), JournalCause::Append(_))
    )));
    assert!(!ended.journal.events().iter().any(|event| matches!(
        event.payload,
        Payload::TrialStart(_) | Payload::TrialEnd(_) | Payload::BackendRetry(_)
    )));
}
