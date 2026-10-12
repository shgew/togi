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
}

#[test]
fn every_scenario_names_the_fixed_build() {
    let want = togi_watchtest::build();
    assert_eq!(want.version, "0.0.0");
    assert_eq!(want.rev, None);
    for scenario in [Scenario::Search, Scenario::Recovering, Scenario::Confirm] {
        let events = scenario.events().expect("the scenario builds");
        let mut builds = events.iter().filter_map(|event| match &event.payload {
            togi_journal::Payload::SessionStart(start) => Some(&start.build),
            togi_journal::Payload::ConfigLoaded(loaded) => Some(&loaded.build),
            _ => None,
        });
        assert!(builds.all(|build| *build == want), "{scenario}");
    }
}
