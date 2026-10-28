use togi_journal::{CommandReset, CorePhase, CoreTransition, ResetTarget, SessionArchived};
use togi_machine::Signal;

use super::{Script, alone, core, fail, first, offset, pass};
use crate::{Decisive, Observation};

fn phase(core_id: i64) -> CoreTransition {
    CoreTransition {
        core: core(core_id),
        from: None,
        to: CorePhase::Search,
        offset: offset(0),
        pass: None,
        failure_point: None,
        check_solo_limit: false,
        workloads: Vec::new(),
        cleared_combination: Vec::new(),
        reason: String::new(),
    }
}

#[test]
fn reset_history_preserves_facts() {
    let mut s = Script::begin(&[0]);
    s.add(alone("before", 0, -10, &[0]));
    s.add(pass("before", 1));
    let core_reset = s.add(CommandReset {
        target: ResetTarget::Core(core(0)),
    });
    s.add_caused(&[core_reset], phase(0));
    s.add(alone("after", 0, -5, &[0]));
    s.add(fail("after", Signal::ComputationError, 1));
    let all_reset = s.add(CommandReset {
        target: ResetTarget::All,
    });
    s.add_caused(&[all_reset], SessionArchived { session: first() });
    let facts = s.extract();
    let kept: Vec<_> = facts
        .facts
        .iter()
        .map(|fact| match &fact.observation {
            Observation::Trial {
                outcome: Decisive::Pass,
                ..
            } => ("pass", fact.profile().get(core(0))),
            Observation::Trial { .. } => ("failure", fact.profile().get(core(0))),
            Observation::IdleFailure { .. } => unreachable!(),
        })
        .collect();
    assert_eq!(
        kept,
        vec![("pass", Some(offset(-10))), ("failure", Some(offset(-5)))]
    );
}
