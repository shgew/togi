use togi_journal::Payload;
use togi_watchtest::{Base, Scenario};

use crate::support::lines;

fn identical_when_built_twice(base: Base) {
    let afresh = base.simulate().expect("the base simulates");
    let shared = base.events().expect("the shared base simulates");
    assert_eq!(lines(&afresh), lines(shared));
    for scenario in Scenario::ALL
        .into_iter()
        .filter(|scenario| scenario.base() == base)
    {
        let first = scenario.events().expect("the scenario builds");
        let second = scenario
            .events_from(&afresh)
            .expect("the scenario builds from the second simulation");
        assert!(!first.is_empty(), "{scenario}");
        assert_eq!(lines(&first), lines(&second), "{scenario}");
    }
}

macro_rules! determinism {
    ($($test:ident: $base:ident,)+) => {
        $(
            #[test]
            fn $test() {
                identical_when_built_twice(Base::$base);
            }
        )+
    };
}

determinism! {
    session_and_its_scenarios_build_byte_identical_journals_twice: Session,
    concluded_and_its_scenarios_build_byte_identical_journals_twice: Concluded,
    unconfirmed_and_its_scenarios_build_byte_identical_journals_twice: Unconfirmed,
    later_hunt_and_its_scenarios_build_byte_identical_journals_twice: LaterHunt,
    first_result_and_its_scenarios_build_byte_identical_journals_twice: FirstResult,
    probe_and_its_scenarios_build_byte_identical_journals_twice: Probe,
    combination_and_its_scenarios_build_byte_identical_journals_twice: Combination,
    crashed_cycle_and_its_scenarios_build_byte_identical_journals_twice: CrashedCycle,
    dead_end_and_its_scenarios_build_byte_identical_journals_twice: DeadEnd,
}

#[test]
fn every_scenario_names_the_fixed_build() {
    let want = togi_watchtest::build();
    assert_eq!(want.version, "0.0.0");
    assert_eq!(want.rev, None);
    for scenario in [Scenario::Search, Scenario::Recovering, Scenario::Confirm] {
        let events = scenario.events().expect("the scenario builds");
        let mut builds = events.iter().filter_map(|event| match &event.payload {
            Payload::SessionStart(start) => Some(&start.build),
            Payload::ConfigLoaded(loaded) => Some(&loaded.build),
            Payload::SessionContext(_)
            | Payload::SessionBaseline(_)
            | Payload::SessionNotice(_)
            | Payload::SessionWarning(_)
            | Payload::SessionArchived(_)
            | Payload::SessionCarried(_)
            | Payload::PreflightCheck(_)
            | Payload::SmuIntent(_)
            | Payload::SmuWrite(_)
            | Payload::SmuReadback(_)
            | Payload::SmuError(_)
            | Payload::ProfileApplied(_)
            | Payload::ProfileChange(_)
            | Payload::ProfileRestored(_)
            | Payload::TrialIntent(_)
            | Payload::TrialStart(_)
            | Payload::TrialProgress(_)
            | Payload::TrialSignal(_)
            | Payload::TrialSample(_)
            | Payload::TrialEnd(_)
            | Payload::TrialCarried(_)
            | Payload::Failure(_)
            | Payload::FailureCarried(_)
            | Payload::Mce(_)
            | Payload::CrashDetected(_)
            | Payload::TunerDecision(_)
            | Payload::CorePhase(_)
            | Payload::CheckingCycle(_)
            | Payload::CheckingStep(_)
            | Payload::CheckingChain(_)
            | Payload::HostRanking(_)
            | Payload::HuntStart(_)
            | Payload::HuntGroup(_)
            | Payload::HuntEnd(_)
            | Payload::HuntSkipped(_)
            | Payload::Combination(_)
            | Payload::DeepeningRound(_)
            | Payload::TunerWarning(_)
            | Payload::BackendRetry(_)
            | Payload::CommandReset(_)
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::DeadEnd(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::Shutdown(_)
            | Payload::JournalTorn(_)
            | Payload::StateRebuilt(_) => None,
        });
        assert!(builds.all(|build| *build == want), "{scenario}");
    }
}
