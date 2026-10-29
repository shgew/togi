use serde_json::json;
use togi_tool_audit::Check::{Avoidance, Exposure, Qualification};
use togi_tool_audit::Completion::InProgress;

use crate::fixtures::*;

#[test_case::test_case(case(
            "a nonzero profile after its complete qualification",
            vec![qualification(2, [-20, 0], "F31", "complete"), day_applied(3, [-20, 0])],
            InProgress,
            &[],
        ); "a nonzero profile after its complete qualification")]
#[test_case::test_case(case(
            "CO 0 needs no qualification",
            vec![day_applied(2, [0, 0])],
            InProgress,
            &[],
        ); "CO 0 needs no qualification")]
#[test_case::test_case(case(
            "a nonzero profile nobody qualified",
            vec![day_applied(2, [-20, 0])],
            InProgress,
            &[(2, Qualification)],
        ); "a nonzero profile nobody qualified")]
#[test_case::test_case(case(
            "the qualification is of another profile",
            vec![qualification(2, [-15, 0], "F31", "complete"), day_applied(3, [-20, 0])],
            InProgress,
            &[(3, Qualification)],
        ); "the qualification is of another profile")]
#[test_case::test_case(case(
            "a qualification still in progress confers nothing",
            vec![qualification(2, [-20, 0], "F31", "start"), day_applied(3, [-20, 0])],
            InProgress,
            &[(3, Qualification)],
        ); "a qualification still in progress confers nothing")]
#[test_case::test_case(case(
            "the qualification must precede the application",
            vec![day_applied(2, [-20, 0]), qualification(3, [-20, 0], "F31", "complete")],
            InProgress,
            &[(2, Qualification)],
        ); "the qualification must precede the application")]
#[test_case::test_case(case(
            "a qualification under the firmware in force",
            vec![
                context(2, "F31"),
                qualification(3, [-20, 0], "F31", "complete"),
                day_applied(4, [-20, 0]),
            ],
            InProgress,
            &[],
        ); "a qualification under the firmware in force")]
#[test_case::test_case(case(
            "a qualification under another firmware",
            vec![
                context(2, "F31"),
                qualification(3, [-20, 0], "F30", "complete"),
                day_applied(4, [-20, 0]),
            ],
            InProgress,
            &[(4, Qualification)],
        ); "a qualification under another firmware")]
#[test_case::test_case(case(
            "a firmware change after the qualification",
            vec![
                context(2, "F31"),
                qualification(3, [-20, 0], "F31", "complete"),
                context(4, "F32"),
                day_applied(5, [-20, 0]),
            ],
            InProgress,
            &[(5, Qualification)],
        ); "a firmware change after the qualification")]
fn a_nonzero_applied_day_profile_was_qualified_under_the_firmware_in_force(case: Case) {
    run(case);
}

#[test_case::test_case(case(
            "a promotion to the vetoed profile",
            vec![veto(2, 12, [-10, -10], "active"), day_step(3, [-5, -10], [-10, -10], true)],
            InProgress,
            &[(3, Avoidance)],
        ); "a promotion to the vetoed profile")]
#[test_case::test_case(case(
            "a promotion deeper than the vetoed profile",
            vec![veto(2, 12, [-10, -10], "active"), day_step(3, [-10, -10], [-15, -10], true)],
            InProgress,
            &[(3, Avoidance)],
        ); "a promotion deeper than the vetoed profile")]
#[test_case::test_case(case(
            "a promotion that keeps one CCD shallower than the veto",
            vec![veto(2, 12, [-10, -10], "active"), day_step(3, [0, -10], [-5, -10], true)],
            InProgress,
            &[],
        ); "a promotion that keeps one CCD shallower than the veto")]
#[test_case::test_case(case(
            "a hold at the vetoed profile proposes no promotion",
            vec![veto(2, 12, [-10, -10], "active"), day_step(3, [-10, -10], [-10, -10], false)],
            InProgress,
            &[],
        ); "a hold at the vetoed profile proposes no promotion")]
#[test_case::test_case(case(
            "a cleared veto forbids nothing",
            vec![
                veto(2, 12, [-10, -10], "active"),
                veto(3, 12, [-10, -10], "cleared"),
                day_step(4, [-5, -10], [-10, -10], true),
            ],
            InProgress,
            &[],
        ); "a cleared veto forbids nothing")]
#[test_case::test_case(case(
            "a veto recorded after the promotion does not reach back",
            vec![day_step(2, [-5, -10], [-10, -10], true), veto(3, 12, [-10, -10], "active")],
            InProgress,
            &[],
        ); "a veto recorded after the promotion does not reach back")]
#[test_case::test_case(case(
            "a nonuniform veto is compared on every physical core",
            vec![
                event(
                    2,
                    "day.veto",
                    json!({
                        "event": {"kind": "record", "session": "20260101T000000Z", "seq": 12},
                        "profile": {"kind": "per_core", "cores": {"0": -15, "8": -5}},
                        "state": "active",
                        "evidence": [],
                        "reason": "r",
                    }),
                ),
                day_step(3, [-10, 0], [-15, -5], true),
                day_step(4, [-10, 0], [-15, -4], true),
            ],
            InProgress,
            &[(3, Avoidance)],
        ); "a nonuniform veto is compared on every physical core")]
fn a_promotion_respects_every_active_veto(case: Case) {
    run(case);
}

#[test_case::test_case(case(
            "a climb promotion to the vetoed profile",
            vec![veto(2, 12, [-10, -10], "active"), climb(3, [-5, -10], [-10, -10], "promote")],
            InProgress,
            &[(3, Avoidance)],
        ); "a climb promotion to the vetoed profile")]
#[test_case::test_case(case(
            "a climb promotion deeper than the vetoed profile",
            vec![veto(2, 12, [-10, -10], "active"), climb(3, [-10, -10], [-15, -10], "promote")],
            InProgress,
            &[(3, Avoidance)],
        ); "a climb promotion deeper than the vetoed profile")]
#[test_case::test_case(case(
            "a climb promotion that keeps one CCD shallower than the veto",
            vec![veto(2, 12, [-10, -10], "active"), climb(3, [0, -10], [-5, -10], "promote")],
            InProgress,
            &[],
        ); "a climb promotion that keeps one CCD shallower than the veto")]
#[test_case::test_case(case(
            "a climb hold at the vetoed profile proposes no promotion",
            vec![veto(2, 12, [-10, -10], "active"), climb(3, [-10, -10], [-10, -10], "hold")],
            InProgress,
            &[],
        ); "a climb hold at the vetoed profile proposes no promotion")]
#[test_case::test_case(case(
            "a firmware restoration into the vetoed profile",
            vec![veto(2, 12, [-10, -10], "active"), climb(3, [0, 0], [-10, -10], "restore")],
            InProgress,
            &[(3, Avoidance)],
        ); "a firmware restoration into the vetoed profile")]
#[test_case::test_case(case(
            "a cleared veto forbids no climb promotion",
            vec![
                veto(2, 12, [-10, -10], "active"),
                veto(3, 12, [-10, -10], "cleared"),
                climb(4, [-5, -10], [-10, -10], "promote"),
            ],
            InProgress,
            &[],
        ); "a cleared veto forbids no climb promotion")]
fn a_climb_decision_respects_every_active_veto(case: Case) {
    run(case);
}

#[test_case::test_case(case(
            "an exposure citing its intent and end",
            vec![
                trial_intent(2, "0001", "R8", "alone", &[0]),
                trial_end(3, "0001"),
                cause(exposure(4, "0001", "live"), &[2, 3]),
            ],
            InProgress,
            &[],
        ); "an exposure citing its intent and end")]
#[test_case::test_case(case(
            "an exposure citing nothing",
            vec![
                trial_intent(2, "0001", "R8", "alone", &[0]),
                trial_end(3, "0001"),
                exposure(4, "0001", "live"),
            ],
            InProgress,
            &[(4, Exposure)],
        ); "an exposure citing nothing")]
#[test_case::test_case(case(
            "an exposure citing an event that is no trial's",
            vec![
                trial_intent(2, "0001", "R8", "alone", &[0]),
                cause(exposure(3, "0001", "live"), &[1]),
            ],
            InProgress,
            &[(3, Exposure)],
        ); "an exposure citing an event that is no trial's")]
#[test_case::test_case(case(
            "an exposure naming another trial than the one it cites",
            vec![
                trial_intent(2, "0001", "R8", "alone", &[0]),
                trial_end(3, "0001"),
                cause(exposure(4, "0002", "live"), &[2, 3]),
            ],
            InProgress,
            &[(4, Exposure)],
        ); "an exposure naming another trial than the one it cites")]
#[test_case::test_case(case(
            "an imported exposure's source lies in another session",
            vec![exposure(2, "0001", "imported")],
            InProgress,
            &[],
        ); "an imported exposure's source lies in another session")]
fn a_live_exposure_cites_the_trial_that_produced_it(case: Case) {
    run(case);
}
