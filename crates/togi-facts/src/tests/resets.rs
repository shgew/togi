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
    let cited = s.add(CommandReset {
        target: ResetTarget::All,
    });
    let uncited = s.add(CommandReset {
        target: ResetTarget::All,
    });
    let archived = s.add_caused(&[cited], SessionArchived { session: first() });
    let after = s.add(CommandReset {
        target: ResetTarget::All,
    });
    let facts = s.extract();
    let archive_effect = vec![Effect {
        seq: archived,
        kind: EventKind::SessionArchived,
    }];
    assert_eq!(facts.resets[0].seq, cited);
    assert_eq!(facts.resets[0].applied, Some(archived));
    assert_eq!(facts.resets[0].effects, archive_effect);
    assert_eq!(facts.resets[1].seq, uncited);
    assert_eq!(facts.resets[1].applied, Some(archived));
    assert_eq!(facts.resets[1].effects, archive_effect);
    assert_eq!(facts.resets[2].seq, after);
    assert_eq!(facts.resets[2].applied, None);
    assert!(facts.resets[2].effects.is_empty());
    assert_eq!(facts.cutoffs.all(), Some(after));
}

#[test]
fn only_the_citing_phase_of_the_reset_core_applies_a_core_reset() {
    let mut s = Script::begin(&[0, 1]);
    let reset = s.add(CommandReset {
        target: ResetTarget::Core(core(0)),
    });
    let other_core = s.add_caused(&[reset], phase(1));
    let facts = s.extract();
    assert_eq!(
        facts.resets[0].effects,
        vec![Effect {
            seq: other_core,
            kind: EventKind::CorePhase
        }]
    );
    assert_eq!(facts.resets[0].applied, None, "another core's phase");

    s.add(phase(0));
    let facts = s.extract();
    assert_eq!(
        facts.resets[0].effects.len(),
        1,
        "uncited phase is no effect"
    );
    assert_eq!(facts.resets[0].applied, None, "uncited phase");

    let citing = s.add_caused(&[reset], phase(0));
    let facts = s.extract();
    assert_eq!(facts.resets[0].effects.len(), 2);
    assert_eq!(facts.resets[0].applied, Some(citing));
}

#[test]
fn latest_reset_per_core_wins() {
    let mut s = Script::begin(&[0, 1]);
    s.add(CommandReset {
        target: ResetTarget::Core(core(0)),
    });
    let core_one = s.add(CommandReset {
        target: ResetTarget::Core(core(1)),
    });
    let latest = s.add(CommandReset {
        target: ResetTarget::Core(core(0)),
    });
    let facts = s.extract();
    assert_eq!(facts.cutoffs.core(core(0)), Some(latest));
    assert_eq!(facts.cutoffs.core(core(1)), Some(core_one));
    assert_eq!(facts.cutoffs.all(), None);
}

#[test]
fn sparse_cutoffs_keep_highest_core_and_missing_cores_independent() {
    let mut s = Script::begin(&[0, 7, 15]);
    let high = s.add(CommandReset {
        target: ResetTarget::Core(core(15)),
    });
    let all = s.add(CommandReset {
        target: ResetTarget::All,
    });
    let low = s.add(CommandReset {
        target: ResetTarget::Core(core(0)),
    });
    let facts = s.extract();
    assert_eq!(facts.cutoffs.core(core(0)), Some(low));
    assert_eq!(facts.cutoffs.core(core(15)), Some(high));
    assert_eq!(facts.cutoffs.core(core(7)), None);
    assert_eq!(facts.cutoffs.core(core(8)), None);
    assert_eq!(facts.cutoffs.all(), Some(all));
    assert_eq!(facts.cores, super::set(&[0, 7, 15]));
}

#[test]
fn archive_without_a_cause_still_applies_the_all_reset() {
    let mut s = Script::begin(&[0]);
    s.add(CommandReset {
        target: ResetTarget::All,
    });
    let archived = s.add(SessionArchived { session: first() });
    let facts = s.extract();
    assert_eq!(facts.resets[0].applied, Some(archived));
    assert_eq!(
        facts.resets[0].effects,
        vec![Effect {
            seq: archived,
            kind: EventKind::SessionArchived
        }]
    );
}
