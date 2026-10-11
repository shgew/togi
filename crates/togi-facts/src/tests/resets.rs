use togi_journal::{
    CommandReset, CorePhase, CoreTransition, EventKind, ResetTarget, SessionArchived,
};
use togi_machine::Signal;

use super::{Script, alone, core, fail, first, offset, pass};
use crate::{Decisive, Effect, Observation, Reset};

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
    let applied = s.add_caused(&[core_reset], phase(0));
    s.add(alone("after", 0, -5, &[0]));
    s.add(fail("after", Signal::ComputationError, 1));
    let all_reset = s.add(CommandReset {
        target: ResetTarget::All,
    });
    let archived = s.add_caused(&[all_reset], SessionArchived { session: first() });
    let facts = s.extract();
    assert_eq!(
        facts.resets,
        vec![
            Reset {
                seq: core_reset,
                target: ResetTarget::Core(core(0)),
                effects: vec![Effect {
                    seq: applied,
                    kind: EventKind::CorePhase
                }],
                applied: Some(applied),
            },
            Reset {
                seq: all_reset,
                target: ResetTarget::All,
                effects: vec![Effect {
                    seq: archived,
                    kind: EventKind::SessionArchived
                }],
                applied: Some(archived),
            },
        ]
    );
    assert_eq!(facts.cutoffs.core(core(0)), Some(core_reset));
    assert_eq!(facts.cutoffs.core(core(1)), None);
    assert_eq!(facts.cutoffs.all(), Some(all_reset));
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

#[test]
fn archive_applies_outstanding_all_resets_without_double_effect() {
    let mut s = Script::begin(&[0]);
    let all_reset = s.add(CommandReset {
        target: ResetTarget::All,
    });
    let archived = s.add_caused(&[all_reset], SessionArchived { session: first() });
    let other = s.add(CommandReset {
        target: ResetTarget::All,
    });
    let facts = s.extract();
    assert_eq!(facts.resets[0].applied, Some(archived));
    assert_eq!(facts.resets[0].effects.len(), 1);
    assert_eq!(facts.resets[1].seq, other);
    assert_eq!(facts.resets[1].applied, None);
    assert_eq!(facts.cutoffs.all(), Some(other));
}

#[test]
fn latest_reset_per_core_wins() {
    let mut s = Script::begin(&[0, 1]);
    s.add(CommandReset {
        target: ResetTarget::Core(core(0)),
    });
    s.add(CommandReset {
        target: ResetTarget::Core(core(1)),
    });
    let latest = s.add(CommandReset {
        target: ResetTarget::Core(core(0)),
    });
    let facts = s.extract();
    assert_eq!(facts.cutoffs.core(core(0)), Some(latest));
    assert_eq!(facts.cutoffs.all(), None);
}
