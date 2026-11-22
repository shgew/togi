use std::num::NonZeroU32;
use std::path::PathBuf;

use test_case::test_case;
use togi_journal::{Event, EventKind, MemFs, Payload};
use togi_simrun::Persistence;
use togi_simrun::scripted::Error;
use togi_tool_sim::scripted::{CONTRACT_VERSION, ScriptedCase};

use crate::ScriptedArgs;
use crate::log::Log;
use crate::scripted::{execute, night_milestones, run};

#[test_case(ScriptedCase::MappingStop, 2; "above the bound")]
#[test_case(ScriptedCase::MceResume, 1; "below the bound")]
fn a_case_refuses_any_boot_bound_but_its_own_before_touching_state(case: ScriptedCase, boots: u32) {
    let state = PathBuf::from("/nonexistent/togi-scripted-state");
    let args = ScriptedArgs {
        case,
        contract: CONTRACT_VERSION.to_owned(),
        state_dir: state.clone(),
        max_boots: NonZeroU32::new(boots).unwrap(),
    };
    assert_eq!(run(args, &mut Log::buffered(Vec::new())), 1);
    assert!(!state.exists());
}

#[test]
fn a_case_refuses_a_journal_it_did_not_start_and_leaves_it_alone() {
    let persistence = Persistence::Memory(MemFs::new());
    execute(ScriptedCase::MappingStop, &persistence).unwrap();
    let before = togi_simrun::events(&persistence).unwrap();
    assert!(matches!(
        execute(ScriptedCase::MappingStop, &persistence),
        Err(Error::Milestone(_))
    ));
    assert_eq!(togi_simrun::events(&persistence).unwrap(), before);
}

fn uncite_the_crash(event: &mut Event) {
    if event.kind() == EventKind::CrashDetected {
        event.cause.clear();
    }
}

fn forget_the_machine_check_boot(event: &mut Event) {
    if let Payload::Mce(mce) = &mut event.payload {
        mce.from_boot = None;
    }
}

#[test_case(uncite_the_crash; "the crash cites no machine check")]
#[test_case(forget_the_machine_check_boot; "the machine check names no boot")]
fn the_machine_check_case_fails_unless_its_crash_owns_the_interrupted_trials_machine_check(
    damage: fn(&mut Event),
) {
    let persistence = Persistence::Memory(MemFs::new());
    execute(ScriptedCase::MceResume, &persistence).unwrap();
    let mut events = togi_simrun::events(&persistence).unwrap();
    night_milestones(ScriptedCase::MceResume, &events).unwrap();
    events.iter_mut().for_each(damage);
    assert!(matches!(
        night_milestones(ScriptedCase::MceResume, &events),
        Err(Error::Milestone(_))
    ));
}
