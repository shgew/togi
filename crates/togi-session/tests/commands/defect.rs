//! Pre-fix mapping journals replay through the session's public reducer, entirely in memory.
use std::collections::BTreeMap;

use jiff::Timestamp;
use test_case::test_case;
use togi_journal::{
    Event, FixStamp, Payload, Queue, Seq, SessionBaseline, SessionContext, SessionStart, Stamp,
    TaskId, Tested, TrialEnd, TrialId, TrialIntent, TrialStart, WorkloadContent,
};
use togi_machine::{
    Backend, BootId, CcdId, ContentId, CoreInfo, CpuId, Digest, ObservedOffset, TargetOffset,
};
use togi_session::ReadState;
use togi_tuner::policy::{Action, Contents, NightQuery, NotReady, Params, State, TrialPlan};

fn at(second: i64) -> Timestamp {
    Timestamp::from_second(1_700_000_000 + second).unwrap()
}

fn ccd(id: u8) -> CcdId {
    CcdId::try_from(id).unwrap()
}

fn cores() -> Vec<CoreInfo> {
    (0..4)
        .map(|id| CoreInfo {
            core: super::core(id),
            ccd: ccd(u8::try_from(id / 2).unwrap()),
            cpus: vec![CpuId::new(id).unwrap(), CpuId::new(id + 32).unwrap()],
        })
        .collect()
}

fn content() -> ContentId {
    ContentId {
        backend: Backend::Mprime,
        executable: Digest::from_bytes([1; 32]),
        configuration: Digest::from_bytes([2; 32]),
    }
}

fn firmware(version: &str) -> SessionContext {
    SessionContext {
        bios_version: version.into(),
        board: "board".into(),
        cpu_model: "cpu".into(),
        microcode: "mc".into(),
        boost_limit_mhz: 5000,
    }
}

/// The minimal public-policy driver this session replay fixture needs. Events never leave memory.
struct Journal {
    events: Vec<Event>,
    state: State,
}

impl Journal {
    fn ready() -> Self {
        let mut journal = Self {
            events: Vec::new(),
            state: State::new(Params::SPEC),
        };
        let mut build = super::build();
        build.ruleset = 13;
        journal.append(
            SessionStart {
                build,
                session: "20231114T221320Z".parse().unwrap(),
                cores: cores(),
            }
            .into(),
            &[],
        );
        journal.append(firmware("1").into(), &[]);
        journal.append(
            SessionBaseline {
                offsets: cores()
                    .iter()
                    .map(|info| (info.core, ObservedOffset(0)))
                    .collect(),
            }
            .into(),
            &[],
        );
        journal
    }

    fn append(&mut self, payload: Payload, cause: &[Seq]) -> Seq {
        let seq = Seq::new(u64::try_from(self.events.len()).unwrap() + 1).unwrap();
        let event = Event::new(
            seq,
            Stamp {
                time: at(i64::try_from(seq.get()).unwrap()),
                mono: None,
            },
            BootId::from("synthetic-boot".to_owned()),
            cause.to_vec(),
            payload,
        );
        self.state.fold(&event).unwrap();
        self.events.push(event);
        seq
    }

    fn next(&self, now: Timestamp) -> Result<Action, NotReady> {
        let contents = Contents::new(togi_machine::ALL_REGIMES.into_iter().flat_map(|regime| {
            togi_machine::workloads(regime)
                .iter()
                .map(|workload| (workload.id(), content()))
        }));
        self.state.next(&NightQuery {
            now,
            contents: &contents,
            unavailable: &BTreeMap::new(),
        })
    }

    fn until_trial(&mut self, now: Timestamp) -> TrialPlan {
        for _ in 0..200 {
            match self.next(now).unwrap() {
                Action::Decide(decision) => {
                    self.append(decision.payload.into(), &decision.causes);
                }
                Action::RunTrial(plan) => return plan,
                Action::EndNight(reason) => panic!("the fixture night ended: {reason:?}"),
            }
        }
        panic!("the fixture night kept deciding");
    }

    fn run(&mut self, plan: &TrialPlan) {
        let trial = TrialId::from(format!("synthetic-trial-{}", self.events.len() + 1));
        let machine = cores();
        let intent = plan
            .intent(
                trial.clone(),
                &machine,
                plan.workloads()
                    .into_iter()
                    .map(|workload| WorkloadContent {
                        workload,
                        content: content(),
                    })
                    .collect(),
            )
            .unwrap();
        let duration = intent.duration_s;
        let intent = self.append(intent.into(), &plan.causes);
        let mut start: TrialStart = serde_json::from_value(serde_json::json!({
            "trial": trial.as_str(), "scope": "synthetic-scope", "pid": 1, "cpus": [], "argv": []
        }))
        .unwrap();
        start.roles = plan.roles(1, &machine).unwrap();
        self.append(start.into(), &[intent]);
        let end: TrialEnd = serde_json::from_value(serde_json::json!({
            "trial": trial.as_str(), "outcome": "pass", "duration_s": duration, "interrupted": false
        }))
        .unwrap();
        self.append(end.into(), &[intent]);
    }
}

/// A synthetic old journal with CCD 0's zero and -5 coarse steps finished. The saved zero
/// intent lets the fixture reproduce the old policy's new-task rewind without running old code.
fn pre_fix_prefix() -> (Journal, TrialIntent, TrialIntent) {
    let mut journal = Journal::ready();
    let mut first = None;
    for _ in 0..2000 {
        let plan = journal.until_trial(at(100));
        let intent = plan
            .intent(
                TrialId::from("synthetic-probe".to_owned()),
                &cores(),
                plan.workloads()
                    .into_iter()
                    .map(|workload| togi_journal::WorkloadContent {
                        workload,
                        content: content(),
                    })
                    .collect(),
            )
            .unwrap();
        if intent.task.is_some_and(|task| task.queue == Queue::Mapping)
            && let Some(Tested::Isolated { ccd: unit, offset }) = intent.tested
            && unit == ccd(0)
        {
            if offset.get() == 0 && first.is_none() {
                first = Some(intent.clone());
            }
            if offset.get() == -10 {
                return (journal, first.unwrap(), intent);
            }
        }
        journal.run(&plan);
    }
    panic!("mapping did not finish its first two coarse steps");
}

fn jsonl_roundtrip(events: &[Event]) -> Vec<Event> {
    let jsonl = events
        .iter()
        .map(|event| serde_json::to_string(event).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    jsonl
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[derive(Clone, Copy)]
enum IntentCase {
    Rewind,
    CurrentCursor,
    OtherCcd,
    Resumed,
    Fixed,
    ChangedContext,
    Exact,
}

#[test_case(IntentCase::Rewind, 1; "new unstamped shallower task is marked")]
#[test_case(IntentCase::CurrentCursor, 0; "ordinary next offset is unmarked")]
#[test_case(IntentCase::OtherCcd, 0; "another CCD without completed steps is unmarked")]
#[test_case(IntentCase::Resumed, 0; "existing task intent is unmarked")]
#[test_case(IntentCase::Fixed, 0; "fixed build suppresses the finding")]
#[test_case(IntentCase::ChangedContext, 0; "earlier context progress is unmarked")]
#[test_case(IntentCase::Exact, 0; "nonisolated intent is unmarked")]
fn pre_fix_fixture_replay_marks_only_the_narrow_rewind(case: IntentCase, expected: usize) {
    let (mut journal, first, next) = pre_fix_prefix();
    let mut intent = match case {
        IntentCase::CurrentCursor => next,
        IntentCase::Rewind
        | IntentCase::OtherCcd
        | IntentCase::Resumed
        | IntentCase::Fixed
        | IntentCase::ChangedContext
        | IntentCase::Exact => first,
    };
    intent.trial = TrialId::from("synthetic-rewind".to_owned());
    if !matches!(case, IntentCase::Resumed) {
        intent.task = Some(TaskId {
            queue: Queue::Mapping,
            ordinal: 9000,
        });
    }
    if matches!(case, IntentCase::OtherCcd) {
        // Use a CCD absent from the layout, so CCD 0's progress cannot be borrowed.
        intent.tested = Some(Tested::Isolated {
            ccd: ccd(2),
            offset: TargetOffset::ZERO,
        });
    }
    if matches!(case, IntentCase::ChangedContext) {
        journal.append(firmware("synthetic-changed-firmware").into(), &[]);
    }
    if matches!(case, IntentCase::Exact) {
        intent.tested = Some(Tested::Exact {});
    }
    let rewind = journal.append(intent.into(), &[]);
    let mut events = jsonl_roundtrip(&journal.events);
    if matches!(case, IntentCase::Fixed) {
        let Payload::SessionStart(start) = &mut events[0].payload else {
            panic!("session start");
        };
        start.build.fixes = FixStamp::through(togi_journal::DefectId::new(1).unwrap());
    }
    let replay = ReadState::replay(&events).unwrap();
    let findings = replay.defects().new_findings().collect::<Vec<_>>();
    assert_eq!(findings.len(), expected);
    if expected == 1 {
        assert_eq!(findings[0].entry.id.get(), 1);
        assert_eq!(findings[0].decisions, [rewind]);
        assert_eq!(
            findings[0].entry.direction,
            togi_journal::DefectDirection::TooCautious
        );
    }
}

#[test]
fn a_pre_fix_rewind_does_not_prevent_fixed_mapping_decisions() {
    let (mut journal, mut first, _) = pre_fix_prefix();
    first.trial = TrialId::from("synthetic-rewind".to_owned());
    first.task = Some(TaskId {
        queue: Queue::Mapping,
        ordinal: 9000,
    });
    journal.append(first.into(), &[]);
    let replay = ReadState::replay(&jsonl_roundtrip(&journal.events)).unwrap();
    assert_eq!(replay.defects().new_findings().count(), 1);
    // Replay retains the unfinished old task, but the fixed policy still produces real work.
    let mut continued = journal;
    for _ in 0..50 {
        match continued.next(at(100)).unwrap() {
            Action::Decide(decision) => {
                continued.append(decision.payload.into(), &decision.causes);
            }
            Action::RunTrial(plan) => {
                continued.run(&plan);
                let replay = ReadState::replay(&jsonl_roundtrip(&continued.events)).unwrap();
                assert_eq!(replay.defects().new_findings().count(), 1);
                return;
            }
            Action::EndNight(reason) => {
                panic!("fixed mapping stopped instead of continuing: {reason:?}")
            }
        }
    }
    panic!("fixed mapping never scheduled its continuation");
}
