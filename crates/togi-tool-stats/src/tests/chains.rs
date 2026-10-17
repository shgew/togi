use togi_journal::{Activity, CheckingChain, CycleEvent, Outcome, Rederived, Seq, TunerDecision};
use togi_machine::{CcdId, Regime, Signal};

use super::support::{Log, cycle, decision, end, intent, nz, profile, set, trial_start, workload};
use crate::chains::{self, Change, Derivation, Kind, Resolution};
use crate::project::Projection;

const CORES: [(i64, u8); 8] = [
    (0, 0),
    (1, 0),
    (2, 0),
    (3, 0),
    (4, 1),
    (5, 1),
    (6, 1),
    (7, 1),
];

/// A derivation of cycle 1's step 1 on `ccd`, ordered by `groups` from the highest request down.
fn chain(
    ccd: u8,
    part: u32,
    groups: &[&[i64]],
    cores: &[i64],
    offsets: &[i64],
    rederived: Option<Rederived>,
) -> CheckingChain {
    CheckingChain {
        cycle: nz(1),
        step: nz(1),
        ccd: CcdId(ccd),
        workload: workload(Regime::R7, 0),
        groups: groups.iter().map(|group| set(group)).collect(),
        cores: set(cores),
        source_seqs: Vec::new(),
        profile: profile(offsets),
        part: nz(part),
        rederived,
    }
}

/// A trial of cycle 1's step 1 loading `cores`, ended after `seconds`; returns the intent's seq.
fn cycle_trial(
    log: &mut Log,
    id: &str,
    cores: &[i64],
    offsets: &[i64],
    outcome: Outcome,
    seconds: u32,
    rerun: bool,
) -> Seq {
    let mut planned = intent(id, Regime::R7, cores, offsets);
    planned.cycle = Some(nz(1));
    planned.step = Some(nz(1));
    planned.activity = Some(Activity::Checking);
    planned.rerun = rerun;
    let at = log.add(planned);
    log.add(trial_start(id));
    let mut ended = end(id, outcome);
    ended.duration_s = seconds;
    log.add(ended);
    at
}

fn backoff(core: i64, from: i64, to: i64) -> TunerDecision {
    let mut d = decision(togi_journal::Decision::Backoff, core, from, to);
    d.activity = Activity::Checking;
    d
}

const P0: [i64; 8] = [-20; 8];
const P1: [i64; 8] = [-20, -19, -20, -20, -20, -20, -20, -20];
const P2: [i64; 8] = [-19, -19, -20, -20, -20, -20, -20, -20];

/// Two CCDs of four cores. Cycle 1's step 1 derives both chains and passes them at P0. A backoff on
/// core 01 moves CCD 0's top requester: its first partial changes to a set the step never required, whose
/// first trial crashes the machine (as #896's trial 0278 did) after a rerun of the set passed. The backoff
/// of core 00 that follows reorders CCD 0 back to the partial it held at P0, which its P0 passes cover;
/// CCD 1, whose offsets never moved, re-derives unchanged both times and runs nothing.
fn rederived_after_backoffs() -> Log {
    let mut log = Log::new(&CORES);
    log.add(cycle(1, CycleEvent::Start, false));
    log.add(chain(0, 1, &[&[0], &[1], &[2, 3]], &[1, 2, 3], &P0, None));
    cycle_trial(&mut log, "0001", &[1, 2, 3], &P0, Outcome::Pass, 300, false);
    cycle_trial(
        &mut log,
        "0002",
        &[1, 2, 3],
        &P0,
        Outcome::Pass,
        1200,
        false,
    );
    log.add(chain(0, 2, &[&[1], &[2, 3]], &[2, 3], &P0, None));
    cycle_trial(&mut log, "0003", &[2, 3], &P0, Outcome::Pass, 300, false);
    log.add(chain(0, 3, &[&[2, 3]], &[], &P0, None));
    log.add(chain(1, 1, &[&[4], &[5, 6, 7]], &[5, 6, 7], &P0, None));
    cycle_trial(&mut log, "0004", &[5, 6, 7], &P0, Outcome::Pass, 300, false);
    log.add(chain(1, 2, &[&[5, 6, 7]], &[], &P0, None));

    log.add(backoff(1, -20, -19));
    let changed = Some(Rederived::Changed);
    let unchanged = Some(Rederived::Unchanged);
    log.add(chain(
        0,
        1,
        &[&[1], &[0], &[2, 3]],
        &[0, 2, 3],
        &P1,
        changed,
    ));
    cycle_trial(&mut log, "0005", &[0, 2, 3], &P1, Outcome::Pass, 1200, true);
    let mut planned = intent("0006", Regime::R7, &[0, 2, 3], &P1);
    planned.cycle = Some(nz(1));
    planned.step = Some(nz(1));
    planned.activity = Some(Activity::Checking);
    log.at(999);
    let crashed = log.add(planned);
    log.add(trial_start("0006"));
    log.boot("b").at(1100).mono(40_000);
    log.add(super::support::crash_detected("a", Some(crashed)));
    let mut ended = end("0006", Outcome::Failure);
    ended.signal = Some(Signal::Crash);
    ended.duration_s = 15;
    ended.interrupted = true;
    log.add(ended);
    log.add(backoff(0, -20, -19));

    log.add(chain(0, 1, &[&[0, 1], &[2, 3]], &[1, 2, 3], &P2, changed));
    log.add(chain(0, 2, &[&[1], &[2, 3]], &[2, 3], &P2, unchanged));
    log.add(chain(0, 3, &[&[2, 3]], &[], &P2, unchanged));
    log.add(chain(1, 1, &[&[4], &[5, 6, 7]], &[5, 6, 7], &P2, unchanged));
    log.add(chain(1, 2, &[&[5, 6, 7]], &[], &P2, unchanged));
    log.add(cycle(1, CycleEvent::End, true));
    log
}

/// What each derivation tells: seq, kind, change, repeated, result, held and its trials.
type Summary = (
    u64,
    Kind,
    Option<Change>,
    bool,
    Resolution,
    bool,
    Vec<String>,
);

fn summary(derivations: &[Derivation<'_>]) -> Vec<Summary> {
    derivations
        .iter()
        .map(|d| {
            (
                d.seq.get(),
                d.kind,
                d.change,
                d.repeated,
                d.resolution,
                d.held,
                d.trials.iter().map(|t| t.trial.to_string()).collect(),
            )
        })
        .collect()
}

#[test]
fn each_derivation_is_attributed_its_change_its_trials_and_whether_the_chain_kept_it() {
    let log = rederived_after_backoffs();
    let journal = log.journal();
    let p = Projection::new(&journal);
    let derivations = chains::attribute(&p, None);
    let (first, unchanged, changed) = (Kind::First, Kind::Unchanged, Kind::Changed);
    let (same, other) = (Some(Change::SameCcd), Some(Change::OtherCcd));
    let (ran, covered, end) = (Resolution::Ran, Resolution::Covered, Resolution::End);
    let ids = |ids: &[&str]| ids.iter().map(|&id| id.to_owned()).collect::<Vec<_>>();
    assert_eq!(
        summary(&derivations),
        [
            (3, first, None, false, ran, true, ids(&["0001", "0002"])),
            (10, first, None, false, ran, true, ids(&["0003"])),
            (14, first, None, false, end, true, ids(&[])),
            (15, first, None, false, ran, true, ids(&["0004"])),
            (19, first, None, false, end, true, ids(&[])),
            (21, changed, same, false, ran, false, ids(&["0006"])),
            (30, changed, same, true, covered, true, ids(&[])),
            (31, unchanged, same, false, covered, true, ids(&[])),
            (32, unchanged, same, false, end, true, ids(&[])),
            (33, unchanged, other, false, covered, true, ids(&[])),
            (34, unchanged, other, false, end, true, ids(&[])),
        ],
        "{derivations:#?}"
    );
    let moved = &derivations[9];
    assert_eq!(moved.moved, set(&[0, 1]), "P0 to P2 moved cores 00 and 01");
    assert_eq!(moved.previous.map(Seq::get), Some(15));
    let crashed = &derivations[5];
    assert_eq!(
        crashed
            .reruns
            .iter()
            .map(|t| t.trial.to_string())
            .collect::<Vec<_>>(),
        ["0005"],
        "a rerun of the set is a rerun obligation, not the derivation's work"
    );
    assert!(crashed.trials[0].crash);
    assert_eq!(
        crashed.trials[0].recovery_s,
        Some(60.0),
        "the next boot started 40 s before its first event at 1100 s, 60 s after the trial started at 1000 s"
    );
}

#[test]
fn a_set_rederived_before_it_ran_is_superseded_and_a_pending_one_is_open() {
    let mut log = Log::new(&CORES);
    log.add(chain(0, 1, &[&[0], &[1, 2, 3]], &[1, 2, 3], &P0, None));
    log.add(backoff(1, -20, -19));
    log.add(chain(
        0,
        1,
        &[&[1], &[0, 2, 3]],
        &[0, 2, 3],
        &P1,
        Some(Rederived::Changed),
    ));
    let journal = log.journal();
    let p = Projection::new(&journal);
    let resolutions: Vec<_> = chains::attribute(&p, None)
        .iter()
        .map(|d| (d.resolution, d.held))
        .collect();
    assert_eq!(
        resolutions,
        [(Resolution::Superseded, false), (Resolution::Open, true)]
    );
}

#[test]
fn a_cutoff_keeps_what_a_later_derivation_rederived() {
    let log = rederived_after_backoffs();
    let journal = log.journal();
    let p = Projection::new(&journal);
    let since = journal.events()[29].time;
    let derivations = chains::attribute(&p, Some(since));
    assert_eq!(derivations.first().map(|d| d.seq.get()), Some(30));
    assert_eq!(derivations[0].previous.map(Seq::get), Some(21));
    assert!(
        derivations[0].repeated,
        "the set before the cutoff still counts"
    );
}

#[test]
fn chain_cost_is_reported_by_derivation_and_attribution() {
    let log = rederived_after_backoffs();
    let journal = log.journal();
    let p = Projection::new(&journal);
    let derivations = chains::attribute(&p, None);
    assert_snapshot!(
        "chains__chain_cost_is_reported_by_derivation_and_attribution",
        super::support::rendered(|table| chains::render(table, &derivations))
    );
}

#[test]
fn chain_records_are_one_json_object_a_derivation() {
    let log = rederived_after_backoffs();
    let text = crate::chain_records(&log.journal(), None).unwrap();
    let records: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 11);
    assert_eq!(
        records[5],
        serde_json::json!({
            "seq": 21,
            "cycle": 1,
            "step": 1,
            "ccd": 0,
            "workload": "mprime-avx2-36k-248k-allcore",
            "part": 1,
            "groups": [[1], [0], [2, 3]],
            "cores": [0, 2, 3],
            "sources": [],
            "kind": "changed",
            "previous": 3,
            "moved": [1],
            "change": "same_ccd",
            "repeated": false,
            "resolution": "ran",
            "held": false,
            "trials": [{
                "seq": 25,
                "trial": "0006",
                "duration_s": 15,
                "outcome": "failure",
                "crash": true,
                "recovery_s": 60.0,
            }],
            "reruns": [{
                "seq": 22,
                "trial": "0005",
                "duration_s": 1200,
                "outcome": "pass",
                "crash": false,
            }],
        })
    );
}
