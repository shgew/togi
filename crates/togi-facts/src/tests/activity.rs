use test_case::test_case;

use togi_journal::{
    CrashDetected, Outcome, Seq, TrialProgress, TrialSample, TrialSignal, TrialStart,
};
use togi_machine::{BootId, CpuId};

use super::{Script, alone, end, trial};

fn started(id: &str) -> TrialStart {
    TrialStart {
        trial: trial(id),
        window_start_ns: None,
        scope: "scope".into(),
        pid: 1,
        cpus: Vec::new(),
        argv: Vec::new(),
        files: Vec::new(),
        instances: Vec::new(),
    }
}

fn evidence(kind: &str, id: &str, script: &mut Script) -> Seq {
    match kind {
        "progress" => script.add(TrialProgress {
            trial: trial(id),
            detail: String::new(),
            signal: None,
            core: None,
        }),
        "signal" => script.add(TrialSignal {
            trial: trial(id),
            schedule: None,
            stops: 0,
            conts: 0,
        }),
        _ => script.add(TrialSample {
            trial: trial(id),
            warning: String::new(),
            pid: 1,
            tid: 1,
            cpu: CpuId::from(0),
        }),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActivityState {
    Running,
    NotStarted,
    Ended,
    OtherBoot,
    UnknownTrial,
}

#[test_case("progress", ActivityState::Running; "progress running")]
#[test_case("progress", ActivityState::NotStarted; "progress not started")]
#[test_case("progress", ActivityState::Ended; "progress ended")]
#[test_case("progress", ActivityState::OtherBoot; "progress other boot")]
#[test_case("progress", ActivityState::UnknownTrial; "progress unknown trial")]
#[test_case("signal", ActivityState::Running; "signal running")]
#[test_case("signal", ActivityState::NotStarted; "signal not started")]
#[test_case("signal", ActivityState::Ended; "signal ended")]
#[test_case("signal", ActivityState::OtherBoot; "signal other boot")]
#[test_case("signal", ActivityState::UnknownTrial; "signal unknown trial")]
#[test_case("sample", ActivityState::Running; "sample running")]
#[test_case("sample", ActivityState::NotStarted; "sample not started")]
#[test_case("sample", ActivityState::Ended; "sample ended")]
#[test_case("sample", ActivityState::OtherBoot; "sample other boot")]
#[test_case("sample", ActivityState::UnknownTrial; "sample unknown trial")]
fn trial_activity_scoped_to_open_started_trial(kind: &str, state: ActivityState) {
    let mut s = Script::begin(&[0]);
    let intent = s.add(alone("one", 0, -10, &[0]));
    let mut want = Script::time(intent);
    if state != ActivityState::NotStarted {
        want = Script::time(s.add(started("one")));
    }
    if state == ActivityState::Ended {
        s.add(end("one", Outcome::Inconclusive, 1));
    }
    if state == ActivityState::OtherBoot {
        s.boot("b");
    }
    let target = if state == ActivityState::UnknownTrial {
        "other"
    } else {
        "one"
    };
    let at = evidence(kind, target, &mut s);
    if state == ActivityState::Running {
        want = Script::time(at);
    }
    let facts = s.extract();
    assert!(
        facts.facts.is_empty(),
        "{kind}/{state:?}: interrupted work is not a fact"
    );
    if state == ActivityState::Ended {
        assert!(facts.open_trials.is_empty(), "{kind}/{state:?}");
        return;
    }
    let [open] = &facts.open_trials[..] else {
        panic!("{kind}/{state:?}: one open trial expected");
    };
    assert_eq!(open.seq, intent);
    assert_eq!(
        open.started,
        state != ActivityState::NotStarted,
        "{kind}/{state:?}"
    );
    assert_eq!(open.last_evidence, want, "{kind}/{state:?}");
    assert!(!open.crashed);
}

#[test]
fn trial_start_from_the_wrong_boot_does_not_start_the_trial() {
    let mut s = Script::begin(&[0]);
    let intent = s.add(alone("one", 0, -10, &[0]));
    s.boot("b");
    s.add(started("one"));
    s.boot("a");
    evidence("progress", "one", &mut s);
    let facts = s.extract();
    let [open] = &facts.open_trials[..] else {
        unreachable!()
    };
    assert!(!open.started);
    assert_eq!(open.last_evidence, Script::time(intent));
}

#[test]
fn trial_start_after_end_does_nothing() {
    let mut s = Script::begin(&[0]);
    s.add(alone("one", 0, -10, &[0]));
    s.add(end("one", Outcome::Inconclusive, 1));
    s.add(started("one"));
    let facts = s.extract();
    assert!(facts.open_trials.is_empty());
    assert!(facts.facts.is_empty());
}

#[test_case(Some(2), "a", true; "matching")]
#[test_case(None, "a", false; "no in flight")]
#[test_case(Some(99), "a", false; "unknown intent")]
#[test_case(Some(2), "b", false; "different boot")]
fn crash_evidence_matches_intent_sequence_and_previous_boot(
    in_flight: Option<u64>,
    previous: &str,
    want: bool,
) {
    let mut s = Script::begin(&[0]);
    s.add(alone("one", 0, -10, &[0]));
    s.boot("c");
    s.add(CrashDetected {
        previous_boot: BootId::from(previous.to_owned()),
        in_flight: in_flight.and_then(Seq::new),
        stray: false,
        condition: None,
        reset_reason: None,
        reset_reason_raw: Vec::new(),
        inconclusive: false,
        pstore: None,
    });
    let facts = s.extract();
    assert!(facts.facts.is_empty());
    assert_eq!(facts.open_trials[0].crashed, want);
}
