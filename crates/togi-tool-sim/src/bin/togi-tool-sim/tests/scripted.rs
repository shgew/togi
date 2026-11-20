use togi_journal::{EventKind, MemFs};
use togi_simrun::Persistence;
use togi_tool_sim::scripted::ScriptedCase;

use crate::scripted::{execute, machine_check_attributed};

#[test]
fn the_machine_check_case_needs_the_crash_to_cite_the_interrupted_trials_machine_check() {
    let persistence = Persistence::Memory(MemFs::new());
    execute(ScriptedCase::MceResume, &persistence).unwrap();
    let mut events = togi_simrun::events(&persistence).unwrap();
    assert!(machine_check_attributed(&events));
    for event in &mut events {
        if event.kind() == EventKind::CrashDetected {
            event.cause.clear();
        }
    }
    assert!(!machine_check_attributed(&events));
}
