//! The narration corpus of `togi status` for #904: every phrase the status renders across the simulated sessions,
//! the edge journals the read tests build and the command's diagnostics, written as one Markdown section to the
//! absolute file `TOGI_NARRATION_CORPUS` names. Ignored, so only an explicit run writes it:
//!
//! ```sh
//! TOGI_NARRATION_CORPUS=/tmp/togi904-status-before.md \
//!     cargo test --locked -p togi narration_corpus -- --ignored --nocapture
//! ```
//!
//! Every rendering goes through the status renderer itself: the sweep folds each base journal event by event and
//! writes [`Status`] from the folded states, checked against [`render`] at every scenario cut, every
//! [`CHECK_EVERY`]th prefix and each journal's end; the scenario cuts and edge journals go through [`render`]; the
//! diagnostics run the command's [`run`] over state directories on disk.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io;
use std::path::Path;

use togi_config::Config;
use togi_journal::{
    Activity, Attribution, CycleEvent, DeadEndCondition, Event, EventKind, FactSource, Failure,
    FailureCarried, HuntStart, Outcome, Payload, Seq, Stamp, State as JournalState, TrialCarried,
    TrialClass, TrialId, TrialIntent,
};
use togi_machine::{Condition, CoreSet, PerCore, Regime, Signal};
use togi_tuner::{Depth as _, GroupState};
use togi_watch::model::{HuntView, Model};
use togi_watchtest::{Base, Scenario, cut_trial, cut_when};

use super::{Args, Problem, Scan, Status, dead_end_label, render, run};
use crate::cli::Context;
use crate::system::Real;

/// The variable naming the absolute file the corpus is written to.
const ENV: &str = "TOGI_NARRATION_CORPUS";

/// How often the sweep checks its folded rendering against [`render`] of the same prefix.
const CHECK_EVERY: usize = 250;

/// The surface every item of this section belongs to, and the one form it renders in.
const SURFACE: &str = "status";
const FORM: &str = "80col";

#[test]
#[ignore = "writes the #904 narration corpus to the file TOGI_NARRATION_CORPUS names"]
fn narration_corpus() {
    let path = std::env::var_os(ENV).unwrap_or_else(|| {
        panic!("{ENV} must name the absolute file the corpus is written to; nothing was written")
    });
    let path = Path::new(&path);
    assert!(
        path.is_absolute(),
        "{ENV} must be an absolute path, not {}",
        path.display()
    );
    let mut corpus = Corpus::new();
    sweep(&mut corpus);
    let scenarios = scenarios(&mut corpus);
    let edges = edges(&mut corpus);
    let synthetics = synthetics(&mut corpus);
    let diagnostics = diagnostics();
    let document = corpus.document(&scenarios, &edges, &synthetics, &diagnostics);
    std::fs::write(path, document)
        .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

// ---------------------------------------------------------------------------------------------------------------
// Rendering

/// The states [`render`] replays, folded one event at a time so a journal's every prefix renders without replaying
/// it from the start.
struct Folding {
    model: Model,
    journal: JournalState,
    tuner: togi_tuner::State,
    broken: Option<String>,
}

impl Folding {
    fn new() -> Self {
        Self {
            model: Model::new(),
            journal: JournalState::default(),
            tuner: togi_tuner::State::new(),
            broken: None,
        }
    }

    /// Folds `event` as [`render`]'s replays do, in its order of refusals: the watch model first.
    fn fold(&mut self, event: &Event) {
        if self.broken.is_some() {
            return;
        }
        let folded = self
            .model
            .fold(event)
            .map_err(|error| error.to_string())
            .and_then(|()| self.journal.apply(event).map_err(|error| error.to_string()))
            .and_then(|()| self.tuner.fold(event).map_err(|error| error.to_string()));
        if let Err(error) = folded {
            self.broken = Some(error);
        }
    }

    /// What [`render`] gives for `events`, the events folded so far.
    fn render(&self, events: &[Event]) -> Result<String, String> {
        if let Some(broken) = &self.broken {
            return Err(broken.clone());
        }
        if self.journal.session.is_none() {
            return Err(problem_text(&Problem::NoSession));
        }
        let projection = self.tuner.project();
        let scan = Scan::of(events);
        let mut out = String::new();
        Status {
            out: &mut out,
            journal: &self.journal,
            projection: &projection,
            snap: self.model.snapshot(),
            scan: &scan,
        }
        .write();
        Ok(out)
    }
}

fn problem_text(problem: &Problem) -> String {
    match problem {
        Problem::NoSession => "no session".to_owned(),
        Problem::Replay(message) => message.clone(),
    }
}

fn rendered(events: &[Event]) -> Result<String, String> {
    render(events).map_err(|problem| problem_text(&problem))
}

const fn base_name(base: Base) -> &'static str {
    match base {
        Base::Session => "session",
        Base::Concluded => "concluded",
        Base::Unconfirmed => "unconfirmed",
        Base::LaterHunt => "later-hunt",
        Base::FirstResult => "first-result",
        Base::Probe => "probe",
        Base::Combination => "combination",
        Base::CrashedCycle => "crashed-cycle",
        Base::DeadEnd => "dead-end",
    }
}

/// Renders every prefix of every base journal: each event is a state the status can be read in.
fn sweep(corpus: &mut Corpus) {
    for base in Base::ALL {
        let events = base.events().expect("the base journal simulates");
        let name = base_name(base);
        let fixture = format!("base:{name}");
        let cuts: BTreeSet<usize> = Scenario::ALL
            .iter()
            .filter(|scenario| scenario.base() == base)
            .map(|scenario| scenario.events().expect("the scenario cuts").len())
            .collect();
        let mut folding = Folding::new();
        let mut checked = 0;
        for (at, event) in events.iter().enumerate() {
            folding.fold(event);
            let prefix = &events[..=at];
            let shown = folding.render(prefix);
            let len = at + 1;
            if len % CHECK_EVERY == 0 || len == events.len() || cuts.contains(&len) {
                assert_eq!(
                    shown,
                    rendered(prefix),
                    "{fixture}: the folded rendering of the first {len} events differs from status::render"
                );
                checked += 1;
            }
            let seq = event.seq.get();
            match shown {
                Ok(text) => corpus.record(&fixture, Some(seq), &text),
                Err(problem) => corpus.problem(&fixture, Some(seq), &problem),
            }
        }
        corpus.bases.push(BaseRun {
            name,
            events: events.len(),
            checked,
        });
    }
}

/// Renders the 18 scenario cuts through [`render`], whole.
fn scenarios(corpus: &mut Corpus) -> Vec<Whole> {
    Scenario::ALL
        .iter()
        .map(|&scenario| {
            let events = scenario.events().expect("the scenario cuts");
            let fixture = format!("scenario:{}", scenario.name());
            let shown = rendered(&events);
            match &shown {
                Ok(text) => corpus.record(&fixture, None, text),
                Err(problem) => corpus.problem(&fixture, None, problem),
            }
            Whole {
                fixture,
                recipe: format!(
                    "`Scenario::{scenario:?}` cut from `Base::{:?}`, {} events",
                    scenario.base(),
                    events.len()
                ),
                shown,
            }
        })
        .collect()
}

/// One named fixture's rendering; the document keeps its provenance and outcome, and the phrases it produced reach
/// the catalogue through [`Corpus::record`].
struct Whole {
    fixture: String,
    recipe: String,
    shown: Result<String, String>,
}

// ---------------------------------------------------------------------------------------------------------------
// Edge journals: the recipes of `tests/read/status.rs`, and the finite variants no simulated journal records.

fn next_seq(events: &[Event]) -> Seq {
    Seq::new(events.last().expect("a journal").seq.get() + 1).expect("a seq")
}

/// `events` followed by `extra` payloads, each with the causes named, on the last event's boot, a second apart. Each
/// message is the one the payload writes.
fn then(events: &[Event], extra: Vec<(Vec<Seq>, Payload)>) -> Vec<Event> {
    let mut events = events.to_vec();
    for (cause, payload) in extra {
        let last = events.last().expect("a journal");
        let stamp = Stamp {
            time: last
                .time
                .checked_add(jiff::SignedDuration::from_secs(1))
                .expect("a time"),
            mono: None,
        };
        let event = Event::new(next_seq(&events), stamp, last.boot.clone(), cause, payload);
        events.push(event);
    }
    events
}

/// `events` followed by an event of `kind` with `data`, decoded as the journal reader decodes it.
fn then_json(events: &[Event], kind: &str, data: serde_json::Value) -> Vec<Event> {
    let last = events.last().expect("a journal");
    let mut line: serde_json::Value =
        serde_json::from_str(&last.encode().expect("an event encodes")).expect("a JSON event");
    line["seq"] = (last.seq.get() + 1).into();
    line["kind"] = kind.into();
    line["msg"] = "".into();
    line["data"] = data;
    if let Some(fields) = line.as_object_mut() {
        fields.remove("cause");
        fields.remove("mono_ms");
    }
    let decoded = Event::decode(&line.to_string())
        .unwrap_or_else(|error| panic!("a {kind} event decodes: {error}"));
    then(events, vec![(vec![], decoded.payload)])
}

/// Rewrites the message of every event `change` changed to the one its payload writes now.
fn rewritten(events: &[Event], mut change: impl FnMut(&mut Payload) -> bool) -> Vec<Event> {
    let mut events = events.to_vec();
    for event in &mut events {
        if change(&mut event.payload) {
            event.msg = event.payload.message();
        }
    }
    events
}

fn first_hunt_start(events: &[Event]) -> HuntStart {
    events
        .iter()
        .find_map(|event| match &event.payload {
            Payload::HuntStart(start) => Some(start.clone()),
            _ => None,
        })
        .expect("a hunt")
}

fn last_intent(events: &[Event]) -> TrialIntent {
    events
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            Payload::TrialIntent(intent) => Some(intent.clone()),
            _ => None,
        })
        .expect("a trial intent")
}

fn session_start(events: &[Event]) -> &togi_journal::SessionStart {
    let Payload::SessionStart(start) = &events[0].payload else {
        unreachable!("a journal starts with its session");
    };
    start
}

fn fact_source(events: &[Event], trial: Option<&str>) -> FactSource {
    let start = session_start(events);
    FactSource {
        session: start.session,
        seq: Seq::new(40).expect("a seq"),
        build: start.build.clone(),
        trial: trial.map(|trial| TrialId::from(trial.to_owned())),
        time: events[0].time,
        boot: events[0].boot.clone(),
        backend: None,
    }
}

/// A carried pass of the class and profile `intent` ran.
fn carried_pass(events: &[Event], intent: &TrialIntent) -> TrialCarried {
    TrialCarried {
        source: fact_source(events, Some("origin")),
        class: TrialClass {
            regime: intent.regime,
            workload: Some(intent.workload),
            cores: intent.cores,
            duration_s: intent.duration_s,
        },
        condition: intent.condition,
        activity: None,
        rerun: false,
        profile: intent.profile,
        outcome: Outcome::Pass,
        signal: None,
        duration_s: intent.duration_s,
        core: None,
        stalled_core: None,
        voltage_requests_v: PerCore::default(),
        top_requesters: CoreSet::default(),
        ccd_mhz: BTreeMap::new(),
    }
}

/// A carried unattributed crash while idle, with no trial, against `profile`.
fn carried_idle_crash(
    events: &[Event],
    cores: CoreSet,
    profile: togi_machine::PerCore<togi_machine::TargetOffset>,
) -> FailureCarried {
    FailureCarried {
        source: fact_source(events, None),
        class: TrialClass {
            regime: Regime::R6,
            workload: None,
            cores,
            duration_s: 0,
        },
        failure: Failure {
            signal: Signal::Crash,
            attribution: Attribution::Unattributed,
            core: None,
            offset: None,
            trial: None,
            regime: Some(Regime::R6),
            condition: Some(Condition::Together),
            profile: Some(profile),
            known_failure: None,
            reason: None,
            round: None,
        },
    }
}

/// A session in which a hunt opens on a carried fact: a carried trial failure that ended with `signal`, or a carried
/// idle crash.
fn hunt_from_a_carried_fact(trial: bool, signal: Option<Signal>) -> Vec<Event> {
    let base = Scenario::Between.events().expect("the scenario cuts");
    let mut start = first_hunt_start(Base::Session.events().expect("the base simulates"));
    let fact: Payload = if trial {
        let mut fact = carried_pass(&base, &last_intent(&base));
        fact.class.regime = start.regime;
        fact.class.workload = Some(start.workload);
        fact.class.cores = start.cores;
        fact.condition = Condition::Together;
        fact.profile = start.failing;
        fact.outcome = Outcome::Failure;
        fact.signal = signal;
        start.trial = Some(TrialId::from("origin".to_owned()));
        fact.into()
    } else {
        start.regime = Regime::R6;
        start.workload = first_hunt_start(Base::Probe.events().expect("the base simulates")).workload;
        start.trial = None;
        carried_idle_crash(&base, start.cores, start.failing).into()
    };
    let seq = next_seq(&base);
    start.failure = seq;
    then(
        &base,
        vec![(vec![], fact), (vec![seq], Payload::HuntStart(start))],
    )
}

fn defect_found(events: &[Event], direction: &str) -> Vec<Event> {
    then_json(
        events,
        "defect.found",
        serde_json::json!({
            "id": 1, "title": "step too cautious", "detail": "why", "pr": 7,
            "direction": direction, "cores": [3, 5], "decisions": [4, 6],
        }),
    )
}

fn reset(events: &[Event], target: &serde_json::Value) -> Vec<Event> {
    then_json(events, "command.reset", serde_json::json!({ "target": target }))
}

fn carried_session(events: &[Event], data: impl FnOnce(serde_json::Value) -> serde_json::Value) -> Vec<Event> {
    let start = session_start(events);
    let source = serde_json::json!({
        "session": start.session, "schema": start.build.schema, "ruleset": start.build.ruleset,
    });
    then_json(events, "session.carried", data(source))
}

fn short_cycle() -> Vec<Event> {
    let config = Config::parse(r#"{"checking":{"cycle":["R1","R2"]}}"#).expect("the config parses");
    let machine = togi_watchtest::default_machine(1).expect("the machine builds");
    let mut checking = false;
    let stop_at_the_first_checking_trial: togi_watchtest::Until = Box::new(move |event| {
        match &event.payload {
            Payload::TrialIntent(intent) => checking = intent.activity == Some(Activity::Checking),
            Payload::TrialStart(_) => return checking,
            _ => {}
        }
        false
    });
    let events = togi_watchtest::simulate(
        machine,
        config,
        Some(stop_at_the_first_checking_trial),
        togi_watchtest::Stop::cycles(1),
    )
    .expect("the short cycle simulates");
    cut_trial(&events, |intent| intent.activity == Some(Activity::Checking))
        .expect("a checking trial")
        .to_vec()
}

fn hunt_parked_at_a_cycle_end() -> Vec<Event> {
    let base = Base::LaterHunt.events().expect("the base simulates");
    let mut events = cut_when(base, |state, event| {
        event.kind() == EventKind::TrialStart && state.hunt_plan().is_some_and(|plan| plan.number == 4)
    })
    .expect("hunt 4 starts a trial")
    .to_vec();
    let cycle_end = events
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            Payload::CheckingCycle(cycle)
                if cycle.event == CycleEvent::End && cycle.passed && cycle.full =>
            {
                Some(event.seq)
            }
            _ => None,
        })
        .expect("a passed full cycle");
    events = rewritten(&events, |payload| match payload {
        Payload::HuntStart(start) if start.hunt.get() == 4 => {
            start.parked_seq = Some(cycle_end);
            true
        }
        _ => false,
    });
    events
}

/// The edge journals: the recipes `tests/read/status.rs` pins, and the finite variants no simulated journal reaches.
fn edge_journals() -> Vec<(String, String, Vec<Event>)> {
    let search = Scenario::Search.events().expect("the scenario cuts");
    let between = Scenario::Between.events().expect("the scenario cuts");
    let mut edges: Vec<(String, String, Vec<Event>)> = Vec::new();
    let mut add = |name: &str, recipe: &str, events: Vec<Event>| {
        edges.push((format!("edge:{name}"), recipe.to_owned(), events));
    };

    let found = defect_found(&search, "too_cautious");
    let answered = then_json(
        &found,
        "defect.answered",
        serde_json::json!({"id": 1, "cores": [3, 5], "answer": "no"}),
    );
    let one = reset(&answered, &serde_json::json!({"core": 3}));
    let both = reset(&one, &serde_json::json!({"core": 5}));
    add("defect-found", "search + a too-cautious defect.found on cores 3 5", found.clone());
    add("defect-answered", "defect-found + defect.answered no", answered);
    add("defect-reset-one-core", "defect-answered + command.reset core 3", one);
    add("defect-reset-each-core", "defect-answered + command.reset cores 3 then 5", both);
    add(
        "defect-reset-all",
        "defect-found + command.reset all",
        reset(&found, &serde_json::json!("all")),
    );
    add(
        "defect-too-aggressive",
        "search + a too-aggressive defect.found on cores 3 5",
        defect_found(&search, "too_aggressive"),
    );

    for (name, corrected) in [("mce-corrected", true), ("mce-uncorrected", false)] {
        add(
            &format!("between-trial-{name}"),
            "search + an mce between trials",
            then_json(
                &search,
                "mce",
                serde_json::json!({
                    "cpu": 3, "bank": 1, "bank_type": "load_store", "corrected": corrected,
                    "between_trials": true, "lines": [],
                }),
            ),
        );
    }
    add(
        "bios-context",
        "search + session.context",
        then_json(
            &search,
            "session.context",
            serde_json::json!({
                "bios_version": "1.2.3", "board": "board", "cpu_model": "AMD Ryzen 9 9950X",
                "microcode": "0xb404023", "boost_limit_mhz": 5700,
            }),
        ),
    );
    add(
        "session-carried-failure-points",
        "search + session.carried with failure points",
        carried_session(&search, |source| {
            serde_json::json!({"sources": [source], "failure_points": true})
        }),
    );
    add(
        "session-carried-points-behind-reason",
        "search + session.carried leaving failure points behind, with a reason",
        carried_session(&search, |source| {
            serde_json::json!({
                "sources": [source], "failure_points": false, "detail": "the evidence epoch changed",
            })
        }),
    );
    add(
        "session-carried-points-behind-no-reason",
        "search + session.carried leaving failure points behind, no reason",
        carried_session(&search, |source| {
            serde_json::json!({"sources": [source], "failure_points": false})
        }),
    );

    let intent = last_intent(&between);
    add(
        "carried-r7-evidence",
        "between + a carried R7 pass and a carried idle crash: self-sufficiency without live exposure",
        then(
            &between,
            vec![
                (vec![], carried_pass(&between, &intent).into()),
                (
                    vec![],
                    carried_idle_crash(&between, intent.cores, intent.profile).into(),
                ),
            ],
        ),
    );
    add(
        "r7-without-request-telemetry",
        "between with every trial.end's per-core voltage requests removed",
        rewritten(&between, |payload| match payload {
            Payload::TrialEnd(end) => {
                end.voltage_requests_v = PerCore::default();
                true
            }
            _ => false,
        }),
    );
    add(
        "evidence-without-tctl",
        "between with every trial.end's Tctl maximum removed",
        rewritten(&between, |payload| match payload {
            Payload::TrialEnd(end) => {
                end.tctl_max_c = None;
                true
            }
            _ => false,
        }),
    );
    add(
        "queued-reset",
        "stopped + command.reset core 3",
        reset(
            &Scenario::Stopped.events().expect("the scenario cuts"),
            &serde_json::json!({"core": 3}),
        ),
    );
    add(
        "hunt-carried-trial",
        "between + a carried failed trial + the hunt it opens",
        hunt_from_a_carried_fact(true, Some(Signal::ComputationError)),
    );
    add(
        "hunt-carried-idle",
        "between + a carried idle crash + the hunt it opens",
        hunt_from_a_carried_fact(false, None),
    );
    add(
        "hunt-carried-trial-no-signal",
        "between + a carried failed trial that recorded no signal + the hunt it opens",
        hunt_from_a_carried_fact(true, None),
    );
    add(
        "hunt-located-held",
        "the first hunt trial of Base::Unconfirmed: loaded cores held at failing offsets",
        cut_trial(
            Base::Unconfirmed.events().expect("the base simulates"),
            |intent| intent.activity == Some(Activity::Hunt),
        )
        .expect("a hunt trial")
        .to_vec(),
    );
    add(
        "hunt-parked-cycle-end",
        "Base::LaterHunt to hunt 4's first trial, hunt 4 parked from the last passed full cycle",
        hunt_parked_at_a_cycle_end(),
    );
    add(
        "round-check-completed",
        "Base::Combination cut when one phase 2 round check has its passes",
        cut_when(
            Base::Combination.events().expect("the base simulates"),
            |state, event| {
                event.kind() == EventKind::TrialEnd
                    && state.project().deepening.is_some_and(|round| {
                        round
                            .checks
                            .iter()
                            .filter(|check| check.passed >= check.needed)
                            .count()
                            == 1
                    })
            },
        )
        .expect("a completed check")
        .to_vec(),
    );
    add(
        "short-cycle",
        "config checking.cycle [R1, R2], cut at the first checking trial",
        short_cycle(),
    );
    add(
        "combination-fallback",
        "combination with its combination event marked fallback",
        rewritten(
            &Scenario::Combination.events().expect("the scenario cuts"),
            |payload| match payload {
                Payload::Combination(combination) => {
                    combination.fallback = true;
                    true
                }
                _ => false,
            },
        ),
    );
    let hunt = Scenario::Hunt.events().expect("the scenario cuts");
    let hunt_intent = last_intent(&hunt);
    add(
        "hunt-excess-pass-evidence",
        "hunt + 7 carried passes of the running group's class",
        then(
            &hunt,
            (0..7)
                .map(|_| (vec![], carried_pass(&hunt, &hunt_intent).into()))
                .collect(),
        ),
    );
    add(
        "no-plan",
        "Base::Session cut when no core searches and no hunt, round or cycle plan is open",
        cut_when(
            Base::Session.events().expect("the base simulates"),
            |state, _| {
                let projection = state.project();
                projection
                    .cores
                    .iter()
                    .all(|core| core.phase != Some(togi_journal::CorePhase::Search))
                    && projection.hunt_plan.is_none()
                    && projection.deepening.is_none()
                    && projection.cycle_plan.steps.is_empty()
            },
        )
        .expect("a state with no plan")
        .to_vec(),
    );
    let deadend = Scenario::Deadend.events().expect("the scenario cuts");
    for &condition in DeadEndCondition::ALL {
        add(
            &format!("dead-end-{}", condition.as_str()),
            "deadend with its deadend event's condition replaced",
            rewritten(&deadend, |payload| match payload {
                Payload::DeadEnd(dead_end) => {
                    dead_end.condition = condition;
                    true
                }
                _ => false,
            }),
        );
    }
    let first_result = Scenario::FirstResult.events().expect("the scenario cuts");
    add(
        "solo-limits-three-counts-deep",
        "first-result with the pass of every search transition two counts deeper and its failure point one beyond: each candidate's solo limit is three counts below its checking offset",
        rewritten(&first_result, |payload| match payload {
            Payload::CorePhase(transition)
                if transition.from == Some(togi_journal::CorePhase::Search)
                    && transition.to != togi_journal::CorePhase::Search =>
            {
                let Some(pass) = transition.pass else {
                    return false;
                };
                let deeper = pass.deeper_or_floor(2);
                transition.pass = Some(deeper);
                if transition.failure_point.is_some() {
                    transition.failure_point = Some(deeper.deeper_or_floor(1));
                }
                true
            }
            _ => false,
        }),
    );
    let member_probe = Scenario::MemberProbe.events().expect("the scenario cuts");
    let mut skipped = false;
    add(
        "hunt-group-skipped",
        "member-probe with its first non-probe hunt.group recorded as skipped",
        rewritten(&member_probe, |payload| match payload {
            Payload::HuntGroup(group) if !skipped && group.probe.is_none() => {
                skipped = true;
                group.skipped = true;
                true
            }
            _ => false,
        }),
    );
    edges
}

fn edges(corpus: &mut Corpus) -> Vec<Whole> {
    edge_journals()
        .into_iter()
        .map(|(fixture, recipe, events)| {
            let shown = rendered(&events);
            match &shown {
                Ok(text) => corpus.record(&fixture, None, text),
                Err(problem) => corpus.problem(&fixture, None, problem),
            }
            Whole {
                fixture,
                recipe,
                shown,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------------------------
// Synthetic renderer cases: finite template branches no journal the command can read reaches, or that the
// simulator's journals happen not to reach, shown by handing the status writer an altered projection or snapshot.
// These are NOT states of a session; the manifest keeps them apart from the journal cases.

/// What one synthetic case changes in what [`Status::write`] is handed.
#[derive(Clone, Copy)]
enum Synth {
    NoPhasePlan,
    ManyRounds,
    NoConfirmedBios,
    NoSoloLimit,
    NoHuntSignal,
    SkippedGroup,
}

fn synthetic_render(events: &[Event], synth: Synth) -> Result<String, String> {
    let model = togi_watch::model::project(events);
    if let Some(problem) = model.problem() {
        return Err(problem.to_string());
    }
    let journal = JournalState::replay(events).map_err(|error| error.to_string())?;
    let tuner = togi_tuner::replay(events).map_err(|error| error.to_string())?;
    let mut projection = tuner.project();
    let scan = Scan::of(events);
    let base = model.snapshot();
    let mut hunt = base.hunt.map(|view| (view.plan.clone(), view.facts.clone()));
    let mut snap = base;
    match synth {
        Synth::NoPhasePlan => projection.phase_plan = None,
        Synth::ManyRounds => {
            if let Some(plan) = projection.phase_plan.as_mut() {
                plan.rounds_left = 3;
            }
        }
        Synth::NoConfirmedBios => {
            if let Some(bios) = projection.bios_profile.as_mut() {
                bios.confirmed = None;
                bios.unconfirmed.clear();
            }
        }
        Synth::NoSoloLimit => {
            if let Some(plan) = projection.phase_plan.as_mut() {
                for candidate in &mut plan.candidates {
                    candidate.solo_limit = None;
                }
            }
        }
        Synth::NoHuntSignal => {
            if let Some((_, facts)) = hunt.as_mut() {
                facts.cause.signal = None;
            }
        }
        Synth::SkippedGroup => {
            if let Some((plan, _)) = hunt.as_mut() {
                if let Some(group) = plan.groups.first_mut() {
                    group.outcome = GroupState::Skipped;
                }
            }
        }
    }
    if let Some((plan, facts)) = &hunt {
        snap.hunt = Some(HuntView { plan, facts });
    }
    let mut out = String::new();
    Status {
        out: &mut out,
        journal: &journal,
        projection: &projection,
        snap,
        scan: &scan,
    }
    .write();
    Ok(out)
}

fn synthetics(corpus: &mut Corpus) -> Vec<Whole> {
    let round = Scenario::Round.events().expect("the scenario cuts");
    let cases: [(&str, &str, Synth, Vec<Event>); 6] = [
        (
            "no-phase-plan",
            "search, with the projection's phase plan removed: the header carries no phase clause",
            Synth::NoPhasePlan,
            Scenario::Search.events().expect("the scenario cuts"),
        ),
        (
            "rounds-left-three",
            "round, with the phase plan's rounds left set to 3: the header clause and the worst-case line say rounds",
            Synth::ManyRounds,
            round.clone(),
        ),
        (
            "no-confirmed-bios-profile-in-phase-2",
            "round, with the projection's BIOS profile left unconfirmed in phase 2: no BIOS note",
            Synth::NoConfirmedBios,
            round.clone(),
        ),
        (
            "candidate-without-solo-limit",
            "round, with every candidate's solo limit removed: the SOLO cell shows -",
            Synth::NoSoloLimit,
            round,
        ),
        (
            "hunt-cause-without-signal",
            "hunt, with the hunt cause's signal removed: the hunt line says failure",
            Synth::NoHuntSignal,
            Scenario::Hunt.events().expect("the scenario cuts"),
        ),
        (
            "hunt-group-skipped",
            "member-probe, with the first hunt group's outcome set to skipped",
            Synth::SkippedGroup,
            Scenario::MemberProbe.events().expect("the scenario cuts"),
        ),
    ];
    cases
        .into_iter()
        .map(|(name, recipe, synth, events)| {
            let fixture = format!("synthetic:{name}");
            let shown = synthetic_render(&events, synth);
            match &shown {
                Ok(text) => corpus.record(&fixture, None, text),
                Err(problem) => corpus.problem(&fixture, None, problem),
            }
            Whole {
                fixture,
                recipe: format!("SYNTHETIC renderer input, not a journal state: {recipe}"),
                shown,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------------------------
// Diagnostics: the command itself over state directories on disk.

struct Diagnostic {
    fixture: &'static str,
    source: &'static str,
    recipe: &'static str,
    expect: &'static str,
    exit: String,
    stdout: String,
    stderr: String,
}

/// A writer that fails every write as a full disk does.
struct Full;

impl io::Write for Full {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::from_raw_os_error(28))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn journal(dir: &Path) -> std::path::PathBuf {
    dir.join("events.jsonl")
}

fn lines(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(journal(dir))
        .expect("the journal reads")
        .lines()
        .map(str::to_owned)
        .collect()
}

fn write_lines(dir: &Path, lines: &[String]) {
    let text = lines.iter().fold(String::new(), |mut text, line| {
        text.push_str(line);
        text.push('\n');
        text
    });
    std::fs::write(journal(dir), text).expect("the journal writes");
}

fn rewrite_first_line(dir: &Path, change: impl FnOnce(&mut serde_json::Value)) {
    let mut lines = lines(dir);
    let mut first: serde_json::Value = serde_json::from_str(&lines[0]).expect("a JSON line");
    change(&mut first);
    lines[0] = first.to_string();
    write_lines(dir, &lines);
}

/// An event line of `kind` after `last`, on its boot.
fn line_after(last: &str, seq: u64, kind: &str, data: &serde_json::Value) -> String {
    let last: serde_json::Value = serde_json::from_str(last).expect("a JSON line");
    serde_json::json!({
        "seq": seq, "time": "2026-02-01T00:00:00Z", "boot": last["boot"],
        "kind": kind, "msg": "x", "data": data,
    })
    .to_string()
}

fn installed(scenario: Scenario) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temporary directory");
    togi_watchtest::install(dir.path(), scenario).expect("the scenario installs");
    dir
}

/// Strips the SGR colouring the notices take when stderr is a terminal.
fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("\u{1b}[") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        match after.find('m') {
            Some(end) => rest = &after[end + 1..],
            None => {
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn run_status(dir: &Path, failing_stdout: bool) -> (String, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut full = Full;
    let system = Real;
    let exit = {
        let stdout: &mut dyn io::Write = if failing_stdout { &mut full } else { &mut out };
        let mut ctx = Context {
            state_dir: dir,
            out: stdout,
            err: &mut err,
            system: &system,
        };
        run(&Args {}, &mut ctx)
    };
    let shown = dir.display().to_string();
    let clean = |bytes: &[u8]| plain(&String::from_utf8_lossy(bytes)).replace(&shown, "<state-dir>");
    (format!("{exit:?}"), clean(&out), clean(&err))
}

fn diagnostics() -> Vec<Diagnostic> {
    let mut found = Vec::new();
    let mut add = |fixture: &'static str,
                   source: &'static str,
                   recipe: &'static str,
                   expect: &'static str,
                   dir: &Path,
                   failing: bool| {
        let (exit, stdout, stderr) = run_status(dir, failing);
        found.push(Diagnostic {
            fixture,
            source,
            recipe,
            expect,
            exit,
            stdout,
            stderr,
        });
    };

    let missing = tempfile::tempdir().expect("a temporary directory");
    add(
        "diag:missing-journal",
        "read.rs:63-66",
        "an empty state directory",
        "no journal at",
        missing.path(),
        false,
    );

    let empty = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(journal(empty.path()), "").expect("the journal writes");
    add(
        "diag:no-session",
        "status.rs:67-70",
        "an empty journal",
        "no session in",
        empty.path(),
        false,
    );

    let rejected = installed(Scenario::Between);
    rewrite_first_line(rejected.path(), |start| {
        if let Some(cores) = start["data"]["cores"].as_array_mut() {
            cores.pop();
        }
    });
    add(
        "diag:fold-rejects",
        "status.rs:71-73, 165-171",
        "between with one core dropped from session.start",
        "does not have",
        rejected.path(),
        false,
    );

    let skipped = installed(Scenario::Search);
    let mut kept = lines(skipped.path());
    let last = kept.pop().expect("a journal");
    let unknown_seq = kept.len() as u64 + 1;
    kept.push(line_after(&last, unknown_seq, "future.thing", &serde_json::json!({})));
    let mut moved: serde_json::Value = serde_json::from_str(&last).expect("a JSON line");
    moved["seq"] = (unknown_seq + 1).into();
    kept.push(moved.to_string());
    kept.push(line_after(
        &last,
        unknown_seq + 2,
        "session.warning",
        &serde_json::json!({"operation": "write state projection", "error": "disk full", "extra": 1}),
    ));
    write_lines(skipped.path(), &kept);
    add(
        "diag:skipped-unknown-kind-and-field",
        "read.rs:74-79",
        "search with an unknown kind before its last event and a session.warning with an unknown field",
        "skipped 1 event",
        skipped.path(),
        false,
    );

    let first = installed(Scenario::Search);
    rewrite_first_line(first.path(), |event| event["kind"] = "future.thing".into());
    add(
        "diag:unknown-first-kind",
        "read.rs:42-45",
        "search whose first line has an unknown kind",
        "togi status: ",
        first.path(),
        false,
    );

    let newer = installed(Scenario::Search);
    rewrite_first_line(newer.path(), |event| {
        event["data"]["build"]["schema"] = 99.into();
    });
    add(
        "diag:schema-newer",
        "read.rs:47-51",
        "search recorded by schema 99",
        "schema 99",
        newer.path(),
        false,
    );

    let older = installed(Scenario::Search);
    rewrite_first_line(older.path(), |event| {
        event["data"]["build"]["schema"] = 0.into();
    });
    add(
        "diag:schema-older",
        "read.rs:47-51",
        "search recorded by schema 0",
        "uses schema",
        older.path(),
        false,
    );

    let ruleset = installed(Scenario::Search);
    rewrite_first_line(ruleset.path(), |event| {
        event["data"]["build"]["ruleset"] = 7.into();
    });
    add(
        "diag:ruleset-differs",
        "read.rs:52-58",
        "search recorded by ruleset 7",
        "ruleset is 7",
        ruleset.path(),
        false,
    );

    let torn = installed(Scenario::Search);
    let mut text = std::fs::read_to_string(journal(torn.path())).expect("the journal reads");
    text.push_str("{\"seq\":");
    std::fs::write(journal(torn.path()), text).expect("the journal writes");
    add(
        "diag:torn-tail",
        "read.rs:80-86",
        "search with 7 torn bytes appended",
        "torn bytes",
        torn.path(),
        false,
    );

    let one = installed(Scenario::Search);
    let mut text = std::fs::read_to_string(journal(one.path())).expect("the journal reads");
    text.push('{');
    std::fs::write(journal(one.path()), text).expect("the journal writes");
    add(
        "diag:torn-tail-one-byte",
        "read.rs:80-87",
        "search with 1 torn byte appended",
        "1 torn byte;",
        one.path(),
        false,
    );

    let full = installed(Scenario::Search);
    add(
        "diag:write-fails",
        "status.rs:75-81",
        "search, with stdout failing every write as a full disk does",
        "write output: ",
        full.path(),
        true,
    );
    found
}

// ---------------------------------------------------------------------------------------------------------------
// Parsing a rendering into blocks: a line that starts at column 0, with the indented lines under it.

#[derive(Clone, Copy, Eq, PartialEq)]
enum Table {
    Cores,
    R7,
    Groups,
    Candidates,
    Checks,
    Regimes,
}

impl Table {
    fn of_header(line: &str) -> Option<Self> {
        Some(if line.starts_with("CORE ") && line.contains("SELF-SUFFICIENT") {
            Self::R7
        } else if line.starts_with("CORE ") {
            Self::Cores
        } else if line.starts_with("GROUP ") {
            Self::Groups
        } else if line.starts_with("CANDIDATE ") {
            Self::Candidates
        } else if line.starts_with("CHECK ") {
            Self::Checks
        } else if line.starts_with("REGIME ") {
            Self::Regimes
        } else {
            return None;
        })
    }

    const fn columns(self) -> &'static [&'static str] {
        match self {
            Self::Cores => &[
                "CORE", "CCD", "SLOT", "OFFSET", "BIOS", "PHASE", "FAILURE", "QUEUED",
            ],
            Self::R7 => &["CORE", "CCD", "SELF-SUFFICIENT", "TOP REQUESTER"],
            Self::Groups => &["GROUP", "CORES", "OUTCOME", "PASS EVIDENCE", "NEED"],
            Self::Candidates => &["CANDIDATE", "OFFSET", "SOLO", "GAP", "STATE"],
            Self::Checks => &["CHECK", "CORES", "PASSES"],
            Self::Regimes => &["REGIME", "WORKLOAD", "VALID PASSES"],
        }
    }

    const fn header_family(self) -> &'static str {
        match self {
            Self::Cores => "cores.header",
            Self::R7 => "r7.header",
            Self::Groups => "groups.header",
            Self::Candidates => "candidates.header",
            Self::Checks => "checks.header",
            Self::Regimes => "evidence.header",
        }
    }

    const fn row_family(self) -> &'static str {
        match self {
            Self::Cores => "cores.row",
            Self::R7 => "r7.row",
            Self::Groups => "groups.row",
            Self::Candidates => "candidates.row",
            Self::Checks => "checks.row",
            Self::Regimes => "evidence.row",
        }
    }
}

/// The block families in the order the status writes them, with the source that writes each.
const FAMILIES: &[(&str, &str)] = &[
    ("header", "status.rs:291-351 header, phase_clause"),
    ("session", "status.rs:240-245"),
    ("bios.context", "status.rs:246-256"),
    ("in-flight", "status.rs:257-263, the in-flight event's msg"),
    ("dead-end", "status.rs:264-271"),
    ("carried", "status.rs:272-275, the session.carried msg"),
    ("cores.header", "status.rs:367-373"),
    ("cores.row", "status.rs:374-408, notes: the last decision's msg"),
    ("bios.note", "status.rs:412-437"),
    ("combinations", "status.rs:439-467"),
    ("r7.heading", "status.rs:469-479"),
    ("r7.header", "status.rs:480-482"),
    ("r7.row", "status.rs:483-502"),
    ("between", "status.rs:281-284, the mce msg"),
    ("hunt", "status.rs:506-543"),
    ("groups.header", "status.rs:544-546"),
    ("groups.row", "status.rs:547-581"),
    ("phase2.round", "status.rs:588-591"),
    ("candidates.header", "status.rs:594-596"),
    ("candidates.row", "status.rs:597-612"),
    ("phase2.remaining", "status.rs:616-624"),
    ("checks.header", "status.rs:627"),
    ("checks.row", "status.rs:628-638"),
    ("defect", "status.rs:643-666"),
    ("evidence.heading", "status.rs:332, 744"),
    ("evidence.header", "status.rs:745-747"),
    ("evidence.row", "status.rs:675-681"),
    ("evidence.tctl", "status.rs:683-689"),
    ("unclassified", "no family: a line the corpus parser does not place"),
];

/// Prose lines by their opening words, tried before a table row so a line right under a table is not taken for one.
const PROSE: &[(&str, &str)] = &[
    ("session ", "session"),
    ("BIOS profile", "bios.note"),
    ("* unconfirmed since", "bios.note"),
    ("Valid passes supporting", "evidence.heading"),
    ("BIOS ", "bios.context"),
    ("in flight: ", "in-flight"),
    ("dead end: ", "dead-end"),
    ("carried: ", "carried"),
    ("combinations:", "combinations"),
    ("R7 self-sufficiency", "r7.heading"),
    ("between-trial evidence", "between"),
    ("hunt ", "hunt"),
    ("phase 2 round ", "phase2.round"),
    ("no core can move", "phase2.remaining"),
    ("worst case if no round fails", "phase2.remaining"),
    ("defect ", "defect"),
    ("Tctl max since", "evidence.tctl"),
];

struct Block {
    family: &'static str,
    lines: Vec<String>,
    /// A table row's cells, cut at its header's column starts.
    cells: Vec<String>,
    flat: String,
}

impl Block {
    fn cell(&self, at: usize) -> &str {
        self.cells.get(at).map_or("", String::as_str)
    }

    /// Whether a note line under the row starts with `label`.
    fn note(&self, label: &str) -> bool {
        self.lines
            .iter()
            .skip(1)
            .any(|line| line.trim_start().starts_with(label))
    }
}

fn blocks(text: &str) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut table: Option<(Table, Vec<usize>)> = None;
    for line in text.lines() {
        if line.is_empty() {
            table = None;
            continue;
        }
        if line.starts_with(' ') {
            if let Some(block) = blocks.last_mut() {
                block.lines.push(line.to_owned());
                continue;
            }
        }
        let mut cells = Vec::new();
        let family = if blocks.is_empty() {
            "header"
        } else if let Some(kind) = Table::of_header(line) {
            let starts = kind
                .columns()
                .iter()
                .map(|name| line.find(name).unwrap_or(0))
                .collect();
            table = Some((kind, starts));
            kind.header_family()
        } else if let Some(&(_, family)) = PROSE.iter().find(|(prefix, _)| line.starts_with(prefix)) {
            family
        } else if let Some((kind, starts)) = &table {
            cells = starts
                .iter()
                .enumerate()
                .map(|(at, &start)| {
                    let end = starts.get(at + 1).copied().unwrap_or(line.len());
                    line.get(start..end.max(start)).unwrap_or("").trim().to_owned()
                })
                .collect();
            kind.row_family()
        } else {
            "unclassified"
        };
        blocks.push(Block {
            family,
            lines: vec![line.to_owned()],
            cells,
            flat: String::new(),
        });
    }
    for block in &mut blocks {
        block.flat = block
            .lines
            .iter()
            .flat_map(|line| line.split_whitespace())
            .collect::<Vec<_>>()
            .join(" ");
    }
    blocks
}

/// The block's text with every number as `N` and every run of bare numbers, such as a core list, as one `N…`: the
/// key that keeps one representative per template.
fn template(flat: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    for word in flat.split_whitespace() {
        let mut out = String::new();
        let mut digits = false;
        for c in word.chars() {
            if c.is_ascii_digit() {
                if !digits {
                    out.push('N');
                }
                digits = true;
            } else {
                out.push(c);
                digits = false;
            }
        }
        match words.last_mut() {
            Some(last) if out == "N" && (last == "N" || last == "N…") => "N…".clone_into(last),
            _ => words.push(out),
        }
    }
    words.join(" ")
}

// ---------------------------------------------------------------------------------------------------------------
// The source-branch manifest: what an executed rendering shows for each branch of the status writer.

type Test = Box<dyn Fn(&[Block]) -> bool>;

struct Branch {
    id: String,
    source: &'static str,
    what: String,
    /// The one fixture whose rendering proves a silent branch: one that writes nothing.
    only: Option<&'static str>,
    /// Why no journal the command can read reaches the branch, when source reading shows it cannot.
    unreachable: Option<&'static str>,
    test: Test,
}

fn any(blocks: &[Block], family: &str, test: impl Fn(&Block) -> bool) -> bool {
    blocks
        .iter()
        .any(|block| block.family == family && test(block))
}

fn header(blocks: &[Block]) -> &str {
    blocks.first().map_or("", |block| block.flat.as_str())
}

fn number(text: &str) -> Option<u32> {
    text.trim().parse().ok()
}

/// The `passed/needed` of a cell or a header's `checks a/b`.
fn fraction(text: &str) -> Option<(u32, u32)> {
    let (passed, needed) = text.split_once('/')?;
    Some((number(passed)?, number(needed.split(|c: char| !c.is_ascii_digit()).next()?)?))
}

fn branch(id: &str, source: &'static str, what: &str, test: impl Fn(&[Block]) -> bool + 'static) -> Branch {
    Branch {
        id: id.to_owned(),
        source,
        what: what.to_owned(),
        only: None,
        unreachable: None,
        test: Box::new(test),
    }
}

fn silent(id: &str, source: &'static str, what: &str, fixture: &'static str, test: impl Fn(&[Block]) -> bool + 'static) -> Branch {
    Branch {
        only: Some(fixture),
        ..branch(id, source, what, test)
    }
}

impl Branch {
    #[must_use]
    fn unreachable(mut self, why: &'static str) -> Self {
        self.unreachable = Some(why);
        self
    }
}

const WHY_NO_PHASE_PLAN: &str = "`phase_plan()` is `None` only when the tuner state is poisoned or has no run (`togi-tuner/src/phase_plan.rs:159-165`). The run exists from `session.start` (`state.rs:290`), which `render` requires before it writes anything (`status.rs:173-175`), and a poisoned tuner fails `togi_tuner::replay`, which `render` turns into an error before any section (`status.rs:170-171`). So a rendered session always has a phase plan and the header's `?` never returns `None`.";

const WHY_BIOS_SILENT: &str = "The silent branch needs a phase 2 or concluded phase plan with no confirmed BIOS profile. Phase 1 ends only at `Phases::passed_full_cycle` (`phases.rs:203-220`), which errors without a checking profile, and `Later::full_cycle` confirms that same checking profile in the same fold, right after, with `settled` true (`checking/mod.rs:385-393`, `later/mod.rs:171-181, 474-480`). Both are cleared together when a core re-enters search (`cores.rs:180-184`, `later/mod.rs:66-68`, `phases.rs:58-60`). So outside phase 1 a confirmed profile always exists.";

const WHY_SOLO_NONE: &str = "A candidate row exists only for a core with a gap or a carried move (`phase_plan.rs:134-138`). A gap counts a move of `phase2_moves`, which needs a solo limit and an offset shallower than it (`phases.rs:276-281`); a carried core is one an open round has already moved (`phase_plan.rs:63-74`) or whose offset is deeper than the last passed profile (`phases.rs:269-275`), and only a core with a solo limit moves deeper in phase 2. A solo limit is cleared only when a core re-enters search (`cores.rs:166-168`), which also resets every round (`cores.rs:180-184`). By source reading, every candidate row therefore has a solo limit.";

fn cell_branch(
    id: &str,
    source: &'static str,
    what: &str,
    family: &'static str,
    test: impl Fn(&Block) -> bool + 'static,
) -> Branch {
    branch(id, source, what, move |blocks| any(blocks, family, &test))
}

fn branches() -> Vec<Branch> {
    let mut all = vec![
        branch("header.search", "status.rs:search_left", "search: N cores left", |b| header(b).starts_with("search: ") && header(b).contains(" cores left")),
        branch("header.search-one", "status.rs:search_left", "search: 1 core left", |b| header(b).starts_with("search: 1 core left")),
        branch("header.hunt", "status.rs:301-303", "hunt N, group N", |b| {
            header(b).starts_with("hunt ")
        }),
        branch("header.round", "status.rs:304-314", "phase 2 round N, checks N/N", |b| {
            header(b).starts_with("phase 2 round ")
        }),
        branch(
            "header.round-check-done",
            "status.rs:305-309",
            "a round header counting a check with all its passes",
            |b| {
                header(b)
                    .split_once("checks ")
                    .and_then(|(_, rest)| fraction(rest))
                    .is_some_and(|(done, _)| done > 0)
            },
        ),
        branch("header.cycle", "status.rs:315-317", "checking cycle N, steps N/N", |b| {
            header(b).starts_with("checking cycle ")
        }),
        branch("header.cycle-not-full", "status.rs:319-323", "the cycle's missing coverage", |b| {
            header(b).contains(", not full: ")
        }),
        branch(
            "header.cycle-missing-one-step",
            "status.rs:missing_coverage",
            "a regime one step short: `1 more step`",
            |b| header(b).split("; ").any(|part| part.ends_with(": 1 more step")),
        ),
        branch(
            "header.cycle-missing-steps",
            "status.rs:missing_coverage",
            "a regime several steps short: `N more steps`",
            |b| header(b).contains(" more steps"),
        ),
        branch("header.not-started", "status.rs:326", "checking not started", |b| {
            header(b).starts_with("checking not started")
        }),
        branch("phase.none", "status.rs:328-331", "no phase plan: no clause", |b| {
            !header(b).contains(" | ")
        })
        .unreachable(WHY_NO_PHASE_PLAN),
        branch("phase.one", "status.rs:337", "| phase 1", |b| header(b).ends_with(" | phase 1")),
        branch("phase.concluded", "status.rs:338-344", "| phase 2 concluded: clean cycles N, latest clean cycle N", |b| {
            header(b).contains("| phase 2 concluded: ")
        }),
        branch("phase.confirming", "status.rs:345", "| phase 2: confirmation cycle", |b| {
            header(b).ends_with("| phase 2: confirmation cycle")
        }),
        branch("phase.rounds-one", "status.rs:346-349, 214-216", "| phase 2: at most 1 round left", |b| {
            header(b).contains("| phase 2: at most 1 round left")
        }),
        branch("phase.rounds-many", "status.rs:346-349, 214-216", "| phase 2: at most N rounds left", |b| {
            header(b).contains("| phase 2: at most ") && !header(b).contains("at most 1 round left")
        }),
        branch("session", "status.rs:240-245", "session ID started TIME", |b| any(b, "session", |_| true)),
        branch("bios.context", "status.rs:246-256", "BIOS context", |b| any(b, "bios.context", |_| true)),
        branch("in-flight.trial", "status.rs:258-260", "in flight: [#N] msg", |b| {
            any(b, "in-flight", |block| block.flat.starts_with("in flight: [#"))
        }),
        branch("in-flight.none", "status.rs:262", "in flight: none", |b| {
            any(b, "in-flight", |block| block.flat == "in flight: none")
        }),
    ];
    for &condition in DeadEndCondition::ALL {
        let shown = format!("dead end: {} [#", dead_end_label(condition));
        all.push(branch(
            &format!("dead-end.{}", condition.as_str()),
            "status.rs:dead_end_label",
            &format!("dead end: {} [#N]", dead_end_label(condition)),
            move |b| any(b, "dead-end", |block| block.flat.starts_with(&shown)),
        ));
    }
    all.extend([
        branch("carried.failure-points", "status.rs:272-275; message.rs:197-204", "carried: … and N failure point(s) from …", |b| {
            any(b, "carried", |block| {
                block.flat.contains("failure points from") || block.flat.contains("failure point from")
            })
        }),
        branch("carried.points-behind-reason", "status.rs:272-275; message.rs:199-203", "carried: …; failure points stay behind: REASON", |b| {
            any(b, "carried", |block| {
                block.flat.contains("failure points stay behind:")
                    && !block.flat.contains("no reason recorded")
            })
        }),
        branch("carried.points-behind-no-reason", "status.rs:272-275; message.rs:200", "carried: …; failure points stay behind: no reason recorded", |b| {
            any(b, "carried", |block| block.flat.contains("stay behind: no reason recorded"))
        }),
        cell_branch("cores.bios-none", "status.rs:354-359", "BIOS cell -", "cores.row", |block| block.cell(4) == "-"),
        cell_branch("cores.bios-confirmed", "status.rs:363", "BIOS cell N", "cores.row", |block| {
            block.cell(4) != "-" && !block.cell(4).ends_with('*')
        }),
        cell_branch("cores.bios-unconfirmed", "status.rs:360-361", "BIOS cell N*", "cores.row", |block| {
            block.cell(4).ends_with('*')
        }),
        cell_branch("cores.phase-search", "status.rs:383-384; words.rs:162", "PHASE SEARCH", "cores.row", |block| block.cell(5) == "SEARCH"),
        cell_branch("cores.phase-has-room", "status.rs:383-384; words.rs:163", "PHASE HAS ROOM", "cores.row", |block| block.cell(5) == "HAS ROOM"),
        cell_branch("cores.phase-at-limit", "status.rs:383-384; words.rs:164", "PHASE AT LIMIT", "cores.row", |block| block.cell(5) == "AT LIMIT"),
        cell_branch("cores.phase-none", "status.rs:383-384", "PHASE -", "cores.row", |block| block.cell(5) == "-"),
        cell_branch("cores.failure-none", "status.rs:385-386", "FAILURE -", "cores.row", |block| block.cell(6) == "-"),
        cell_branch("cores.failure-point", "status.rs:385-386", "FAILURE N", "cores.row", |block| block.cell(6) != "-"),
        cell_branch("cores.queued-none", "status.rs:387-389", "QUEUED -", "cores.row", |block| block.cell(7) == "-"),
        cell_branch("cores.queued-reset", "status.rs:387-389", "QUEUED reset", "cores.row", |block| block.cell(7) == "reset"),
        cell_branch("cores.note-combinations", "status.rs:391-400", "combinations CN, …", "cores.row", |block| block.note("combinations ")),
        cell_branch("cores.note-last-decision", "status.rs:401-406", "last decision [#N] msg", "cores.row", |block| block.note("last decision ")),
        branch("bios.silent", "status.rs:424", "no BIOS note: none confirmed outside phase 1", |b| {
            !any(b, "bios.note", |_| true)
        })
        .unreachable(WHY_BIOS_SILENT),
        branch("bios.none-in-phase-one", "status.rs:421-423", "BIOS profile: none confirmed yet; …", |b| {
            any(b, "bios.note", |block| block.flat.starts_with("BIOS profile: none confirmed yet"))
        }),
        branch("bios.confirmed", "status.rs:425-428", "BIOS profile confirmed by the passed full cycle [#N]", |b| {
            any(b, "bios.note", |block| block.flat.starts_with("BIOS profile confirmed by"))
        }),
        branch("bios.unconfirmed", "status.rs:unconfirmed_note", "* unconfirmed since [#N]: cores … show stepped-back offsets; …", |b| {
            any(b, "bios.note", |block| block.flat.starts_with("* unconfirmed since") && block.flat.contains(" show stepped-back offsets;"))
        }),
        branch("bios.unconfirmed-one", "status.rs:unconfirmed_note", "* unconfirmed since [#N]: core N shows a stepped-back offset; …", |b| {
            any(b, "bios.note", |block| block.flat.starts_with("* unconfirmed since") && block.flat.contains(" shows a stepped-back offset;"))
        }),
        branch("combinations.observed", "status.rs:453", "CN core … , observed in hunt N [#N]", |b| {
            any(b, "combinations", |block| block.flat.contains("observed in hunt"))
        }),
        branch("combinations.fallback", "status.rs:450-451", "CN …, fallback over every candidate of hunt N [#N]", |b| {
            any(b, "combinations", |block| block.flat.contains("fallback over every candidate of hunt"))
        }),
        branch("r7.heading", "status.rs:475-479", "R7 self-sufficiency (…; not a guarantee)", |b| any(b, "r7.heading", |_| true)),
        cell_branch("r7.self-sufficient", "status.rs:self_sufficiency", "observed (N passing trials)", "r7.row", |block| block.cell(2).starts_with("observed (")),
        cell_branch("r7.self-sufficient-one", "status.rs:self_sufficiency", "observed (1 passing trial)", "r7.row", |block| block.cell(2) == "observed (1 passing trial)"),
        cell_branch("r7.not-yet", "status.rs:487", "not yet demonstrated", "r7.row", |block| block.cell(2) == "not yet demonstrated"),
        cell_branch("r7.top-yes", "status.rs:489", "TOP REQUESTER yes", "r7.row", |block| block.cell(3).starts_with("yes")),
        cell_branch("r7.top-no", "status.rs:489", "TOP REQUESTER no", "r7.row", |block| block.cell(3).starts_with("no")),
        cell_branch("r7.offset-fallback", "status.rs:490-492", "TOP REQUESTER … (offset fallback)", "r7.row", |block| block.cell(3).ends_with("(offset fallback)")),
        branch("between", "status.rs:281-284", "between-trial evidence [#N]: msg", |b| any(b, "between", |_| true)),
        branch("hunt.parked-zero", "status.rs:518", "…; parked offsets all-zero", |b| {
            any(b, "hunt", |block| block.flat.contains("parked offsets all-zero"))
        }),
        branch("hunt.parked-cycle-end", "status.rs:parked_source", "…; parked offsets from cycle end #N", |b| {
            any(b, "hunt", |block| block.flat.contains("parked offsets from cycle end #"))
        }),
        cell_branch("groups.locate", "status.rs:group_cores", "CORES locate, note locate reruns the failed load …", "groups.row", |block| {
            block.cell(1) == "locate" && block.note("locate ")
        }),
        branch("hunt.parked-held", "status.rs:520-523", "…; loaded cores … held at their failing offsets, the rest parked at 0", |b| {
            any(b, "hunt", |block| block.flat.contains("held at their failing offsets"))
        }),
        branch("hunt.signal-named", "status.rs:525-529", "unattributed SIGNAL in …", |b| {
            any(b, "hunt", |block| {
                !block.flat.contains("unattributed failure in") && !block.flat.contains("unattributed failure with")
            })
        }),
        branch("hunt.signal-fallback", "status.rs:hunt_failure", "unattributed failure … (no signal)", |b| {
            any(b, "hunt", |block| block.flat.contains("unattributed failure in") || block.flat.contains("unattributed failure with"))
        }),
        branch("hunt.no-trial", "status.rs:hunt_failure", "unattributed SIGNAL with no trial in flight;", |b| {
            any(b, "hunt", |block| block.flat.contains(" with no trial in flight;"))
        }),
        branch("hunt.trial", "status.rs:hunt_failure", "unattributed SIGNAL in RN trial ID;", |b| {
            any(b, "hunt", |block| block.flat.contains(" trial ") && !block.flat.contains("no trial in flight"))
        }),
        branch("hunt.candidates", "status.rs:539-543", "candidates N…", |b| {
            any(b, "hunt", |block| block.flat.contains(" candidates "))
        }),
        cell_branch("groups.probe", "status.rs:550-565", "CORES probe, note probe NN at N with …", "groups.row", |block| block.cell(1) == "probe"),
        cell_branch("groups.many-cores", "status.rs:566-569", "CORES N cores, note cores …", "groups.row", |block| block.cell(1).ends_with(" cores")),
        cell_branch("groups.running", "status.rs:573; words.rs:172", "OUTCOME running", "groups.row", |block| block.cell(2) == "running"),
        cell_branch("groups.passed", "status.rs:573; words.rs:173", "OUTCOME passed", "groups.row", |block| block.cell(2) == "passed"),
        cell_branch("groups.failed", "status.rs:573; words.rs:174", "OUTCOME failed", "groups.row", |block| block.cell(2) == "failed"),
        cell_branch("groups.skipped", "status.rs:573; words.rs:175", "OUTCOME skipped", "groups.row", |block| block.cell(2) == "skipped"),
        cell_branch("groups.excess-evidence", "status.rs:574-575", "PASS EVIDENCE above NEED, uncapped", "groups.row", |block| {
            matches!((number(block.cell(3)), number(block.cell(4))), (Some(passed), Some(needed)) if passed > needed)
        }),
        branch("phase2.round", "status.rs:588-591", "phase 2 round N [#N]", |b| any(b, "phase2.round", |_| true)),
        cell_branch("candidates.waits", "status.rs:598-599", "STATE waits", "candidates.row", |block| block.cell(4) == "waits"),
        cell_branch("candidates.moving", "status.rs:601; words.rs:182", "STATE moving", "candidates.row", |block| block.cell(4) == "moving"),
        cell_branch("candidates.in-place", "status.rs:601; words.rs:184", "STATE checked again in place", "candidates.row", |block| block.cell(4) == "checked again in place"),
        cell_branch("candidates.solo-none", "status.rs:606-608", "SOLO -", "candidates.row", |block| block.cell(2) == "-")
            .unreachable(WHY_SOLO_NONE),
        cell_branch("candidates.solo-limit", "status.rs:606-608", "SOLO N", "candidates.row", |block| block.cell(2) != "-"),
        branch("phase2.no-core-can-move", "status.rs:616-617", "no core can move: the confirmation cycle is all that is left", |b| {
            any(b, "phase2.remaining", |block| block.flat.starts_with("no core can move"))
        }),
        branch("phase2.worst-case-one", "status.rs:619-622, 214-216", "worst case if no round fails: 1 round left, …", |b| {
            any(b, "phase2.remaining", |block| block.flat.contains("fails: 1 round left"))
        }),
        branch("phase2.worst-case-many", "status.rs:619-622, 214-216", "worst case if no round fails: N rounds left, …", |b| {
            any(b, "phase2.remaining", |block| {
                block.flat.starts_with("worst case") && !block.flat.contains("fails: 1 round left")
            })
        }),
        cell_branch("checks.complete", "status.rs:636", "PASSES N/N with every pass", "checks.row", |block| {
            fraction(block.cell(2)).is_some_and(|(passed, needed)| passed >= needed)
        }),
        cell_branch("checks.pending", "status.rs:636", "PASSES N/N short of its passes", "checks.row", |block| {
            fraction(block.cell(2)).is_some_and(|(passed, needed)| passed < needed)
        }),
        branch("defect.listed", "status.rs:653-661", "defect N: TITLE (fixed by pull request #N); decisions … affected cores …", |b| {
            any(b, "defect", |_| true)
        }),
        branch("defect.reset-commands", "status.rs:662-664", "togi reset --core N", |b| {
            any(b, "defect", |block| block.note("togi reset --core "))
        }),
        silent("defect.cleared-core-by-core", "status.rs:150-156, 645-647", "nothing once every affected core is reset", "edge:defect-reset-each-core", |b| {
            !any(b, "defect", |_| true)
        }),
        silent("defect.cleared-by-reset-all", "status.rs:151", "nothing after a reset of every core", "edge:defect-reset-all", |b| {
            !any(b, "defect", |_| true)
        }),
        silent("defect.too-aggressive-unlisted", "status.rs:141", "nothing for a too-aggressive defect", "edge:defect-too-aggressive", |b| {
            !any(b, "defect", |_| true)
        }),
        branch("evidence.silent", "status.rs:670-672", "no evidence table before a checking profile", |b| {
            !any(b, "evidence.header", |_| true)
        }),
        branch("evidence.table", "status.rs:673-682", "REGIME WORKLOAD VALID PASSES", |b| any(b, "evidence.header", |_| true)),
        branch("evidence.heading", "status.rs:332, 744", "Valid passes supporting the checking profile (live and not alone, …; carried passes excluded)", |b| {
            any(b, "evidence.heading", |block| block.flat.contains("carried passes excluded"))
        }),
        branch("evidence.tctl", "status.rs:683-689", "Tctl max since last profile change N°C [#N]", |b| {
            any(b, "evidence.tctl", |_| true)
        }),
        branch("evidence.no-tctl", "status.rs:683", "an evidence table with no Tctl peak", |b| {
            any(b, "evidence.header", |_| true) && !any(b, "evidence.tctl", |_| true)
        }),
        branch("parser.unclassified", "this exporter", "a block the parser does not place (should stay unreached)", |b| {
            any(b, "unclassified", |_| true)
        }),
    ]);
    all
}

// ---------------------------------------------------------------------------------------------------------------
// The collected corpus.

/// The fixtures that produced something, each with the seqs of the base-journal prefixes that did.
#[derive(Default)]
struct Cases(BTreeMap<String, Vec<u64>>);

impl Cases {
    fn add(&mut self, fixture: &str, seq: Option<u64>) {
        let seqs = self.0.entry(fixture.to_owned()).or_default();
        if let Some(seq) = seq {
            if seqs.last() != Some(&seq) {
                seqs.push(seq);
            }
        }
    }

    fn count(&self) -> usize {
        self.0.values().map(|seqs| seqs.len().max(1)).sum()
    }

    /// The first `limit` fixtures, hand-built ones before the base-journal sweep, each with how many prefixes of it
    /// produced the text, and how many renderings there are in all.
    fn ids(&self, limit: usize) -> String {
        let mut cases: Vec<(&String, &Vec<u64>)> = self.0.iter().collect();
        cases.sort_by(|a, b| {
            (a.0.starts_with("base:"), a.0).cmp(&(b.0.starts_with("base:"), b.0))
        });
        let mut shown: Vec<String> = cases
            .iter()
            .take(limit)
            .map(|(fixture, seqs)| {
                let id = format!("`{SURFACE}·{fixture}·{FORM}`");
                match (seqs.first(), seqs.last()) {
                    (Some(first), Some(last)) if seqs.len() > 1 => {
                        format!("{id} ×{} (#{first} … #{last})", seqs.len())
                    }
                    (Some(first), _) => format!("{id} at #{first}"),
                    _ => id,
                }
            })
            .collect();
        if cases.len() > limit {
            shown.push(format!(
                "+{} more fixtures; {} renderings in all",
                cases.len() - limit,
                self.count()
            ));
        }
        shown.join("; ")
    }
}


struct Entry {
    raw: String,
    cases: Cases,
}

struct BaseRun {
    name: &'static str,
    events: usize,
    checked: usize,
}

struct Corpus {
    entries: BTreeMap<(usize, String), Entry>,
    branches: Vec<Branch>,
    hits: Vec<Cases>,
    problems: BTreeMap<String, Cases>,
    bases: Vec<BaseRun>,
    renderings: usize,
}

impl Corpus {
    fn new() -> Self {
        let branches = branches();
        let hits = branches.iter().map(|_| Cases::default()).collect();
        Self {
            entries: BTreeMap::new(),
            branches,
            hits,
            problems: BTreeMap::new(),
            bases: Vec::new(),
            renderings: 0,
        }
    }

    fn record(&mut self, fixture: &str, seq: Option<u64>, text: &str) {
        self.renderings += 1;
        let blocks = blocks(text);
        for block in &blocks {
            let rank = FAMILIES
                .iter()
                .position(|(family, _)| *family == block.family)
                .unwrap_or(FAMILIES.len());
            let key = (rank, template(&block.flat));
            self.entries
                .entry(key)
                .or_insert_with(|| Entry {
                    raw: block.lines.join("\n"),
                    cases: Cases::default(),
                })
                .cases
                .add(fixture, seq);
        }
        for (branch, hits) in self.branches.iter().zip(&mut self.hits) {
            if branch.only.is_some_and(|only| only != fixture) {
                continue;
            }
            if (branch.test)(&blocks) {
                hits.add(fixture, seq);
            }
        }
    }

    fn problem(&mut self, fixture: &str, seq: Option<u64>, problem: &str) {
        self.problems
            .entry(problem.to_owned())
            .or_default()
            .add(fixture, seq);
    }

    fn document(
        &self,
        scenarios: &[Whole],
        edges: &[Whole],
        synthetics: &[Whole],
        diagnostics: &[Diagnostic],
    ) -> String {
        let mut doc = String::new();
        let _ = writeln!(doc, "## `togi status`\n");
        self.scope(&mut doc, scenarios, edges, synthetics, diagnostics);
        self.manifest(&mut doc);
        self.catalogue(&mut doc);
        roster(&mut doc, scenarios, edges, synthetics);
        diagnostics_section(&mut doc, diagnostics);
        doc
    }

    fn scope(
        &self,
        doc: &mut String,
        scenarios: &[Whole],
        edges: &[Whole],
        synthetics: &[Whole],
        diagnostics: &[Diagnostic],
    ) {
        let _ = writeln!(doc, "### Scope and generation\n");
        let _ = writeln!(
            doc,
            "Generated by `TOGI_NARRATION_CORPUS=<absolute file> cargo test --locked -p togi narration_corpus -- --ignored --nocapture` \
             (`crates/togi/src/status/corpus.rs`). Every text below is the status renderer's own output at its only \
             width, 80 columns (`table.rs` `WIDTH`); the status reads no clock, so each journal prefix has exactly one \
             rendering.\n"
        );
        let _ = writeln!(doc, "- Sweep: every prefix of every `Base::ALL` journal, folded event by event:");
        for base in &self.bases {
            let _ = writeln!(
                doc,
                "  - `base:{}`: {} events, {} prefixes rendered; {} of them checked equal to `status::render`.",
                base.name, base.events, base.events, base.checked
            );
        }
        let _ = writeln!(
            doc,
            "- Scenario cuts: the {} `Scenario::ALL` journals through `status::render`.\n\
             - Edge fixtures: {} recipes from `crates/togi/tests/read/status.rs` and the finite variants no simulated \
             journal reaches (every dead-end condition, defect directions and resets, carried-session forms, \
             telemetry-less R7, Tctl-less evidence, a carried failure with no signal, solo limits three counts deep, a \
             skipped hunt group). Each is rendered through `status::render`; the roster states which ones the status \
             rendered and which it refused, and a refused one contributes no phrase.\n\
             - Synthetic renderer cases: {} cases that hand the private `Status::write` an altered projection or \
             snapshot. They are NOT session states; they show a template branch whose rendering is otherwise \
             unreachable, and the manifest names each such branch as synthetic only.\n\
             - Diagnostics: {} runs of the `status` command itself over state directories on disk; their stderr is \
             listed separately and is not part of the phrase catalogue.\n\
             - Renderings recorded: {}; distinct block templates: {}.\n",
            scenarios.len(),
            edges.len(),
            synthetics.len(),
            diagnostics.len(),
            self.renderings,
            self.entries.len()
        );
        let _ = writeln!(
            doc,
            "**What is enumerated and what is not.** The unit is a template branch of the status writer, with the \
             values the journals above produce. Unbounded parameters are represented, not enumerated: core IDs and \
             lists, offsets, seqs, counts, durations, timestamps, session IDs, BIOS context strings and defect \
             titles. Recorded journal messages are forwarded verbatim (`in flight`, `carried`, `last decision`, \
             `between-trial evidence`) and so are external error strings in diagnostics; the corpus shows the \
             messages these journals record, not every message `togi-journal`'s `payload/message.rs` can build. The \
             catalogue keeps one actual rendering per template: a block's text with every number as `N` and every \
             run of bare numbers as `N…`. Its cases list the first three fixtures (hand-built ones first) with the \
             number of base-journal prefixes that produced the text and the first and last seq, then the number of \
             further fixtures and renderings. A case ID is `{SURFACE}·<fixture>·{FORM}`.\n"
        );
        if !self.problems.is_empty() {
            let _ = writeln!(doc, "**Journals the status refused to render** (listed under diagnostics' terms, not phrases):\n");
            for (problem, cases) in &self.problems {
                let _ = writeln!(doc, "- `{}`: {}", problem.replace('`', "'"), cases.ids(3));
            }
            let _ = writeln!(doc);
        }
    }

    fn manifest(&self, doc: &mut String) {
        let _ = writeln!(doc, "### Source-branch manifest\n");
        let _ = writeln!(
            doc,
            "Each branch of `crates/togi/src/status.rs` (and the shared words it calls), decided from the executed \
             renderings. A silent branch, one that writes nothing, is proven only by the one fixture named for it. \
             `Reached` says what kind of case rendered the branch: `journal` (a real journal, simulated or \
             rewritten), `synthetic only` (only a case that alters the writer's input), or `unreachable` (no journal \
             the command accepts reaches it, shown by source reading below).\n"
        );
        let _ = writeln!(doc, "| Branch | Source | Renders | Reached | Cases | Examples |");
        let _ = writeln!(doc, "|---|---|---|---|---|---|");
        let mut unreached = Vec::new();
        let mut unreachable = Vec::new();
        for (branch, hits) in self.branches.iter().zip(&self.hits) {
            let synthetic = !hits.0.is_empty() && hits.0.keys().all(|fixture| fixture.starts_with("synthetic:"));
            let reached = match (hits.0.is_empty(), synthetic, branch.unreachable) {
                (true, _, None) => {
                    if branch.id != "parser.unclassified" {
                        unreached.push(branch);
                    }
                    "**not reached**"
                }
                (true, _, Some(_)) => {
                    unreachable.push((branch, false));
                    "unreachable"
                }
                (false, true, Some(_)) => {
                    unreachable.push((branch, true));
                    "synthetic only; unreachable"
                }
                (false, true, None) => "synthetic only",
                (false, false, _) => "journal",
            };
            let what = match branch.only {
                Some(only) => format!("{} (silent; proven by `{only}`)", branch.what),
                None => branch.what.clone(),
            };
            let _ = writeln!(
                doc,
                "| `{}` | {} | {} | {reached} | {} | {} |",
                branch.id,
                branch.source,
                what.replace('|', "\\|"),
                if hits.0.is_empty() { "0".to_owned() } else { hits.count().to_string() },
                hits.ids(3).replace('|', "\\|")
            );
        }
        let _ = writeln!(doc, "\n#### Coverage gaps\n");
        if unreached.is_empty() {
            let _ = writeln!(doc, "Every listed branch rendered in at least one case, or is shown unreachable below.\n");
        } else {
            for branch in unreached {
                let _ = writeln!(doc, "- `{}` ({}): {} — not reached by any case above.", branch.id, branch.source, branch.what);
            }
            let _ = writeln!(doc);
        }
        let _ = writeln!(doc, "#### Unreachable through the command\n");
        if unreachable.is_empty() {
            let _ = writeln!(doc, "None.\n");
        }
        for (branch, rendered) in unreachable {
            let shown = if rendered {
                format!("rendered only by the synthetic case(s) {}", self.synthetic_ids(branch))
            } else {
                "no case renders it".to_owned()
            };
            let _ = writeln!(doc, "- `{}` ({}): {} — {shown}. {}", branch.id, branch.source, branch.what, branch.unreachable.unwrap_or(""));
        }
        let _ = writeln!(doc);
        let _ = writeln!(
            doc,
            "Not mapped branch by branch here: the message builders of `togi-journal/src/payload/message.rs` whose text \
             the status forwards; their renderings appear in the catalogue only as the journals record them.\n"
        );
    }

    fn synthetic_ids(&self, branch: &Branch) -> String {
        let at = self
            .branches
            .iter()
            .position(|other| other.id == branch.id)
            .unwrap_or(0);
        self.hits[at].ids(6)
    }

    fn catalogue(&self, doc: &mut String) {
        let _ = writeln!(doc, "### Phrase catalogue\n");
        let mut family = usize::MAX;
        let mut item = 0;
        for ((rank, template), entry) in &self.entries {
            if *rank != family {
                family = *rank;
                item = 0;
                let (name, source) = FAMILIES.get(family).copied().unwrap_or(("unclassified", "-"));
                let _ = writeln!(doc, "#### `{name}` — {source}\n");
            }
            item += 1;
            let name = FAMILIES.get(family).map_or("unclassified", |(name, _)| *name);
            let _ = writeln!(doc, "**{SURFACE}.{name}.{item}** · template `{}`\n", template.replace('`', "'"));
            let _ = writeln!(doc, "```text\n{}\n```\n", entry.raw);
            let _ = writeln!(doc, "Cases: {}\n", entry.cases.ids(3));
        }
    }
}

/// The fixture roster: every fixture's ID, provenance and outcome. The phrases they produced are in the catalogue;
/// whole status renderings are not repeated, as nearly all of their lines are catalogue phrases.
fn roster(doc: &mut String, scenarios: &[Whole], edges: &[Whole], synthetics: &[Whole]) {
    let _ = writeln!(doc, "### Fixture roster\n");
    let _ = writeln!(
        doc,
        "Every fixture except the base-journal sweep (listed in the scope above). `Outcome` is what `status::render` \
         gave: the lines it wrote, or the refusal it returned.\n"
    );
    let _ = writeln!(doc, "| Case | Kind | Provenance | Outcome |");
    let _ = writeln!(doc, "|---|---|---|---|");
    for (kind, fixtures) in [
        ("scenario cut", scenarios),
        ("edge", edges),
        ("synthetic", synthetics),
    ] {
        for whole in fixtures {
            let outcome = match &whole.shown {
                Ok(text) => format!("rendered, {} lines", text.lines().count()),
                Err(problem) => format!("**refused**: `{}`", problem.replace('`', "'")),
            };
            let _ = writeln!(
                doc,
                "| `{SURFACE}·{}·{FORM}` | {kind} | {} | {} |",
                whole.fixture,
                whole.recipe.replace('|', "\\|"),
                outcome.replace('|', "\\|")
            );
        }
    }
    let _ = writeln!(doc);
}

fn diagnostics_section(doc: &mut String, diagnostics: &[Diagnostic]) {
    let _ = writeln!(doc, "### Diagnostics (stderr, separate from the status prose)\n");
    let _ = writeln!(doc, "| Case | Source | Recipe | Exit | Expected in stderr | Seen |");
    let _ = writeln!(doc, "|---|---|---|---|---|---|");
    for diagnostic in diagnostics {
        let seen = if diagnostic.stderr.contains(diagnostic.expect) { "yes" } else { "**no**" };
        let _ = writeln!(
            doc,
            "| `{SURFACE}·{}·stderr` | {} | {} | {} | `{}` | {seen} |",
            diagnostic.fixture, diagnostic.source, diagnostic.recipe, diagnostic.exit, diagnostic.expect
        );
    }
    let _ = writeln!(doc);
    for diagnostic in diagnostics {
        let _ = writeln!(doc, "#### `{SURFACE}·{}·stderr`\n", diagnostic.fixture);
        let _ = writeln!(doc, "Exit `{}`; stdout {}.\n", diagnostic.exit, if diagnostic.stdout.is_empty() {
            "empty".to_owned()
        } else {
            format!("{} lines (the status itself)", diagnostic.stdout.lines().count())
        });
        let _ = writeln!(doc, "```text\n{}```\n", diagnostic.stderr);
    }
}
