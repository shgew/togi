use serde_json::json;
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
