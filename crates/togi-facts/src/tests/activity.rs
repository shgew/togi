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
            cpu: CpuId(0),
        }),
    }
}

#[test]
fn trial_activity_scoped_to_open_started_trial() {
    for kind in ["progress", "signal", "sample"] {
        for state in [
            "running",
            "not started",
            "ended",
            "other boot",
            "unknown trial",
        ] {
            let mut s = Script::begin(&[0]);
            let intent = s.add(alone("one", 0, -10, &[0]));
            let mut want = Script::time(intent);
            if state != "not started" {
                want = Script::time(s.add(started("one")));
            }
            if state == "ended" {
                s.add(end("one", Outcome::Inconclusive, 1));
            }
            if state == "other boot" {
                s.boot("b");
            }
            let target = if state == "unknown trial" {
                "other"
            } else {
                "one"
            };
            let at = evidence(kind, target, &mut s);
            if state == "running" {
                want = Script::time(at);
            }
            let facts = s.extract();
            assert!(
                facts.facts.is_empty(),
                "{kind}/{state}: interrupted work is not a fact"
            );
            if state == "ended" {
                assert!(facts.open_trials.is_empty(), "{kind}/{state}");
                continue;
            }
            let [open] = &facts.open_trials[..] else {
                panic!("{kind}/{state}: one open trial expected");
            };
            assert_eq!(open.seq, intent);
            assert_eq!(open.started, state != "not started", "{kind}/{state}");
            assert_eq!(open.last_evidence, want, "{kind}/{state}");
            assert!(!open.crashed);
        }
    }
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

#[test]
fn crash_evidence_matches_intent_sequence_and_previous_boot() {
    for (name, in_flight, previous, want) in [
        ("matching", Some(2), "a", true),
        ("no in flight", None, "a", false),
        ("unknown intent", Some(99), "a", false),
        ("different boot", Some(2), "b", false),
    ] {
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
        assert!(facts.facts.is_empty(), "{name}");
        assert_eq!(facts.open_trials[0].crashed, want, "{name}");
    }
}
