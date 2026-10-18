use test_case::test_case;
use togi_journal::{CorePhase, CoreTransition, Payload};
use togi_machine::Condition;
use togi_tuner::Profile;

use super::{Flow, SessionError};
use crate::testing::{Bench, begun, core, kinds_of, run_plan, search_plan, target};
use crate::write::{ConstraintRefusal, ProfileFailure, Refused};

#[test_case(ProfileFailure::Refused(Refused::Reaches {
        core: core(0),
        target: target(-20),
        refusal: ConstraintRefusal::Reached(togi_tuner::Reached::FailurePoint {
            core: core(0),
            point: togi_tuner::FailurePoint::new(target(-10)),
        }),
    }); "refused constraint")]
#[test_case(ProfileFailure::Shape {
        profile: 2,
        cores: 4,
    }; "misshapen profile")]
fn a_refused_or_misshapen_profile_is_a_fatal_session_error_never_dead_end_evidence(
    failure: ProfileFailure,
) {
    let text = failure.to_string();
    let Flow::Fatal(SessionError::Profile(kept)) = Flow::from(failure) else {
        panic!("{text}: not a fatal profile error");
    };
    assert_eq!(kept.to_string(), text);
}

#[test]
fn a_refused_profile_ends_the_run_with_the_refusal_starts_no_trial_and_claims_nothing_applied() {
    let bench = Bench::new(2);
    let mut session = begun(&bench);
    session
        .appender
        .append(
            Payload::CorePhase(CoreTransition {
                core: core(0),
                from: Some(CorePhase::Search),
                to: CorePhase::Search,
                offset: target(-9),
                pass: None,
                failure_point: Some(target(-10)),
                check_solo_limit: false,
                workloads: Vec::new(),
                cleared_combination: Vec::new(),
                reason: "a confirmed failure point".into(),
            }),
            &[],
        )
        .unwrap();
    let mut plan = search_plan(&session, core(0), togi_machine::Regime::R1);
    plan.condition = Condition::Parked;
    plan.profile = Profile::alone(
        session.appender().tuner().topology().unwrap(),
        core(0),
        target(-20),
    );
    let before = kinds_of(session.appender());

    let error = run_plan(&mut session, plan).unwrap_err();

    let SessionError::Profile(ProfileFailure::Refused(Refused::Reaches {
        core: refused,
        target: wanted,
        refusal,
    })) = &error
    else {
        panic!("not the refusal: {error:?}");
    };
    assert_eq!((*refused, *wanted), (core(0), target(-20)));
    assert!(refusal.to_string().contains("-10"), "{refusal}");
    assert_eq!(kinds_of(session.appender()), before);
    assert!(session.appender().failure().is_none());

    let ended = session.close(Err(error));

    assert!(ended.stop.is_none(), "{:?}", ended.stop);
    assert!(
        matches!(
            ended.errors.first(),
            Some(SessionError::Profile(ProfileFailure::Refused(_)))
        ),
        "{:?}",
        ended.errors
    );
    let events = ended.journal.events();
    assert_eq!(
        events.len(),
        before.len(),
        "the refusal and the cleanup that followed appended {:?}",
        events[before.len()..]
            .iter()
            .map(togi_journal::Event::kind)
            .collect::<Vec<_>>()
    );
    let mut smu = bench.sim.seams().smu;
    for n in 0..2 {
        assert_eq!(smu.offset(core(n)).unwrap().0, 0, "core {n}");
    }
}

#[test]
fn distinct_causes_with_identical_text_are_both_rendered() {
    #[derive(Debug, thiserror::Error)]
    #[error("read")]
    struct Read(#[source] std::io::Error);

    let error = Read(std::io::Error::other("read"));
    assert_eq!(togi_machine::error_chain(&error), "read: read");
}
