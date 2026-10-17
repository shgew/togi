use std::num::NonZeroU32;

use togi_journal::{Outcome, Payload, TrialId, TrialProgress, TrialSample};
use togi_machine::{CancellationToken, CpuId, Reporter, Sample, Signal};
use togi_sim::model::Script;

use super::Journaled;
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
        cpu: CpuId(13),
    });
    assert_eq!(
        payloads(&rig, before),
        [Payload::TrialSample(TrialSample {
            trial: trial.clone(),
            warning: "slow sample".to_owned(),
            pid: 4101,
            tid: 4107,
            cpu: CpuId(13),
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
        cpu: CpuId(3),
    });
    assert!(cancel.is_cancelled());
    reporter.progress("after the failure");
    reporter.signal(core(0), Signal::Stall, "after the failure");
    let Some(AppendFailure::Journal(failure)) = reporter.into_failure() else {
        panic!("the first failure was not kept");
    };
    assert!(matches!(failure.cause(), JournalCause::Append(_)));
    assert_eq!(rig.events().len(), before);
}

#[test]
fn every_signal_is_journaled_with_its_label_and_a_core_only_where_one_is_named() {
    let cases = [
        (
            Signal::ComputationError,
            "core 07 computation error: bad sum",
            true,
        ),
        (
            Signal::UnexpectedExit,
            "core 07 backend exited early: bad sum",
            true,
        ),
        (Signal::Stall, "core 07 backend stalled: bad sum", true),
        (
            Signal::CorrectedMce,
            "core 07 corrected_mce: bad sum",
            false,
        ),
        (
            Signal::UncorrectedMce,
            "core 07 uncorrected_mce: bad sum",
            false,
        ),
        (Signal::Crash, "core 07 crash: bad sum", true),
    ];
    for (signal, detail, named) in cases {
        let mut rig = Rig::new(8);
        let start = rig.events().last().unwrap().seq;
        let before = rig.events().len();
        let cancel = CancellationToken::new();
        let trial = trial();
        Journaled::new(&mut rig.appender, &trial, start, &cancel).signal(
            core(7),
            signal,
            "bad sum",
        );
        assert_eq!(
            payloads(&rig, before),
            [Payload::TrialProgress(TrialProgress {
                trial: trial.clone(),
                detail: detail.to_owned(),
                signal: Some(signal),
                core: named.then(|| core(7)),
            })],
            "{signal:?}"
        );
        assert_eq!(rig.events().last().unwrap().cause, [start], "{signal:?}");
    }
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
    let mut input = bench.input();
    input.policy.cycles = Some(NonZeroU32::MIN);
    run(input)
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
    bench.sim.reboot();

    let mut input = bench.input();
    input.policy.cycles = Some(NonZeroU32::MIN);
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
