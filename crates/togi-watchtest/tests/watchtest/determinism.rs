use std::io::{BufRead, BufReader};

use flate2::read::MultiGzDecoder;
use togi_journal::{Event, Payload};
use togi_watchtest::{Base, Scenario};

fn assert_same_journal(first: &[Event], second: &[Event], label: impl std::fmt::Debug) {
    assert_eq!(first.len(), second.len(), "{label:?}");
    for (first, second) in first.iter().zip(second) {
        assert_eq!(
            first.encode().expect("an event encodes"),
            second.encode().expect("an event encodes"),
            "{label:?}: #{}",
            first.seq.get()
        );
    }
}

fn identical_when_built_twice(base: Base, compressed_fixture: &[u8]) {
    let afresh = base.simulate().expect("the first base simulates");
    let again = base.simulate().expect("the second base simulates");
    assert_same_journal(&afresh, &again, base);
    let shared = base.events().expect("the base fixture decodes");
    assert_same_journal(&afresh, shared, base);

    // Check the actual uncompressed fixture bytes too, including every newline and the exact end of the file.
    let mut fixture = BufReader::new(MultiGzDecoder::new(compressed_fixture));
    let mut line = String::new();
    for event in &afresh {
        line.clear();
        fixture.read_line(&mut line).expect("read a fixture line");
        let mut expected = event.encode().expect("an event encodes");
        expected.push('\n');
        assert_eq!(line, expected, "{base:?}: #{}", event.seq.get());
    }
    assert_eq!(
        fixture.read_line(&mut line).expect("read the fixture end"),
        0,
        "{base:?}: the fixture has no extra bytes"
    );
    for scenario in Scenario::ALL
        .into_iter()
        .filter(|scenario| scenario.base() == base)
    {
        let first = scenario
            .events_from(&afresh)
            .expect("the scenario builds from the first simulation");
        let second = scenario
            .events_from(&again)
            .expect("the scenario builds from the second simulation");
        assert!(!first.is_empty(), "{scenario}");
        assert_same_journal(&first, &second, scenario);
        let fixture = scenario.events().expect("the scenario fixture decodes");
        assert_same_journal(&first, &fixture, scenario);
    }
}

macro_rules! determinism {
    ($($test:ident: $base:ident,)+) => {
        $(
            #[test]
            fn $test() {
                identical_when_built_twice(
                    Base::$base,
                    include_bytes!(concat!("../../src/fixtures/", stringify!($base), ".jsonl.gz")),
                );
            }
        )+
    };
}

determinism! {
    night_and_its_scenarios_build_byte_identical_journals_twice: Night,
    mapped_and_its_scenarios_build_byte_identical_journals_twice: Mapped,
    filler_and_its_scenarios_build_byte_identical_journals_twice: Filler,
    dead_end_and_its_scenarios_build_byte_identical_journals_twice: DeadEnd,
    calendar_and_its_scenarios_build_byte_identical_journals_twice: Calendar,
}

#[test_case::test_case(Scenario::Controls; "controls")]
#[test_case::test_case(Scenario::Recovering; "recovering")]
#[test_case::test_case(Scenario::Stopped; "stopped")]
fn every_scenario_names_the_fixed_build(scenario: Scenario) {
    let want = togi_watchtest::build();
    assert_eq!(want.version, "0.0.0");
    assert_eq!(want.rev, None);
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
        | Payload::TaskStart(_)
        | Payload::CommandReset(_)
        | Payload::DefectFound(_)
        | Payload::DefectAnswered(_)
        | Payload::DeadEnd(_)
        | Payload::BootSavedEntry(_)
        | Payload::BootLeaveReason(_)
        | Payload::Shutdown(_)
        | Payload::JournalTorn(_)
        | Payload::DayBootStart(_)
        | Payload::DayCheckpoint(_)
        | Payload::DayOffsetsChanged(_)
        | Payload::DayCleanStop(_)
        | Payload::DayAnswer(_)
        | Payload::DayRecord(_)
        | Payload::DayFault(_)
        | Payload::SessionImported(_)
        | Payload::ExposureObserved(_)
        | Payload::CcdEdge(_)
        | Payload::CcdCap(_)
        | Payload::NightPlan(_)
        | Payload::NightQualification(_)
        | Payload::NightReproduction(_)
        | Payload::DayStep(_)
        | Payload::DayApplied(_)
        | Payload::DayEvent(_)
        | Payload::DayVeto(_)
        | Payload::DayHold(_)
        | Payload::DayFreeze(_)
        | Payload::DayInterval(_)
        | Payload::DayPause(_)
        | Payload::DayImported(_)
        | Payload::UnitMapping(_)
        | Payload::UnitEdge(_)
        | Payload::ClimbDecision(_)
        | Payload::UnitStop(_)
        | Payload::CampaignStart(_)
        | Payload::CampaignCharge(_)
        | Payload::CampaignReclassified(_)
        | Payload::FirmwareRestoration(_)
        | Payload::SuccessorImported(_)
        | Payload::DayUncleanEnd(_)
        | Payload::StateRebuilt(_) => None,
    });
    assert!(builds.all(|build| *build == want), "{scenario}");
}
