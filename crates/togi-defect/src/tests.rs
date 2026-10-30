use jiff::Timestamp;
use test_case::test_case;
use togi_journal::{
    Attribution, Binding, Build, CcdCap, CcdEdge, CommandReset, ConfigLoaded, DefectAnswer,
    DefectAnswered, DefectDirection, DefectFound, DefectId, Edge, Event, EventKind, Failure,
    Finite, FixStamp, Payload, Rate, ResetTarget, SCHEMA, Seq, SessionStart, SessionWarning, Slope,
    Stamp,
};
use togi_machine::{BootId, CcdId, CoreId, CoreInfo, CoreSet, CpuId, Signal, TargetOffset};

use crate::{Clause, DefectState, Entry, KNOWN_DEFECTS, fixed_stamp, is_ordered};

const BACKOFF_ON_FAILURE: Clause = Clause::DirectCause {
    decision_kind: EventKind::CcdEdge,
    cause_kind: EventKind::Failure,
};
const DEEPER_ON_WARNING: Clause = Clause::DirectCause {
    decision_kind: EventKind::CcdCap,
    cause_kind: EventKind::SessionWarning,
};

const fn entry(id: u32, direction: DefectDirection, clauses: &'static [Clause]) -> Entry {
    Entry {
        id: DefectId::new(id).unwrap(),
        title: "synthetic",
        detail: "synthetic detail",
        pr: 100,
        direction,
        clauses,
    }
}

static CAUTIOUS: [Entry; 1] = [entry(
    1,
    DefectDirection::TooCautious,
    &[BACKOFF_ON_FAILURE],
)];
static FIXED_LATER: [Entry; 1] = [entry(
    2,
    DefectDirection::TooCautious,
    &[BACKOFF_ON_FAILURE],
)];
static PAIR: [Entry; 2] = [
    entry(1, DefectDirection::TooCautious, &[BACKOFF_ON_FAILURE]),
    entry(2, DefectDirection::TooAggressive, &[BACKOFF_ON_FAILURE]),
];
static ALTERNATIVES: [Entry; 1] = [entry(
    1,
    DefectDirection::TooCautious,
    &[DEEPER_ON_WARNING, BACKOFF_ON_FAILURE],
)];
static NONE: [Entry; 0] = [];
static NOT_TUNER: [Entry; 1] = [entry(
    1,
    DefectDirection::TooCautious,
    &[Clause::DirectCause {
        decision_kind: EventKind::SessionWarning,
        cause_kind: EventKind::Failure,
    }],
)];

#[derive(Clone, Copy)]
enum Step {
    Backoff,
    Deeper,
}

fn core(index: i64) -> CoreId {
    CoreId::new(index).unwrap()
}

fn cores(indexes: &[i64]) -> CoreSet {
    indexes.iter().map(|&index| core(index)).collect()
}

fn id(value: u32) -> DefectId {
    DefectId::new(value).unwrap()
}

fn build(fixes: u32) -> Build {
    Build {
        version: "0.0.0".into(),
        rev: None,
        ruleset: 1,
        schema: SCHEMA,
        fixes: DefectId::new(fixes).map_or(FixStamp::none(), FixStamp::through),
        evidence: 1,
    }
}

#[derive(Default)]
struct Log {
    events: Vec<Event>,
}

impl Log {
    fn push(&mut self, cause: &[Seq], payload: impl Into<Payload>) -> Seq {
        let seq = Seq::new(self.events.len() as u64 + 1).unwrap();
        let stamp = Stamp {
            time: Timestamp::from_second(1_700_000_000).unwrap(),
            mono: None,
        };
        self.events.push(Event::new(
            seq,
            stamp,
            BootId::from("boot".to_owned()),
            cause.to_vec(),
            payload.into(),
        ));
        seq
    }

    fn start(&mut self, fixes: u32) -> Seq {
        self.push(
            &[],
            SessionStart {
                build: build(fixes),
                session: "20231114T221320Z".parse().unwrap(),
                cores: (0..16)
                    .map(|index| CoreInfo {
                        core: core(index),
                        ccd: CcdId::new(index).unwrap(),
                        cpus: vec![CpuId::from(u16::try_from(index).unwrap())],
                    })
                    .collect(),
            },
        )
    }

    fn config(&mut self, fixes: u32) -> Seq {
        self.push(
            &[],
            ConfigLoaded {
                build: build(fixes),
                config: togi_config::Config::default().snapshot(),
                path: "togi.json".into(),
                file: false,
                kernel_cursor: None,
                kernel_error: None,
            },
        )
    }

    fn failure(&mut self) -> Seq {
        self.push(
            &[],
            Failure {
                signal: Signal::UnexpectedExit,
                attribution: Attribution::Attributed,
                core: None,
                offset: None,
                trial: None,
                regime: None,
                condition: None,
                profile: None,
                known_failure: None,
                reason: None,
                round: None,
            },
        )
    }

    fn warning(&mut self) -> Seq {
        self.push(
            &[],
            SessionWarning {
                operation: "note".into(),
                trial: None,
                error: String::new(),
            },
        )
    }

    fn decision(&mut self, at: i64, step: Step, cause: &[Seq]) -> Seq {
        let ccd = CcdId::new(at).unwrap();
        match step {
            Step::Backoff => self.push(
                cause,
                CcdEdge {
                    ccd,
                    edge: Edge::Unknown {},
                    old: Edge::Unknown {},
                    reason: "synthetic".into(),
                },
            ),
            Step::Deeper => self.push(
                cause,
                CcdCap {
                    ccd,
                    old: TargetOffset::ZERO,
                    new: TargetOffset::ZERO,
                    budget: Rate::new(1.0).unwrap(),
                    edge: Edge::Unknown {},
                    adequate: Vec::new(),
                    uncovered: Vec::new(),
                    cells: Vec::new(),
                    slope: Slope::Fallback {
                        beta: Finite::new(0.1).unwrap(),
                    },
                    candidates: Vec::new(),
                    binding: Binding::Baseline {},
                },
            ),
        }
    }

    fn found(&mut self, id: u32, direction: DefectDirection, at: &[i64], decisions: &[Seq]) -> Seq {
        self.push(
            decisions,
            DefectFound {
                id: self::id(id),
                title: "recorded".into(),
                detail: "recorded detail".into(),
                pr: 7,
                direction,
                cores: cores(at),
                decisions: decisions.to_vec(),
            },
        )
    }

    fn answer(&mut self, id: u32, answer: DefectAnswer, at: &[i64], found: Seq) -> Seq {
        self.push(
            &[found],
            DefectAnswered {
                id: self::id(id),
                cores: cores(at),
                answer,
            },
        )
    }

    fn reset(&mut self, target: ResetTarget, cause: &[Seq]) -> Seq {
        self.push(cause, CommandReset { target })
    }

    fn state(&self, registry: &'static [Entry]) -> DefectState {
        self.prefix(registry, self.events.len())
    }

    fn prefix(&self, registry: &'static [Entry], len: usize) -> DefectState {
        let mut state = DefectState::new(registry);
        for event in &self.events[..len] {
            state.fold(event);
        }
        state
    }
}

fn pending_ids(state: &DefectState) -> Vec<u32> {
    state
        .new_findings()
        .map(|pending| pending.entry.id.get())
        .collect()
}

fn finding_ids(state: &DefectState) -> Vec<u32> {
    state.findings().map(|view| view.id.get()).collect()
}

#[test]
fn fix_watermark_strict() {
    let mut log = Log::default();
    log.start(0);
    let failure = log.failure();
    let before = log.decision(0, Step::Backoff, &[failure]);
    log.config(1);
    let at_boundary_below = log.decision(1, Step::Backoff, &[failure]);
    log.config(2);
    let fixed = log.decision(2, Step::Backoff, &[failure]);
    log.start(1);
    let reopened = log.decision(3, Step::Backoff, &[failure]);

    let state = log.state(&FIXED_LATER);
    let pending: Vec<_> = state.new_findings().collect();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].decisions,
        [before, at_boundary_below, reopened].as_slice()
    );
    assert!(!pending[0].decisions.contains(&fixed));
    assert_eq!(pending[0].cores, cores(&[0, 1, 3]));
}

#[test]
fn watermark_below_every_entry_is_affected_and_equal_is_not() {
    let mut log = Log::default();
    log.start(1);
    let failure = log.failure();
    log.decision(0, Step::Backoff, &[failure]);
    assert_eq!(pending_ids(&log.state(&PAIR)), [2]);
}

#[test]
fn finding_once_per_id() {
    let mut log = Log::default();
    log.start(0);
    let failure = log.failure();
    let first = log.decision(5, Step::Backoff, &[failure]);
    let second = log.decision(2, Step::Backoff, &[failure]);
    let third = log.decision(5, Step::Backoff, &[failure]);
    let state = log.state(&PAIR);
    let pending: Vec<_> = state.new_findings().collect();
    assert_eq!(pending_ids(&state), [1, 2]);
    assert_eq!(pending[0].decisions, [first, second, third].as_slice());
    assert_eq!(pending[0].cores, cores(&[2, 5]));

    let found = log.found(
        1,
        DefectDirection::TooCautious,
        &[2, 5],
        &[first, second, third],
    );
    let late = log.decision(7, Step::Backoff, &[failure]);
    let again = log.found(1, DefectDirection::TooAggressive, &[7], &[late]);
    let state = log.state(&PAIR);
    assert_eq!(pending_ids(&state), [2]);
    assert_eq!(state.finding_seq(id(1)), Some(found));
    assert_ne!(state.finding_seq(id(1)), Some(again));
    assert_eq!(state.finding_seq(id(2)), None);
    let views: Vec<_> = state.findings().collect();
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].direction, DefectDirection::TooCautious);
    assert_eq!(views[0].cores, cores(&[2, 5]));
}

#[test]
fn cause_kind_must_match() {
    let mut log = Log::default();
    log.start(0);
    let warning = log.warning();
    let failure = log.failure();
    log.decision(0, Step::Backoff, &[warning]);
    log.decision(1, Step::Deeper, &[failure]);
    log.decision(2, Step::Backoff, &[]);
    log.decision(3, Step::Backoff, &[Seq::new(999).unwrap()]);
    let state = log.state(&CAUTIOUS);
    assert_eq!(pending_ids(&state), Vec::<u32>::new());
    assert_eq!(state.excluded_failures().count(), 0);

    let matched = log.decision(4, Step::Backoff, &[warning, failure]);
    let state = log.state(&CAUTIOUS);
    let pending: Vec<_> = state.new_findings().collect();
    assert_eq!(pending[0].decisions, [matched].as_slice());
    assert_eq!(pending[0].cores, cores(&[4]));

    assert_eq!(pending_ids(&log.state(&NOT_TUNER)), Vec::<u32>::new());
}

#[test]
fn excluded_failures_persist_after_finding() {
    let mut log = Log::default();
    log.start(0);
    let first = log.failure();
    let second = log.failure();
    let unrelated = log.failure();
    let warning = log.warning();
    let backoff = log.decision(0, Step::Backoff, &[second, first]);
    log.decision(1, Step::Deeper, &[unrelated]);
    log.decision(2, Step::Deeper, &[warning, unrelated]);
    let state = log.state(&CAUTIOUS);
    assert_eq!(
        state.excluded_failures().collect::<Vec<_>>(),
        [first, second]
    );
    assert!(state.excludes_failure(first) && state.excludes_failure(second));
    assert!(!state.excludes_failure(unrelated) && !state.excludes_failure(warning));

    log.found(1, DefectDirection::TooCautious, &[0], &[backoff]);
    let state = log.state(&CAUTIOUS);
    assert_eq!(pending_ids(&state), Vec::<u32>::new());
    assert_eq!(
        state.excluded_failures().collect::<Vec<_>>(),
        [first, second]
    );

    let state = log.state(&ALTERNATIVES);
    assert_eq!(
        state.excluded_failures().collect::<Vec<_>>(),
        [first, second]
    );
    assert!(!state.excludes_failure(unrelated) && !state.excludes_failure(warning));
}

#[test]
fn failure_cited_beside_a_non_failure_match_is_not_excluded() {
    let mut log = Log::default();
    log.start(0);
    let warning = log.warning();
    let failure = log.failure();
    let matched = log.decision(0, Step::Deeper, &[warning, failure]);
    let state = log.state(&ALTERNATIVES);
    assert_eq!(state.new_findings().count(), 1);
    assert_eq!(state.excluded_failures().count(), 0);
    assert!(!state.excludes_failure(failure));

    let both = log.decision(1, Step::Backoff, &[warning, failure]);
    let state = log.state(&ALTERNATIVES);
    assert_eq!(state.excluded_failures().collect::<Vec<_>>(), [failure]);
    assert_eq!(
        state.new_findings().next().unwrap().decisions,
        [matched, both].as_slice()
    );
}

#[test]
fn excluded_failures_follow_fixes_filtering() {
    let mut log = Log::default();
    log.start(1);
    let failure = log.failure();
    log.decision(0, Step::Backoff, &[failure]);
    assert_eq!(log.state(&CAUTIOUS).excluded_failures().count(), 0);
    assert_eq!(log.state(&PAIR).excluded_failures().count(), 1);
    assert!(log.state(&PAIR).excludes_failure(failure));
}

#[test_case(DefectAnswer::Yes; "a yes answer")]
#[test_case(DefectAnswer::No; "a no answer")]
fn archived_answers_and_resets_resolve_no_finding(answer: DefectAnswer) {
    let mut log = Log::default();
    let first_decision = log.warning();
    let second_decision = log.warning();
    let first = log.found(1, DefectDirection::TooCautious, &[2], &[first_decision]);
    let yes = log.answer(1, answer, &[2], first);
    let second = log.found(2, DefectDirection::TooAggressive, &[3], &[second_decision]);
    log.answer(2, answer, &[3], second);
    log.reset(ResetTarget::Core(core(3)), &[yes]);
    log.reset(ResetTarget::All, &[]);
    let state = log.state(&PAIR);
    assert_eq!(finding_ids(&state), [1, 2]);
    assert_eq!(state.finding_seq(id(1)), Some(first));
    assert_eq!(state.finding_seq(id(2)), Some(second));
    let views: Vec<_> = state.findings().collect();
    assert_eq!(views[1].seq, second);
    assert_eq!(views[1].direction, DefectDirection::TooAggressive);
    assert_eq!(views[1].cores, cores(&[3]));
    assert_eq!(views[1].decisions, [second_decision]);
}

#[test]
fn findings_are_in_journal_order() {
    let mut log = Log::default();
    log.found(2, DefectDirection::TooCautious, &[1], &[]);
    log.found(1, DefectDirection::TooCautious, &[1], &[]);
    assert_eq!(finding_ids(&log.state(&PAIR)), [2, 1]);
}

fn scenario() -> Log {
    let mut log = Log::default();
    log.start(0);
    let failure = log.failure();
    log.warning();
    let first = log.decision(1, Step::Backoff, &[failure]);
    let second = log.decision(0, Step::Backoff, &[failure]);
    log.config(1);
    log.decision(2, Step::Backoff, &[failure]);
    let found = log.found(1, DefectDirection::TooCautious, &[0, 1], &[first, second]);
    let other = log.found(2, DefectDirection::TooAggressive, &[0], &[second]);
    let yes = log.answer(1, DefectAnswer::Yes, &[0, 1], found);
    log.reset(ResetTarget::Core(core(0)), &[yes]);
    log.reset(ResetTarget::Core(core(1)), &[found]);
    log.answer(2, DefectAnswer::No, &[0], other);
    log.reset(ResetTarget::All, &[]);
    log.decision(3, Step::Backoff, &[failure]);
    log
}

#[test_case(&PAIR; "pair")]
#[test_case(&CAUTIOUS; "cautious")]
#[test_case(&ALTERNATIVES; "alternatives")]
#[test_case(&NONE; "no registry")]
fn fold_prefix_equals_fresh_fold(registry: &'static [Entry]) {
    let log = scenario();
    let mut incremental = DefectState::new(registry);
    assert_eq!(incremental, log.prefix(registry, 0));
    for (at, event) in log.events.iter().enumerate() {
        incremental.fold(event);
        let fresh = log.prefix(registry, at + 1);
        assert_eq!(incremental, fresh, "prefix {}", at + 1);
        assert_eq!(pending_ids(&incremental), pending_ids(&fresh));
        assert_eq!(finding_ids(&incremental), finding_ids(&fresh));
        assert_eq!(
            incremental.excluded_failures().collect::<Vec<_>>(),
            fresh.excluded_failures().collect::<Vec<_>>()
        );
    }
}

#[test]
fn unknown_aggressive_finding_stays_recorded() {
    let mut log = Log::default();
    let found = log.found(7, DefectDirection::TooAggressive, &[3], &[]);
    let state = log.state(&NONE);
    let views: Vec<_> = state.findings().collect();
    assert_eq!(views.len(), 1);
    assert!(views[0].entry.is_none());
    assert_eq!(views[0].id, id(7));
    assert_eq!(views[0].seq, found);
    assert_eq!(views[0].direction, DefectDirection::TooAggressive);
    assert_eq!(views[0].title, "recorded");
    assert_eq!(views[0].cores, cores(&[3]));
    assert_eq!(state.finding_seq(id(7)), Some(found));

    log.answer(7, DefectAnswer::No, &[3], found);
    assert_eq!(finding_ids(&log.state(&NONE)), [7]);
}

#[test]
fn known_finding_view_carries_its_entry() {
    let mut log = Log::default();
    log.found(2, DefectDirection::TooAggressive, &[1], &[]);
    let state = log.state(&PAIR);
    let view = state.findings().next().unwrap();
    assert_eq!(view.entry.map(|entry| entry.id), Some(id(2)));
}

#[test]
fn empty_registry_fixed_stamp_zero() {
    assert_eq!(fixed_stamp(&[]), FixStamp::none());
    assert_eq!(fixed_stamp(KNOWN_DEFECTS), FixStamp::none());
    assert_eq!(fixed_stamp(&PAIR), FixStamp::through(id(2)));

    let mut log = Log::default();
    log.start(0);
    let failure = log.failure();
    log.decision(0, Step::Backoff, &[failure]);
    let state = log.state(&NONE);
    assert_eq!(state, DefectState::new(&NONE).tap_fold(&log));
    assert_eq!(state.new_findings().count(), 0);
}

trait TapFold {
    fn tap_fold(self, log: &Log) -> Self;
}

impl TapFold for DefectState {
    fn tap_fold(mut self, log: &Log) -> Self {
        for event in &log.events {
            self.fold(event);
        }
        self
    }
}

#[test]
fn default_state_uses_the_known_registry() {
    assert_eq!(DefectState::default(), DefectState::new(KNOWN_DEFECTS));
}

#[test]
fn zero_is_not_a_defect_id() {
    assert_eq!(DefectId::new(0), None);
    assert_eq!(DefectId::new(3).map(DefectId::get), Some(3));
}

#[test]
fn registry_ids_strictly_increasing() {
    assert!(is_ordered(KNOWN_DEFECTS));
    assert!(is_ordered(&NONE));
    assert!(is_ordered(&PAIR));
    let repeated = [PAIR[0], PAIR[0]];
    let descending = [PAIR[1], PAIR[0]];
    assert!(!is_ordered(&repeated));
    assert!(!is_ordered(&descending));
}

#[test]
#[should_panic(expected = "strictly increasing")]
fn state_refuses_a_disordered_registry() {
    static DESCENDING: [Entry; 2] = [
        entry(2, DefectDirection::TooCautious, &[]),
        entry(1, DefectDirection::TooCautious, &[]),
    ];
    let _ = DefectState::new(&DESCENDING);
}

/// Every query's visit count on the scenario, each run to completion and non-empty.
fn query_visits(state: &DefectState, failure: Seq) -> [u64; 5] {
    state.take_visits();
    let mut visits = [0; 5];
    let mut measure = |at: usize, returned: usize| {
        assert!(returned > 0, "query {at} returned nothing");
        visits[at] = state.take_visits();
        assert!(visits[at] > 0, "query {at} visited nothing");
    };
    measure(0, state.new_findings().count());
    measure(1, state.findings().count());
    measure(2, state.excluded_failures().count());
    measure(3, usize::from(state.excludes_failure(failure)));
    measure(4, usize::from(state.finding_seq(id(1)).is_some()));
    visits
}

/// The scenario every query answers non-empty, behind `extra` irrelevant events of each kind.
fn busy_log(extra: usize) -> (Log, Seq) {
    let mut log = Log::default();
    log.start(0);
    for _ in 0..extra {
        log.warning();
        log.failure();
        log.reset(ResetTarget::Core(core(5)), &[]);
    }
    let failure = log.failure();
    let decision = log.decision(0, Step::Backoff, &[failure]);
    log.found(1, DefectDirection::TooCautious, &[0], &[decision]);
    let aggressive = log.found(7, DefectDirection::TooAggressive, &[1], &[]);
    log.answer(7, DefectAnswer::Yes, &[1], aggressive);
    (log, failure)
}

#[test]
fn query_cost_independent_of_event_count() {
    let (small, small_failure) = busy_log(0);
    let (large, large_failure) = busy_log(5_000);
    let base = query_visits(&small.state(&PAIR), small_failure);
    let state = large.state(&PAIR);
    assert_eq!(state.retained_kinds(), 5_001);
    assert_eq!(query_visits(&state, large_failure), base);
    assert_eq!(small.state(&PAIR).retained_kinds(), 1);

    let empty = large.state(&NONE);
    assert_eq!(empty.retained_kinds(), 0);
    assert_eq!(empty, DefectState::new(&NONE).tap_fold(&large));

    let mut warnings_only = Log::default();
    for _ in 0..5_000 {
        warnings_only.warning();
    }
    assert_eq!(warnings_only.state(&CAUTIOUS).retained_kinds(), 0);
}
