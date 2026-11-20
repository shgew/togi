use test_case::test_case;
use togi_journal::{Event, EventKind, MemFs, Payload};
use togi_simrun::Persistence;
use togi_simrun::scripted::Error;
use togi_tool_sim::scripted::ScriptedCase;

use crate::scripted::{execute, night_milestones};

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
