use std::collections::BTreeMap;

use togi_config::Config;
use togi_journal::{
    Activity, Attribution, CycleEvent, Event, EventKind, FactSource, Failure, FailureCarried,
    HuntStart, Outcome, Payload, Seq, Stamp, TrialCarried, TrialClass, TrialId, TrialIntent,
};
use togi_machine::{Condition, CoreSet, PerCore, Regime, Signal};
use togi_watchtest::{Base, Scenario, cut_trial, cut_when};
use unicode_width::UnicodeWidthStr as _;

use crate::support::{Session, append_between_trial_mce, append_event, session, text, togi};

#[test]
fn scenarios_render_the_status() {
    for scenario in Scenario::ALL {
        let dir = session(scenario);
        let ran = togi(&dir, &["status"]);
        assert_eq!(ran.status.code(), Some(0), "{}", scenario.name());
        assert_eq!(text(&ran.stderr), "", "{}", scenario.name());
        crate::support::snapshot(
            &format!("status__status_{}", scenario.name()),
            text(&ran.stdout),
        );
    }
}

#[test]
fn a_journal_without_a_session_is_an_error() {
    let dir = crate::support::Session::empty();
    crate::support::write_journal(&dir, "");
    let ran = togi(&dir, &["status"]);
    assert_eq!(ran.status.code(), Some(1));
    assert_eq!(text(&ran.stdout), "");
    assert!(
        text(&ran.stderr).contains("no session in"),
        "{}",
        text(&ran.stderr)
    );
}

#[test]
fn a_missing_journal_is_an_error_naming_the_file() {
    let dir = crate::support::Session::empty();
    let ran = togi(&dir, &["status"]);
    assert_eq!(ran.status.code(), Some(1));
    assert!(
        text(&ran.stderr).contains("no journal at"),
        "{}",
        text(&ran.stderr)
    );
}

#[test]
fn status_takes_no_arguments() {
    let dir = session(Scenario::Search);
    let ran = togi(&dir, &["status", "extra"]);
    assert_eq!(ran.status.code(), Some(2));
    assert_eq!(text(&ran.stdout), "");
}

#[test]
fn status_fits_eighty_columns() {
    for scenario in Scenario::ALL {
        let dir = session(scenario);
        let ran = togi(&dir, &["status"]);
        for line in text(&ran.stdout).lines() {
            assert!(
                line.width() <= 80,
                "{}: {} cells: {line}",
                scenario.name(),
                line.width()
            );
        }
    }
}

#[test]
fn a_concluded_phase_names_the_latest_clean_cycle_not_the_running_cycle() {
    let events = Scenario::Concluded.events().unwrap();
    let projection = togi_tuner::replay(&events).unwrap().project();
    let latest = projection.last_clean_cycle.unwrap().get();
    let running = projection.checking.cycle.unwrap().get();
    assert_ne!(latest, running);
    let shown = status_of(&installed(&events));
    let header = flat(shown.split("\nsession ").next().unwrap());
    assert!(
        header.contains(&format!(
            "phase 2 concluded: clean cycles {}, latest clean cycle {latest}",
            projection.clean_cycles
        )),
        "{header}"
    );
    assert!(
        header.starts_with(&format!("checking cycle {running},")),
        "{header}"
    );
}

#[test]
fn status_help() {
    let ran = togi(&crate::support::Session::empty(), &["status", "--help"]);
    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(text(&ran.stderr), "");
    crate::support::snapshot("status__status_help", text(&ran.stdout));
}

#[test]
fn a_too_cautious_defect_lists_reset_commands_until_each_affected_core_is_reset() {
    let dir = session(Scenario::Search);
    append_event(
        &dir,
        "defect.found",
        "defect 1",
        &serde_json::json!({
            "id": 1, "title": "step\u{1b}[31m too cautious", "detail": "why", "pr": 7,
            "direction": "too_cautious", "cores": [3, 5], "decisions": [4, 6],
        }),
    );
    let shown = status_of(&dir);
    assert!(
        shown.contains("defect 1: step\\x1b[31m too cautious (fixed by pull request #7)"),
        "{shown}"
    );
    assert!(shown.contains("togi reset --core 3\n"));
    assert!(shown.contains("togi reset --core 5\n"));
    append_event(
        &dir,
        "defect.answered",
        "defect 1 answered no",
        &serde_json::json!({"id": 1, "cores": [3, 5], "answer": "no"}),
    );
    let answered = status_of(&dir);
    assert!(
        answered.contains("defect 1: "),
        "an answer hides the defect"
    );
    assert!(answered.contains("togi reset --core 3\n"));
    assert!(answered.contains("togi reset --core 5\n"));
    append_event(
        &dir,
        "command.reset",
        "reset core 3",
        &serde_json::json!({"target": {"core": 3}}),
    );
    let one = status_of(&dir);
    assert!(!one.contains("togi reset --core 3"));
    assert!(one.contains("togi reset --core 5\n"));
    append_event(
        &dir,
        "command.reset",
        "reset core 5",
        &serde_json::json!({"target": {"core": 5}}),
    );
    let both = status_of(&dir);
    assert!(!both.contains("defect 1"), "{both}");
    assert!(!both.contains("togi reset --core"), "{both}");
}

#[test]
fn a_reset_of_every_core_removes_the_defect() {
    let dir = session(Scenario::Search);
    append_event(
        &dir,
        "defect.found",
        "defect 1",
        &serde_json::json!({
            "id": 1, "title": "too cautious", "detail": "why", "pr": 7,
            "direction": "too_cautious", "cores": [3, 5], "decisions": [4, 6],
        }),
    );
    append_event(
        &dir,
        "command.reset",
        "reset all",
        &serde_json::json!({"target": "all"}),
    );
    assert!(!status_of(&dir).contains("defect 1"));
}

#[test]
fn a_between_trial_mce_is_listed_as_evidence_and_escaped() {
    let dir = session(Scenario::Search);
    append_between_trial_mce(&dir);
    let ran = togi(&dir, &["status"]);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    let shown = text(&ran.stdout);
    assert!(shown.contains("between-trial evidence [#"), "{shown}");
    assert!(shown.contains("\\x1b[2J"), "{shown}");
    assert!(!shown.contains('\u{1b}'));
}

#[test]
fn a_hunt_names_the_decisive_failure_signal() {
    let dir = session(Scenario::Hunt);
    let ran = togi(&dir, &["status"]);
    let shown = text(&ran.stdout);
    let hunt = shown
        .lines()
        .find(|line| line.starts_with("hunt 1 [#"))
        .unwrap();
    assert!(
        hunt.contains("unattributed corrected_mce in R2 trial"),
        "{hunt}"
    );
}

#[test]
fn wide_bios_fields_wrap_by_terminal_cells_and_stay_whole() {
    let dir = session(Scenario::Search);
    let board = "日本 ".repeat(20);
    append_event(
        &dir,
        "session.context",
        "context",
        &serde_json::json!({
            "bios_version": "v".repeat(30), "board": board, "cpu_model": "cpu ".repeat(15),
            "microcode": "0x1", "boost_limit_mhz": 5000,
        }),
    );
    let shown = status_of(&dir);
    assert!(flat(&shown).contains(board.trim_end()), "{shown}");
    for line in shown.lines() {
        assert!(line.width() <= 80, "{} cells: {line}", line.width());
    }
}

#[test]
fn carried_facts_add_self_sufficiency_evidence_but_no_live_regime_exposure() {
    let base = Scenario::Between.events().unwrap();
    let intent = last_intent(&base);
    let before = status_of(&installed(&base));
    let carried = vec![
        (vec![], carried_pass(&base, &intent).into()),
        (
            vec![],
            FailureCarried {
                source: fact_source(&base, None),
                class: TrialClass {
                    regime: Regime::R6,
                    workload: None,
                    cores: intent.cores,
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
                    profile: Some(intent.profile),
                    known_failure: None,
                    reason: None,
                    round: None,
                },
            }
            .into(),
        ),
    ];
    let after = status_of(&installed(&then(&base, carried)));
    let changed: Vec<(&str, &str)> = before
        .lines()
        .zip(after.lines())
        .filter(|(before, after)| before != after)
        .collect();
    assert_eq!(before.lines().count(), after.lines().count(), "{after}");
    let [(was, now)] = changed[..] else {
        panic!("exactly the carried pass's self-sufficiency line changes: {changed:?}");
    };
    let passing = |line: &str| -> u32 {
        let (_, rest) = line.split_once("observed (").unwrap();
        rest.split(' ').next().unwrap().parse().unwrap()
    };
    assert_eq!(passing(now), passing(was) + 1, "{was} -> {now}");
    let regime = |shown: &str| -> Vec<String> {
        shown
            .lines()
            .skip_while(|line| !line.starts_with("REGIME"))
            .take_while(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(
        regime(&before),
        regime(&after),
        "carried passes are not live exposure"
    );
}

#[test]
fn a_queued_reset_shows_in_the_queued_column_until_the_search_restarts() {
    let dir = session(Scenario::Stopped);
    let queued = |dir: &Session| {
        let ran = togi(dir, &["status"]);
        assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
        text(&ran.stdout)
            .lines()
            .find(|line| line.starts_with("03 "))
            .unwrap()
            .to_owned()
    };
    assert!(queued(&dir).ends_with(" -"));
    append_event(
        &dir,
        "command.reset",
        "reset core 3",
        &serde_json::json!({"target": {"core": 3}}),
    );
    assert!(queued(&dir).ends_with(" reset"), "{}", queued(&dir));
}

#[test]
fn the_core_table_leads_with_core_ccd_slot_and_the_resident_offset() {
    let dir = session(Scenario::Checking);
    let ran = togi(&dir, &["status"]);
    let shown = text(&ran.stdout);
    let words = |line: &str| {
        line.split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let header = shown.lines().find(|line| line.starts_with("CORE")).unwrap();
    assert_eq!(
        words(header),
        [
            "CORE", "CCD", "SLOT", "OFFSET", "BIOS", "PHASE", "FAILURE", "QUEUED"
        ]
    );
    let row = |core: &str| {
        words(
            shown
                .lines()
                .find(|line| line.starts_with(&format!("{core} ")))
                .unwrap(),
        )
    };
    let first = row("00");
    assert_eq!(first[..4], ["00", "0", "0", "-26"]);
    assert_eq!(
        first[first.len() - 2],
        "-28",
        "the resident offset is not the failure point: {first:?}"
    );
    let ninth = row("08");
    assert_eq!(ninth[..4], ["08", "1", "0", "-5"]);
    assert_eq!(ninth[ninth.len() - 2], "-7");
    let last = row("15");
    assert_eq!(last[..3], ["15", "1", "7"]);
}

#[test]
fn the_phase_column_uses_the_dashboards_state_words() {
    use togi_journal::CorePhase;
    use togi_watch::words::phase_word;

    let all = [CorePhase::Search, CorePhase::HasRoom, CorePhase::AtLimit].map(phase_word);
    let mut seen = std::collections::BTreeSet::new();
    for scenario in Scenario::ALL {
        let dir = session(scenario);
        let ran = togi(&dir, &["status"]);
        let shown = text(&ran.stdout);
        let header = shown.lines().find(|line| line.starts_with("CORE")).unwrap();
        let (start, end) = (
            header.find("PHASE").unwrap(),
            header.find("FAILURE").unwrap(),
        );
        for line in shown
            .lines()
            .skip_while(|line| !line.starts_with("CORE"))
            .skip(1)
            .take_while(|line| !line.is_empty())
            .filter(|line| {
                line.get(..2)
                    .is_some_and(|id| id.bytes().all(|b| b.is_ascii_digit()))
            })
        {
            let cell = line.get(start..end).unwrap().trim();
            if cell != "-" {
                assert!(all.contains(&cell), "{} shows {cell:?}", scenario.name());
                seen.insert(cell.to_owned());
            }
        }
    }
    assert_eq!(seen.len(), all.len(), "{seen:?} of {all:?}");
}

#[test]
fn the_regime_table_keeps_its_regime_column() {
    let dir = session(Scenario::Between);
    let ran = togi(&dir, &["status"]);
    let shown = text(&ran.stdout);
    let at = shown
        .lines()
        .position(|line| line.starts_with("REGIME"))
        .unwrap();
    let rows: Vec<&str> = shown.lines().skip(at).collect();
    assert_eq!(
        rows[0].split_whitespace().collect::<Vec<_>>(),
        ["REGIME", "WORKLOAD", "VALID", "PASSES"]
    );
    assert_eq!(
        rows[1].split_whitespace().collect::<Vec<_>>(),
        ["R7", "mprime-avx2-36k-248k-allcore", "6"]
    );
}

pub(super) fn installed(events: &[Event]) -> Session {
    crate::support::installed(events)
}

fn status_of(dir: &Session) -> String {
    let ran = togi(dir, &["status"]);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    assert_eq!(text(&ran.stderr), "");
    text(&ran.stdout).to_owned()
}

/// `shown` with every run of whitespace as one space, so a wrapped sentence can be searched whole.
fn flat(shown: &str) -> String {
    shown.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn next_seq(events: &[Event]) -> Seq {
    Seq::new(events.last().unwrap().seq.get() + 1).unwrap()
}

/// `events` followed by `extra` payloads, each with the causes named, on the last event's boot.
pub(super) fn then(events: &[Event], extra: Vec<(Vec<Seq>, Payload)>) -> Vec<Event> {
    let mut events = events.to_vec();
    for (cause, payload) in extra {
        let last = events.last().unwrap();
        let stamp = Stamp {
            time: last
                .time
                .checked_add(jiff::SignedDuration::from_secs(1))
                .unwrap(),
            mono: None,
        };
        let event = Event::new(next_seq(&events), stamp, last.boot.clone(), cause, payload);
        events.push(event);
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
        .unwrap()
}

pub(super) fn last_intent(events: &[Event]) -> TrialIntent {
    events
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            Payload::TrialIntent(intent) => Some(intent.clone()),
            _ => None,
        })
        .unwrap()
}

pub(super) fn fact_source(events: &[Event], trial: Option<&str>) -> FactSource {
    let Payload::SessionStart(start) = &events[0].payload else {
        unreachable!("a journal starts with its session");
    };
    FactSource {
        session: start.session,
        seq: Seq::new(40).unwrap(),
        build: start.build.clone(),
        trial: trial.map(|trial| TrialId::from(trial.to_owned())),
        time: events[0].time,
        boot: events[0].boot.clone(),
        backend: None,
    }
}

/// A carried pass of the class and profile `intent` ran.
pub(super) fn carried_pass(events: &[Event], intent: &TrialIntent) -> TrialCarried {
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

fn hunt_line(shown: &str, hunt: u32) -> String {
    let at = shown
        .find(&format!("hunt {hunt} [#"))
        .unwrap_or_else(|| panic!("no hunt {hunt} in\n{shown}"));
    let paragraph = shown[at..].split("\n  candidates").next().unwrap();
    flat(paragraph)
}

enum Carried {
    Trial,
    Idle,
}

/// The status of a session in which a hunt opens on a carried fact, the failure it cites.
fn hunt_from_a_carried_fact(carried: &Carried) -> String {
    let base = Scenario::Between.events().unwrap();
    let mut start = first_hunt_start(Base::Session.events().unwrap());
    let fact: Payload = match carried {
        Carried::Trial => {
            let mut fact = carried_pass(&base, &last_intent(&base));
            fact.class.regime = start.regime;
            fact.class.workload = Some(start.workload);
            fact.class.cores = start.cores;
            fact.condition = Condition::Together;
            fact.profile = start.failing;
            fact.outcome = Outcome::Failure;
            fact.signal = Some(Signal::ComputationError);
            start.trial = Some(TrialId::from("origin".to_owned()));
            fact.into()
        }
        Carried::Idle => {
            start.regime = Regime::R6;
            start.workload = first_hunt_start(Base::Probe.events().unwrap()).workload;
            start.trial = None;
            FailureCarried {
                source: fact_source(&base, None),
                class: TrialClass {
                    regime: Regime::R6,
                    workload: None,
                    cores: start.cores,
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
                    profile: Some(start.failing),
                    known_failure: None,
                    reason: None,
                    round: None,
                },
            }
            .into()
        }
    };
    let seq = next_seq(&base);
    start.failure = seq;
    let events = then(
        &base,
        vec![(vec![], fact), (vec![seq], Payload::HuntStart(start))],
    );
    hunt_line(&status_of(&installed(&events)), 1)
}

#[test]
fn a_hunt_opened_by_a_carried_trial_names_the_carried_signal() {
    let hunt = hunt_from_a_carried_fact(&Carried::Trial);
    assert!(
        hunt.contains("unattributed computation_error in R2 trial origin;"),
        "{hunt}"
    );
}

#[test]
fn a_hunt_opened_by_a_carried_idle_failure_names_the_carried_signal() {
    let hunt = hunt_from_a_carried_fact(&Carried::Idle);
    assert!(
        hunt.contains("unattributed crash with no trial in flight;"),
        "{hunt}"
    );
    assert!(!hunt.contains("trial -"), "{hunt}");
}

#[test]
fn a_located_hunt_names_the_held_loaded_cores_as_its_parked_source() {
    let base = Scenario::Between.events().unwrap();
    let intent = last_intent(&Scenario::Checking.events().unwrap());
    let mut fact = carried_pass(&base, &intent);
    fact.outcome = Outcome::Failure;
    fact.signal = Some(Signal::Crash);
    let seq = next_seq(&base);
    let mut start = first_hunt_start(Base::Session.events().unwrap());
    start.failure = seq;
    start.trial = Some(TrialId::from("origin".to_owned()));
    start.regime = intent.regime;
    start.workload = intent.workload;
    start.cores = intent.cores;
    start.duration_s = intent.duration_s;
    start.failing = intent.profile;
    start.parked = intent
        .profile
        .iter()
        .map(|(core, offset)| {
            (
                core,
                if intent.cores.contains(core) {
                    offset
                } else {
                    togi_machine::TargetOffset::ZERO
                },
            )
        })
        .collect();
    start.parked_seq = None;
    start.candidates = intent
        .profile
        .iter()
        .filter(|(core, offset)| !intent.cores.contains(*core) && offset.get() != 0)
        .map(|(core, _)| core)
        .collect();
    let events = then(
        &base,
        vec![(vec![], fact.into()), (vec![seq], start.into())],
    );
    let hunt = hunt_line(&status_of(&installed(&events)), 1);
    assert!(
        hunt.contains(
            "loaded cores 00 01 02 03 04 05 06 07 held at their failing offsets, the rest parked at 0"
        ),
        "{hunt}"
    );
    assert!(!hunt.contains("all-zero"), "{hunt}");
}

#[test]
fn a_hunt_after_a_passed_full_cycle_names_the_cycle_end_it_parks_from() {
    let base = Base::LaterHunt.events().unwrap();
    let mut passed_full_cycle = false;
    let mut hunt_number = 0;
    let mut events = cut_when(base, |state, event| {
        if let Payload::CheckingCycle(cycle) = &event.payload {
            passed_full_cycle |= cycle.event == CycleEvent::End && cycle.passed && cycle.full;
        }
        if event.kind() == EventKind::TrialStart
            && passed_full_cycle
            && let Some(plan) = state.hunt_plan()
        {
            hunt_number = plan.number;
            return true;
        }
        false
    })
    .unwrap()
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
        .unwrap();
    for event in &mut events {
        if let Payload::HuntStart(start) = &mut event.payload {
            if start.hunt.get() == hunt_number {
                start.parked_seq = Some(cycle_end);
            }
        }
    }
    let hunt = hunt_line(&status_of(&installed(&events)), hunt_number);
    assert!(
        hunt.contains(&format!("parked offsets from cycle end #{cycle_end}")),
        "{hunt}"
    );
}

#[test]
fn hunt_group_columns_stay_aligned_when_a_group_lists_sixteen_cores() {
    let dir = session(Scenario::MemberProbe);
    let shown = status_of(&dir);
    let header = shown
        .lines()
        .find(|line| line.starts_with("GROUP"))
        .unwrap();
    let at = |word: &str| header.find(word).unwrap();
    let (outcome, evidence, need) = (at("OUTCOME"), at("PASS EVIDENCE"), at("NEED"));
    let rows: Vec<&str> = shown
        .lines()
        .skip_while(|line| !line.starts_with("GROUP"))
        .skip(1)
        .filter(|line| {
            line.strip_prefix('G')
                .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        })
        .collect();
    assert!(rows.len() > 10, "{shown}");
    assert!(
        rows.iter().any(|row| row[..outcome].contains("16 cores")),
        "no 16-core group in\n{shown}"
    );
    for row in &rows {
        let word = row[outcome..].split_whitespace().next().unwrap();
        assert!(
            ["running", "passed", "failed", "skipped"].contains(&word),
            "OUTCOME is not at column {outcome}: {row}"
        );
        assert!(
            row[evidence..]
                .split_whitespace()
                .next()
                .unwrap()
                .parse::<u32>()
                .is_ok(),
            "PASS EVIDENCE is not at column {evidence}: {row}"
        );
        assert!(
            row[need..].trim().parse::<u32>().is_ok(),
            "NEED is not at column {need}: {row}"
        );
        assert!(row.width() <= 80, "{row}");
    }
    assert!(
        shown.lines().any(|line| {
            line.trim_start()
                .starts_with("cores 00 01 02 03 04 05 06 07 08 09 10 11 12 13 14 15")
        }),
        "the 16 cores are not listed under their group:\n{shown}"
    );
}

#[test]
fn a_phase_two_round_with_completed_checks_counts_them() {
    let base = Base::Combination.events().unwrap();
    let events = cut_when(base, |state, event| {
        event.kind() == EventKind::TrialEnd
            && state.project().deepening.is_some_and(|round| {
                round
                    .checks
                    .iter()
                    .filter(|check| check.passed >= check.needed)
                    .count()
                    == 1
            })
    })
    .unwrap();
    let shown = status_of(&installed(events));
    assert!(
        shown.starts_with("phase 2 round 1, checks 1/"),
        "{}",
        shown.lines().next().unwrap()
    );
    let done = shown
        .lines()
        .skip_while(|line| !line.starts_with("CHECK"))
        .skip(1)
        .filter_map(|line| line.split_whitespace().last())
        .filter(|cell| {
            cell.split_once('/')
                .is_some_and(|(p, n)| p == n && p != "0")
        })
        .count();
    assert_eq!(done, 1, "{shown}");
}

#[test]
fn a_short_checking_cycle_names_the_coverage_it_lacks() {
    let config = Config::parse(r#"{"checking":{"cycle":["R1","R2"]}}"#).unwrap();
    let machine = togi_watchtest::default_machine(1).unwrap();
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
    .unwrap();
    let cut = cut_trial(&events, |intent| {
        intent.activity == Some(Activity::Checking)
    })
    .unwrap();
    let shown = flat(&status_of(&installed(cut)));
    assert!(
        shown.contains("checking cycle 1, steps 0/2, not full: R1: 2 more steps; R2: 2 more steps"),
        "{shown}"
    );
    assert!(shown.contains("R7: 3 more steps"), "{shown}");
}

#[test]
fn a_full_checking_cycle_does_not_say_it_is_not_full() {
    let shown = status_of(&session(Scenario::Checking));
    assert!(!shown.contains("not full"), "{shown}");
}

#[test]
fn a_fallback_combination_names_the_hunt_it_covered_every_candidate_of() {
    let mut events = Scenario::Combination.events().unwrap();
    let observed = status_of(&installed(&events));
    assert!(observed.contains("observed in hunt 1 [#"), "{observed}");
    assert!(!observed.contains("fallback over"), "{observed}");
    let mut seq = None;
    for event in &mut events {
        if let Payload::Combination(combination) = &mut event.payload {
            combination.fallback = true;
            seq = Some(event.seq);
        }
    }
    let shown = status_of(&installed(&events));
    assert!(
        shown.contains(&format!(
            "fallback over every candidate of hunt 1 [#{}]",
            seq.unwrap()
        )),
        "{shown}"
    );
    assert!(!shown.contains("observed in hunt"), "{shown}");
}

#[test]
fn pass_evidence_above_need_is_shown_uncapped() {
    let base = Scenario::Hunt.events().unwrap();
    let intent = last_intent(&base);
    let carried: Vec<(Vec<Seq>, Payload)> = (0..7)
        .map(|_| (vec![], carried_pass(&base, &intent).into()))
        .collect();
    let shown = status_of(&installed(&then(&base, carried)));
    let header = shown
        .lines()
        .find(|line| line.starts_with("GROUP"))
        .unwrap();
    let (evidence, need) = (
        header.find("PASS EVIDENCE").unwrap(),
        header.find("NEED").unwrap(),
    );
    let running = shown.lines().find(|line| line.starts_with("G1 ")).unwrap();
    let passed: u32 = running[evidence..need].trim().parse().unwrap();
    let needed: u32 = running[need..].trim().parse().unwrap();
    assert!(
        passed > needed,
        "{passed} passes against {needed}:\n{shown}"
    );
}

#[test]
fn a_journal_the_fold_rejects_is_an_error_with_nothing_on_stdout() {
    let dir = session(Scenario::Between);
    let mut lines = crate::support::lines(&dir);
    let mut start: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(start["kind"], "session.start");
    start["data"]["cores"].as_array_mut().unwrap().pop();
    lines[0] = start.to_string();
    crate::support::write_journal(&dir, lines.join("\n") + "\n");
    let ran = togi(&dir, &["status"]);
    assert_eq!(ran.status.code(), Some(1), "{}", text(&ran.stdout));
    assert_eq!(text(&ran.stdout), "");
    let stderr = text(&ran.stderr);
    assert!(
        stderr.starts_with("togi status: ") && stderr.contains("does not have"),
        "{stderr}"
    );
}

#[test]
fn a_session_with_no_search_hunt_round_or_checking_plan_says_checking_has_not_started() {
    let base = Base::Session.events().unwrap();
    let events = cut_when(base, |state, _| {
        let projection = state.project();
        projection
            .cores
            .iter()
            .all(|core| core.phase != Some(togi_journal::CorePhase::Search))
            && projection.hunt_plan.is_none()
            && projection.deepening.is_none()
            && projection.cycle_plan.steps.is_empty()
    })
    .unwrap();
    let shown = status_of(&installed(events));
    assert!(shown.contains("checking not started"), "{shown}");
}

#[test]
fn a_failed_write_to_stdout_is_an_error() {
    let dir = session(Scenario::Search);
    let ran = crate::support::output(dir.host().run_to_closed_stdout(dir.path(), &["status"]));
    assert_eq!(ran.status.code(), Some(1), "{}", text(&ran.stderr));
    assert!(
        text(&ran.stderr).starts_with("togi status: write output: "),
        "{}",
        text(&ran.stderr)
    );
}

/// Rewrites the message of the last event of `kind` in the journal.
fn rewrite_msg(dir: &Session, kind: &str, msg: &str) {
    let mut lines = crate::support::lines(dir);
    let at = lines
        .iter()
        .rposition(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()["kind"] == kind)
        .unwrap();
    let mut event: serde_json::Value = serde_json::from_str(&lines[at]).unwrap();
    event["msg"] = msg.into();
    lines[at] = event.to_string();
    crate::support::write_journal(dir, lines.join("\n") + "\n");
}

const HOSTILE: &str = "a\u{1b}[31mb\u{202e}c\nd\u{2066}e";
const HOSTILE_ESCAPED: &str = "a\\x1b[31mb\\u202ec\\nd\\u2066e";

fn assert_visibly_escaped(shown: &str) {
    for raw in ['\u{1b}', '\u{202e}', '\u{2066}'] {
        assert!(
            !shown.contains(raw),
            "{raw:?} reached the terminal:\n{shown}"
        );
    }
}

#[test]
fn controls_in_an_in_flight_message_are_escaped() {
    let dir = session(Scenario::Search);
    rewrite_msg(&dir, "trial.intent", HOSTILE);
    let shown = status_of(&dir);
    assert_visibly_escaped(&shown);
    let line = shown
        .lines()
        .find(|line| line.starts_with("in flight: "))
        .unwrap();
    assert!(line.ends_with(HOSTILE_ESCAPED), "{line}");
}

#[test]
fn controls_in_a_carried_session_message_are_escaped() {
    let dir = session(Scenario::Search);
    append_event(
        &dir,
        "session.carried",
        HOSTILE,
        &serde_json::json!({"sources": [], "failure_points": false}),
    );
    let shown = status_of(&dir);
    assert_visibly_escaped(&shown);
    assert!(
        shown
            .lines()
            .any(|line| line.starts_with("carried: [#") && line.ends_with(HOSTILE_ESCAPED)),
        "{shown}"
    );
}

#[test]
fn controls_in_a_decision_message_are_escaped() {
    let dir = session(Scenario::Search);
    rewrite_msg(&dir, "tuner.decision", HOSTILE);
    let shown = status_of(&dir);
    assert_visibly_escaped(&shown);
    assert!(
        shown
            .lines()
            .any(|line| line.contains("last decision [#") && line.ends_with(HOSTILE_ESCAPED)),
        "{shown}"
    );
}

#[test]
fn controls_in_bios_context_fields_are_escaped() {
    let dir = session(Scenario::Search);
    append_event(
        &dir,
        "session.context",
        "context",
        &serde_json::json!({
            "bios_version": HOSTILE, "board": HOSTILE, "cpu_model": HOSTILE,
            "microcode": HOSTILE, "boost_limit_mhz": 5000,
        }),
    );
    let shown = status_of(&dir);
    assert_visibly_escaped(&shown);
    assert_eq!(flat(&shown).matches(HOSTILE_ESCAPED).count(), 4, "{shown}");
}
