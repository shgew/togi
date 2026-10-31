use std::cell::Cell;
use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::path::Path;
use std::rc::Rc;

use togi_journal::{Event, Payload, WorkloadContent};
use togi_machine::{ALL_REGIMES, Backend, ContentId, Digest, Error, Regime, WorkloadId, workloads};
use togi_simrun::{SimOutcome, simulate};

use super::common::Host;
use crate::content::{RecordedContent, recorded};

const MACHINE: [&str; 4] = ["--machine", "/hunt.json", "--state-dir", "/state"];

/// A fixture identity no simulator produces, one per marker.
fn marked(workload: WorkloadId, marker: u8) -> ContentId {
    ContentId {
        backend: workload.backend(),
        executable: Digest::from_bytes([marker; 32]),
        configuration: Digest::from_bytes([marker ^ 0x55; 32]),
    }
}

fn everything_marked(marker: u8) -> RecordedContent {
    RecordedContent {
        contents: ALL_REGIMES
            .into_iter()
            .flat_map(workloads)
            .map(|workload| workload.id().runs())
            .map(|workload| (workload, marked(workload, marker)))
            .collect(),
    }
}

/// Every identity the journal's exposures and trial intents record.
fn identities(events: &[Event]) -> Vec<WorkloadContent> {
    events
        .iter()
        .flat_map(|event| recorded(&event.payload).iter().cloned())
        .collect()
}

/// The identities of the trial intents alone: what the trials a run starts report. An exposure also records the
/// identities of an earlier intent, so after a resume it can carry the ones the recording was made with.
fn intents(events: &[Event]) -> Vec<WorkloadContent> {
    events
        .iter()
        .filter_map(|event| {
            if let Payload::TrialIntent(intent) = &event.payload {
                Some(intent.content.iter().cloned())
            } else {
                None
            }
        })
        .flatten()
        .collect()
}

/// One boot of the hunting machine whose trials report `marker` identities, ending in a crash.
fn record(host: &mut Host, marker: u8) {
    let before = host
        .states
        .get(Path::new("/state"))
        .map_or(0, |_| host.events("/state").len());
    let ran = host.execute(
        MACHINE.into_iter().chain(["--max-boots", "1"]),
        |mut input| {
            input.adapt_trials = Some(everything_marked(marker).adapter());
            simulate(input)
        },
    );
    assert!(matches!(ran.code, Some(0 | 3)), "{}", ran.stderr);
    assert!(
        !identities(&host.events("/state")[before..]).is_empty(),
        "the recording of marker {marker} holds no workload identity"
    );
}

/// Resumes the one-boot recording for one boot and returns what the trial intents it appends report. The recording
/// holds only the workloads its own boot ran, so a longer resume could meet one it never recorded and refuse.
fn resumed(host: &mut Host, extra: &[&str]) -> Vec<WorkloadContent> {
    let before = host.events("/state").len();
    let ran = host.run(
        MACHINE
            .into_iter()
            .chain(["--max-boots", "1"])
            .chain(extra.iter().copied()),
    );
    assert!(matches!(ran.code, Some(0 | 3)), "{}", ran.stderr);
    let appended = intents(&host.events("/state")[before..]);
    assert!(!appended.is_empty(), "the resumed run started no trial");
    appended
}

#[test]
fn recorded_content_keeps_the_recorded_identities_when_resuming() {
    let mut host = Host::hunting();
    record(&mut host, 7);
    for content in resumed(&mut host, &["--recorded-content"]) {
        assert_eq!(
            content.content,
            marked(content.workload.runs(), 7),
            "{content:?}"
        );
    }
}

#[test]
fn without_the_option_a_resumed_run_reports_synthetic_identities() {
    let mut host = Host::hunting();
    record(&mut host, 7);
    for content in resumed(&mut host, &[]) {
        assert_eq!(
            content.content,
            togi_sim::content_id(content.workload),
            "{content:?}"
        );
    }
}

#[test]
fn the_runner_wraps_the_trials_of_every_boot() {
    let wrapped = Rc::new(Cell::new(0_u32));
    let mut host = Host::hunting();
    let counter = Rc::clone(&wrapped);
    let ran = host.execute(
        MACHINE.into_iter().chain(["--max-boots", "3"]),
        move |mut input| {
            let mut adapt = everything_marked(7).adapter();
            input.adapt_trials = Some(Box::new(move |trials| {
                counter.set(counter.get() + 1);
                adapt(trials)
            }));
            simulate(input)
        },
    );
    assert_eq!(ran.code, Some(3), "{}", ran.stderr);
    assert_eq!(wrapped.get(), 3);
    let appended = intents(&host.events("/state"));
    assert!(!appended.is_empty(), "no trial started");
    for content in appended {
        assert_eq!(
            content.content,
            marked(content.workload.runs(), 7),
            "{content:?}"
        );
    }
}

#[test]
fn the_option_gives_the_runner_its_adapter_and_the_default_gives_none() {
    let mut host = Host::hunting();
    record(&mut host, 7);
    let ran = host.execute(MACHINE, |input| {
        assert!(input.adapt_trials.is_none());
        Ok(SimOutcome::Capped {
            boots: NonZeroU32::MIN,
        })
    });
    assert_eq!(ran.code, Some(3), "{}", ran.stderr);
    let ran = host.execute(MACHINE.into_iter().chain(["--recorded-content"]), |input| {
        assert!(input.adapt_trials.is_some());
        Ok(SimOutcome::Capped {
            boots: NonZeroU32::MIN,
        })
    });
    assert_eq!(ran.code, Some(3), "{}", ran.stderr);
}

#[test]
fn the_adapter_answers_recorded_workloads_and_refuses_the_rest() {
    let mut host = Host::hunting();
    record(&mut host, 7);
    let known: BTreeSet<WorkloadId> = identities(&host.events("/state"))
        .iter()
        .map(|content| content.workload.runs())
        .collect();
    let absent = ALL_REGIMES
        .into_iter()
        .flat_map(workloads)
        .map(togi_machine::Workload::id)
        .find(|workload| !known.contains(&workload.runs()))
        .expect("the bounded recording leaves an unseen workload");
    let ran = host.execute(
        MACHINE.into_iter().chain(["--recorded-content"]),
        |mut input| {
            let mut adapt = input.adapt_trials.take().unwrap();
            let mut trials = adapt(input.simulator.seams().trials);
            for workload in &known {
                assert_eq!(trials.content(*workload).unwrap(), marked(*workload, 7));
            }
            let refused = trials.content(absent).unwrap_err();
            assert!(refused.to_string().contains(absent.as_str()), "{refused}");
            // A backend the simulator cannot run stays missing whatever the recording says.
            input.simulator.miss_backend(Backend::Mprime);
            let mut trials = adapt(input.simulator.seams().trials);
            let mprime = workloads(Regime::R1)[0].id();
            assert!(matches!(trials.content(mprime), Err(Error::BackendMissing)));
            Ok(SimOutcome::Capped {
                boots: NonZeroU32::MIN,
            })
        },
    );
    assert_eq!(ran.code, Some(3), "{}", ran.stderr);
}

#[test]
fn conflicting_recorded_identities_refuse_before_the_session_starts() {
    let mut host = Host::hunting();
    record(&mut host, 7);
    record(&mut host, 8);
    let before = host.events("/state");
    let ran = host.execute(MACHINE.into_iter().chain(["--recorded-content"]), |_| {
        unreachable!("the session started on ambiguous identities")
    });
    assert_eq!(ran.code, Some(1), "{}", ran.stderr);
    assert!(
        ran.stderr
            .starts_with("sim: read the content identities the journal recorded: events ")
            && ran
                .stderr
                .contains("record different content identities for workload "),
        "{}",
        ran.stderr
    );
    assert_eq!(host.events("/state").len(), before.len());
}

#[test]
fn the_option_without_a_recording_refuses_before_the_session_starts() {
    let mut host = Host::hunting();
    let ran = host.execute(MACHINE.into_iter().chain(["--recorded-content"]), |_| {
        unreachable!("the session started without recorded identities")
    });
    assert_eq!(ran.code, Some(1), "{}", ran.stderr);
    assert_eq!(
        ran.stderr,
        "sim: read the content identities the journal recorded: the state directory's journal records no workload content identity\n"
    );
}
