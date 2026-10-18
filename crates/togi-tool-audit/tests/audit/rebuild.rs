use serde_json::json;
use togi_tool_audit::Check::{Replay, Termination};
use togi_tool_audit::Completion::{Finished, InProgress};

use crate::fixtures::*;

fn rebuilt(seq: u64, fields: &[&str]) -> togi_journal::Event {
    event(seq, "state.rebuilt", json!({"fields": fields}))
}

pub(crate) fn snapshot() -> serde_json::Value {
    json!({
        "start_offsets": {},
        "candidate_solo_limits": {},
        "durations": {
            "search_trial_s": 90,
            "short_trial_s": 120,
            "checking_trial_s": 120,
            "checking_idle_s": 900,
            "checking_all_core_s": 1200
        },
        "evidence": {"miss": 0.05, "rate": 0.5},
        "checking": {"cycle": ["R7", "R2", "R1"]},
        "dead_ends": {"inconclusive_in_a_row": 3, "stray_crashes_in_a_row": 3},
        "backends": {"mprime": null, "ycruncher": null},
        "backend_user": null
    })
}

fn config_loaded(seq: u64, fixes: u32) -> togi_journal::Event {
    event(
        seq,
        "config.loaded",
        json!({"build": build(fixes), "config": snapshot(), "path": "", "file": false}),
    )
}

#[test_case::test_case(case("no rebuild", vec![phase(2, 0, None)], InProgress, &[]); "no rebuild")]
#[test_case::test_case(case(
            "a snapshot behind the journal is stale, not wrong",
            vec![rebuilt(2, &["cores", "last_seq"])],
            InProgress,
            &[],
        ); "a snapshot behind the journal is stale, not wrong")]
#[test_case::test_case(case(
            "a snapshot behind the journal naming only last_seq is stale too",
            vec![rebuilt(2, &["last_seq"])],
            InProgress,
            &[],
        ); "a snapshot behind the journal naming only last_seq is stale too")]
#[test_case::test_case(case(
            "a saved tuner projection that disagrees is a violation",
            vec![rebuilt(2, &["tuner"]), stop(3)],
            InProgress,
            &[(2, Replay)],
        ); "a saved tuner projection that disagrees is a violation")]
#[test_case::test_case(case(
            "the same build disagreeing with its own snapshot",
            vec![rebuilt(2, &["cores"]), config_loaded(3, 0)],
            InProgress,
            &[(2, Replay)],
        ); "the same build disagreeing with its own snapshot")]
#[test_case::test_case(case(
            "a build change explains the disagreement",
            vec![rebuilt(2, &["bios", "phases"]), config_loaded(3, 1)],
            InProgress,
            &[],
        ); "a build change explains the disagreement")]
#[test_case::test_case(case(
            "a run that shuts down without recording its build",
            vec![rebuilt(2, &["cores"]), stop(3)],
            InProgress,
            &[(2, Replay)],
        ); "a run that shuts down without recording its build")]
#[test_case::test_case(case(
            "a run that dead-ends without recording its build",
            vec![rebuilt(2, &["cores"]), dead_end(3, "d")],
            InProgress,
            &[(2, Replay)],
        ); "a run that dead-ends without recording its build")]
#[test_case::test_case(case(
            "a run archived without recording its build",
            vec![rebuilt(2, &["cores"]), archived(3)],
            InProgress,
            &[(2, Replay)],
        ); "a run archived without recording its build")]
#[test_case::test_case(case(
            "a later boot without the build",
            vec![rebuilt(2, &["cores"]), boot(phase(3, 0, None), "c")],
            InProgress,
            &[(2, Replay)],
        ); "a later boot without the build")]
#[test_case::test_case(case(
            "a finished journal ending on one",
            vec![rebuilt(2, &["cores"])],
            Finished,
            &[(2, Replay), (2, Termination)],
        ); "a finished journal ending on one")]
#[test_case::test_case(case(
            "an open journal may be read before its build is recorded",
            vec![rebuilt(2, &["cores"])],
            InProgress,
            &[],
        ); "an open journal may be read before its build is recorded")]
#[test_case::test_case(case(
            "a stale snapshot then shutdown",
            vec![rebuilt(2, &["cores", "last_seq"]), stop(3)],
            InProgress,
            &[],
        ); "a stale snapshot then shutdown")]
fn a_rebuild_disagreeing_with_a_current_snapshot_is_a_violation(case: Case) {
    run(case);
}
