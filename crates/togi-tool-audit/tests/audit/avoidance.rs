use serde_json::json;
use togi_journal::Event;
use togi_tool_audit::Check;
use togi_tool_audit::Check::Avoidance;
use togi_tool_audit::Completion::InProgress;

use crate::fixtures::*;

#[test]
fn failure_points_and_combinations_are_avoided_by_applied_profiles() {
    run(vec![
        case(
            "one count shallower than a point is safe",
            vec![phase(2, 0, Some(-30)), applied(3, [-29, 0])],
            InProgress,
            &[],
        ),
        case(
            "a profile at a point reaches it",
            vec![phase(2, 0, Some(-30)), applied(3, [-30, 0])],
            InProgress,
            &[(3, Avoidance)],
        ),
        case(
            "a failure records its point, and a restore deeper than it reaches it",
            vec![failure(2, 0, -30), restored(3, [-31, 0])],
            InProgress,
            &[(3, Avoidance)],
        ),
        case(
            "a profile applied before the point is recorded is not judged by it",
            vec![applied(2, [-30, 0]), phase(3, 0, Some(-30))],
            InProgress,
            &[],
        ),
        case(
            "a profile shallower than one member of a combination is safe",
            vec![
                combination(2, 1, &[(0, -30), (8, -20)]),
                applied(3, [-31, -19]),
            ],
            InProgress,
            &[],
        ),
        case(
            "a profile reaching every member reaches the combination",
            vec![
                combination(2, 1, &[(0, -30), (8, -20)]),
                applied(3, [-31, -20]),
            ],
            InProgress,
            &[(3, Avoidance)],
        ),
        case(
            "a carried failure point binds the new session",
            vec![
                event(
                    2,
                    "session.carried",
                    json!({"sources": [], "failure_points": true, "carried": [{"core": 8, "failure_point": {"offset": -10, "from": {"session": "20251231T000000Z", "seq": 7}, "signal": "stall"}}]}),
                ),
                applied(3, [0, -10]),
            ],
            InProgress,
            &[(3, Avoidance)],
        ),
        case(
            "failure points are not carried across a BIOS change",
            vec![
                event(
                    2,
                    "session.carried",
                    json!({"sources": [], "failure_points": false, "carried": [{"core": 8, "failure_point": {"offset": -20, "from": {"session": "20251231T000000Z", "seq": 7}, "signal": "stall"}}]}),
                ),
                applied(3, [0, -20]),
            ],
            InProgress,
            &[],
        ),
        case(
            "an SMU write one count shallower than a point is safe",
            vec![
                phase(2, 0, Some(-30)),
                intent(3, set(0, -29)),
                write(4, set(0, -29), 3),
                readback(5, 0, -29, Some(4)),
            ],
            InProgress,
            &[],
        ),
        case(
            "an SMU write at a point reaches it",
            vec![
                phase(2, 0, Some(-30)),
                intent(3, set(0, -30)),
                write(4, set(0, -30), 3),
                readback(5, 0, -30, Some(4)),
            ],
            InProgress,
            &[(4, Avoidance)],
        ),
    ]);
}

#[test]
fn the_audit_keeps_its_own_failure_history() {
    run(vec![
        case(
            "failure points accumulate when the tuner's projection omits one",
            vec![
                failure(2, 0, -20),
                decision(3, 0, "backoff", None),
                applied(4, [-20, 0]),
            ],
            InProgress,
            &[(4, Avoidance)],
        ),
        case(
            "the shallowest point wins",
            vec![failure(2, 0, -20), failure(3, 0, -30), applied(4, [-25, 0])],
            InProgress,
            &[(4, Avoidance)],
        ),
        case(
            "a hunt culprit records its failing offset",
            vec![
                event(
                    2,
                    "hunt.start",
                    json!({"hunt": 1, "failure": 1, "regime": "R1", "workload": "mprime-avx2-36k-248k", "cores": [0, 8], "duration_s": 60, "failing": offsets([0, -20]), "parked": offsets([0, 0]), "candidates": [0, 8], "trials": 2, "trial_s": 60, "miss": 0.1, "rate": 0.1, "ranking": [0, 8]}),
                ),
                event(
                    3,
                    "hunt.end",
                    json!({"hunt": 1, "result": "culprit", "cores": [8], "groups": 1, "reason": "r"}),
                ),
                applied(4, [0, -20]),
            ],
            InProgress,
            &[(4, Avoidance)],
        ),
        case(
            "a hunt that names no single culprit records no point",
            vec![
                event(
                    2,
                    "hunt.start",
                    json!({"hunt": 1, "failure": 1, "regime": "R1", "workload": "mprime-avx2-36k-248k", "cores": [0, 8], "duration_s": 60, "failing": offsets([-20, -20]), "parked": offsets([0, 0]), "candidates": [0, 8], "trials": 2, "trial_s": 60, "miss": 0.1, "rate": 0.1, "ranking": [0, 8]}),
                ),
                event(
                    3,
                    "hunt.end",
                    json!({"hunt": 1, "result": "combination", "cores": [0, 8], "groups": 1, "reason": "r"}),
                ),
                applied(4, [-20, -20]),
            ],
            InProgress,
            &[],
        ),
        case(
            "a combination is judged against sparse ids as the SMU writes reach it",
            vec![
                event(2, "session.baseline", json!({"offsets": offsets([0, 0])})),
                combination(3, 3, &[(0, -30), (8, -20)]),
                intent(4, set(0, -30)),
                write(5, set(0, -30), 4),
                readback(6, 0, -30, Some(5)),
                intent(7, set(8, -20)),
                write(8, set(8, -20), 7),
                readback(9, 8, -20, Some(8)),
            ],
            InProgress,
            &[(8, Avoidance)],
        ),
    ]);
}

#[test]
fn a_held_failure_records_no_point() {
    run(vec![
        case(
            "the hold cancels the failure's point",
            vec![
                failure(2, 0, -20),
                cause(decision(3, 0, "hold", None), &[2]),
                applied(4, [-20, 0]),
            ],
            InProgress,
            &[],
        ),
        case(
            "a backoff keeps it",
            vec![
                failure(2, 0, -20),
                cause(decision(3, 0, "backoff", None), &[2]),
                applied(4, [-20, 0]),
            ],
            InProgress,
            &[(4, Avoidance)],
        ),
        case(
            "the failure that pairs with a hold records its own point",
            vec![
                failure(2, 0, -20),
                cause(decision(3, 0, "hold", None), &[2]),
                failure(4, 0, -22),
                cause(decision(5, 0, "backoff", None), &[4]),
                applied(6, [-22, 0]),
            ],
            InProgress,
            &[(6, Avoidance)],
        ),
    ]);
}

#[test]
fn a_failure_counts_against_the_core_its_decision_charges() {
    run(vec![
        case(
            "charged to another core, the named core keeps no point",
            vec![
                failure(2, 8, -20),
                cause(decision(3, 0, "backoff", Some(-30)), &[2]),
                applied(4, [-25, -20]),
            ],
            InProgress,
            &[],
        ),
        case(
            "the charged core's own point stands",
            vec![
                failure(2, 8, -20),
                cause(decision(3, 0, "backoff", Some(-30)), &[2]),
                applied(4, [-30, 0]),
            ],
            InProgress,
            &[(4, Avoidance)],
        ),
        case(
            "charged to its named core once, the point survives a later charge elsewhere",
            vec![
                failure(2, 8, -20),
                cause(decision(3, 8, "backoff", None), &[2]),
                cause(decision(4, 0, "backoff", Some(-30)), &[2]),
                applied(5, [-25, -20]),
            ],
            InProgress,
            &[(5, Avoidance)],
        ),
    ]);
}

fn carried_source() -> serde_json::Value {
    json!({
        "session": "20251231T000000Z",
        "seq": 7,
        "build": build(0),
        "time": "2026-01-01T00:00:00Z",
        "boot": "b",
    })
}

fn carried_trial(cores: &[u8]) -> Vec<Event> {
    vec![event(
        2,
        "trial.carried",
        json!({
            "source": carried_source(),
            "class": {"regime": "R7", "cores": cores, "duration_s": 60},
            "condition": "together",
            "profile": offsets([-30, 0]),
            "outcome": "failure",
            "duration_s": 60,
        }),
    )]
}

fn carried_failure(cores: &[u8]) -> Vec<Event> {
    vec![event(
        2,
        "failure.carried",
        json!({
            "source": carried_source(),
            "class": {"regime": "R7", "cores": cores, "duration_s": 60},
            "failure": {"signal": "computation_error", "attribution": "attributed"},
        }),
    )]
}

fn earlier_failure(cores: &[u8]) -> Vec<Event> {
    vec![
        trial_intent(2, "0001", "R7", "together", cores),
        failure_with(
            3,
            json!({"core": 9, "offset": -5, "trial": "0001", "regime": "R7", "condition": "together"}),
        ),
    ]
}

/// A multi-core R7 failure of core 8 that cites the last of `source`, a decision of core 0 that cites the failure,
/// and a profile at core 8's point; core 0 is charged in place of core 8 only if the source's cores include it.
fn known_failure_case(name: &'static str, source: Vec<Event>, moves: bool) -> Case {
    let cited = source.len() as u64 + 1;
    let mut events = source;
    events.push(cause(
        failure_with(
            cited + 1,
            json!({"core": 8, "offset": -20, "regime": "R7", "condition": "together", "known_failure": cited}),
        ),
        &[cited],
    ));
    events.push(cause(decision(cited + 2, 0, "backoff", None), &[cited + 1]));
    events.push(applied(cited + 3, [-25, -20]));
    let want: &[(u64, Check)] = if moves {
        &[]
    } else {
        &[(cited + 3, Avoidance)]
    };
    case(name, events, InProgress, want)
}

#[test]
fn a_failure_moves_to_another_core_only_on_grounds_the_journal_records() {
    let r7 = |loaded: &[u8], rest: Vec<Event>| {
        let mut events = vec![
            trial_intent(2, "0001", "R7", "together", loaded),
            failure_with(
                3,
                json!({"core": 8, "offset": -20, "trial": "0001", "regime": "R7", "condition": "together"}),
            ),
        ];
        events.extend(rest);
        events
    };
    run(vec![
        case(
            "a search backoff of another core that records no point leaves the alone trial's point",
            vec![
                failure(2, 8, -20),
                cause(decision_in(3, "search", 0, "backoff", None), &[2]),
                applied(4, [-25, -20]),
            ],
            InProgress,
            &[(4, Avoidance)],
        ),
        case(
            "a checking backoff of another core that records no point leaves it too",
            vec![
                failure(2, 8, -20),
                cause(decision(3, 0, "backoff", None), &[2]),
                applied(4, [-25, -20]),
            ],
            InProgress,
            &[(4, Avoidance)],
        ),
        case(
            "a phase-2 decision charging another core moves it",
            vec![
                failure(2, 8, -20),
                cause(decision_in(3, "deepening", 0, "yield", None), &[2]),
                applied(4, [-25, -20]),
            ],
            InProgress,
            &[],
        ),
        case(
            "a multi-core R7 failure moves to a core its trial loaded",
            r7(
                &[0, 8],
                vec![
                    cause(decision(4, 0, "backoff", None), &[3]),
                    applied(5, [-25, -20]),
                ],
            ),
            InProgress,
            &[],
        ),
        case(
            "a multi-core R7 failure does not move to a core its trial left idle",
            r7(
                &[8, 9],
                vec![
                    cause(decision(4, 0, "backoff", None), &[3]),
                    applied(5, [-25, -20]),
                ],
            ),
            InProgress,
            &[(5, Avoidance)],
        ),
        known_failure_case(
            "a known failure takes its loaded cores from a trial.carried, so the charge moves to a core in it",
            carried_trial(&[0, 8]),
            true,
        ),
        known_failure_case(
            "a known failure's trial.carried class leaves a core outside it unmoved",
            carried_trial(&[8, 9]),
            false,
        ),
        known_failure_case(
            "a known failure takes its loaded cores from a failure.carried, so the charge moves to a core in it",
            carried_failure(&[0, 8]),
            true,
        ),
        known_failure_case(
            "a known failure's failure.carried class leaves a core outside it unmoved",
            carried_failure(&[8, 9]),
            false,
        ),
        known_failure_case(
            "a known failure takes its loaded cores from an earlier failure's trial, so the charge moves to a core in it",
            earlier_failure(&[0, 8]),
            true,
        ),
        known_failure_case(
            "a known failure's earlier failure's trial leaves a core outside it unmoved",
            earlier_failure(&[8, 9]),
            false,
        ),
    ]);
}

#[test]
fn a_reset_clears_the_marks_once_search_restarts() {
    run(vec![
        case(
            "search restarting after a reset clears the point and the combinations naming the core",
            vec![
                phase(2, 0, Some(-30)),
                combination(3, 1, &[(0, -30), (8, -20)]),
                reset_core(4, 0),
                search(5, 0),
                applied(6, [-30, -20]),
            ],
            InProgress,
            &[],
        ),
        case(
            "a reset only queued has cleared nothing",
            vec![
                phase(2, 0, Some(-30)),
                reset_core(3, 0),
                applied(4, [-30, 0]),
            ],
            InProgress,
            &[(4, Avoidance)],
        ),
        case(
            "a reset of another core clears nothing of this one",
            vec![
                phase(2, 0, Some(-30)),
                reset_core(3, 8),
                search(4, 8),
                applied(5, [-30, 0]),
            ],
            InProgress,
            &[(5, Avoidance)],
        ),
    ]);
}

#[test]
fn a_multi_core_r7_failure_naming_a_core_at_zero_counts_only_once_a_dead_end_cites_it() {
    let parked = trial_intent(2, "0001", "R7", "parked", &[0, 8]);
    let named = failure_with(
        3,
        json!({"core": 8, "offset": 0, "trial": "0001", "regime": "R7", "condition": "parked"}),
    );
    run(vec![
        case(
            "routed to the unloaded cores, it records no point",
            vec![parked.clone(), named.clone(), applied(4, [-30, -1])],
            InProgress,
            &[],
        ),
        case(
            "a dead end citing it records the point at zero",
            vec![
                parked.clone(),
                named,
                cause(
                    event(
                        4,
                        "deadend",
                        json!({"condition": "failure_at_zero", "core": 8, "detail": "core 08 failed at CO 0"}),
                    ),
                    &[3],
                ),
                applied(5, [-30, -1]),
            ],
            InProgress,
            &[(5, Avoidance)],
        ),
        case(
            "a single-core R7 failure at zero records it at once",
            vec![
                trial_intent(2, "0001", "R7", "together", &[8]),
                failure_with(
                    3,
                    json!({"core": 8, "offset": 0, "trial": "0001", "regime": "R7", "condition": "together"}),
                ),
                applied(4, [-30, -1]),
            ],
            InProgress,
            &[(4, Avoidance)],
        ),
        case(
            "a named core off zero records its point",
            vec![
                failure_with(
                    2,
                    json!({"core": 8, "offset": -20, "trial": "0001", "regime": "R7", "condition": "parked"}),
                ),
                applied(3, [0, -20]),
            ],
            InProgress,
            &[(3, Avoidance)],
        ),
        case(
            "a known failure takes its loaded cores from the trial it cites",
            vec![
                trial_intent(2, "0001", "R7", "parked", &[0, 8]),
                trial_end(3, "0001"),
                cause(
                    failure_with(
                        4,
                        json!({"core": 8, "offset": 0, "regime": "R7", "condition": "parked", "known_failure": 3}),
                    ),
                    &[3],
                ),
                applied(5, [-30, -1]),
            ],
            InProgress,
            &[],
        ),
    ]);
}

#[test]
fn zeroing_is_never_a_deepening_into_a_failure_point() {
    run(vec![
        case(
            "restoring zero after a failure at zero is safe",
            vec![failure(2, 0, 0), restored(3, [0, 0])],
            InProgress,
            &[],
        ),
        case(
            "one count deeper than a point at zero is not",
            vec![failure(2, 0, 0), applied(3, [-1, 0])],
            InProgress,
            &[(3, Avoidance)],
        ),
        case(
            "zeroing every core is safe against a combination of members at zero",
            vec![
                combination(2, 1, &[(0, 0), (8, 0)]),
                intent(3, set_all(0)),
                write(4, set_all(0), 3),
                readback(5, 0, 0, Some(4)),
                readback(6, 8, 0, Some(4)),
            ],
            InProgress,
            &[],
        ),
    ]);
}
