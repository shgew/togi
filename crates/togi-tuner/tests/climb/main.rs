#![expect(
    clippy::unwrap_used,
    reason = "test fixtures assert valid domain values"
)]

mod allowance;
mod firmware;
mod hold_step;
mod units;

use std::collections::BTreeSet;

use jiff::Timestamp;
use togi_machine::{BootId, CcdId, CoreId, CoreInfo, TargetOffset};
use togi_tuner::Topology;
use togi_tuner::climb::{
    self, Campaign, Ceiling, Ceilings, ClimbAction, ClimbDecision, ClimbHistory, ClimbInputs,
    EdgeState, EventPhase, EvidenceId, HoldInput,
};
use togi_tuner::day::{
    Classification, CpuCause, DailyEvent, EventId, Firmware, Freeze, HOLD_SECS, Invocation, Pause,
    QuestionState, vetoes,
};
use togi_tuner::unit::{UnitId, UnitProfile, Units};

fn offset(value: i64) -> TargetOffset {
    TargetOffset::new(value).unwrap()
}
fn evidence() -> EvidenceId {
    EvidenceId::new(1).unwrap()
}
fn now() -> Timestamp {
    Timestamp::from_second(1).unwrap()
}
fn observed(value: i64) -> Ceiling {
    Ceiling::new(EdgeState::Observed {
        offset: offset(value),
        witness: evidence(),
    })
}
fn topology() -> Topology {
    Topology::new(
        &(0..16)
            .map(|id| CoreInfo {
                core: CoreId::new(id).unwrap(),
                ccd: CcdId::new(id / 8).unwrap(),
                cpus: Vec::new(),
            })
            .collect::<Vec<_>>(),
    )
    .unwrap()
}
fn coarse() -> Units {
    Units::coarse(&topology())
}
fn cores() -> Units {
    Units::cores(&topology())
}
fn ids(units: &Units) -> Vec<UnitId> {
    units.iter().map(|unit| unit.id).collect()
}
fn profile(units: &Units, values: &[i64]) -> UnitProfile {
    assert_eq!(units.iter().count(), values.len());
    UnitProfile::try_from(
        units
            .iter()
            .zip(values)
            .map(|(unit, &value)| (unit.id, offset(value)))
            .collect::<Vec<_>>(),
    )
    .unwrap()
}
fn cpu() -> Classification {
    Classification::CpuSuspect(CpuCause::ManualRecord)
}

struct Fixture {
    history: ClimbHistory,
    campaign: Campaign,
    ceilings: Ceilings,
    events: Vec<DailyEvent>,
    qualified: BTreeSet<UnitProfile>,
    failed: BTreeSet<UnitProfile>,
    hold: u64,
    firmware: Firmware,
    freeze: Freeze,
    invocation: Invocation,
    pending_question: bool,
    unresolved_event: bool,
    apply: climb::ApplyPrerequisites,
    application: u64,
}

impl Fixture {
    fn new(units: Units) -> Self {
        let ceilings = units.iter().map(|unit| (unit.id, observed(-30))).collect();
        Self {
            history: ClimbHistory::stock(units),
            campaign: Campaign::approved_fresh(),
            ceilings,
            events: Vec::new(),
            qualified: BTreeSet::new(),
            failed: BTreeSet::new(),
            hold: HOLD_SECS,
            firmware: Firmware::Current,
            freeze: Freeze::Off,
            invocation: Invocation::ColdBoot {
                pause: Pause::Running,
            },
            pending_question: false,
            unresolved_event: false,
            apply: climb::ApplyPrerequisites {
                firmware_valid: true,
                activation_open: true,
                safe_apply: true,
            },
            application: 0,
        }
    }

    fn decide(&self) -> ClimbDecision {
        let vetoes = vetoes(&self.events);
        climb::decide(&ClimbInputs {
            history: &self.history,
            campaign: &self.campaign,
            ceilings: &self.ceilings,
            events: &self.events,
            vetoes: &vetoes,
            qualified: &self.qualified,
            failed: &self.failed,
            hold: HoldInput {
                profile: self.history.current(),
                credited_secs: self.hold,
            },
            firmware: self.firmware,
            invocation: self.invocation,
            freeze: self.freeze,
            pending_question: self.pending_question,
            unresolved_event: self.unresolved_event,
            apply: self.apply,
        })
        .unwrap()
    }

    fn candidate(&self) -> UnitProfile {
        climb::propose(
            self.history.current(),
            &self.ceilings,
            self.history.finishes(),
        )
        .unwrap()
        .profile
    }

    fn apply(&mut self, decision: &ClimbDecision) {
        self.application += 1;
        self.history
            .apply(
                climb::ApplicationId::new(self.application).unwrap(),
                Timestamp::from_second(i64::try_from(self.application).unwrap()).unwrap(),
                decision,
            )
            .unwrap();
        self.hold = 0;
    }

    fn advance(&mut self) {
        self.hold = HOLD_SECS;
        self.qualified.insert(self.candidate());
        let decision = self.decide();
        assert!(
            matches!(decision.action, ClimbAction::Promote(_)),
            "{decision:?}"
        );
        self.apply(&decision);
    }

    fn event(&mut self, name: &str, class: Classification) -> EventId {
        let event = DailyEvent {
            id: EventId::Boot(BootId::from(name.to_owned())),
            profile: self.history.actual(self.history.current()).unwrap(),
            witnesses: Vec::new(),
            question: QuestionState::NotAsked,
            class,
        };
        let id = event.id.clone();
        let phase = if self
            .history
            .current()
            .iter()
            .all(|(unit, _)| self.history.finishes().is_finished(unit))
        {
            EventPhase::PostFinish
        } else {
            EventPhase::Learning
        };
        self.campaign
            .observe(&event, &[], phase, self.history.units().phase());
        self.events.push(event);
        id
    }

    fn apply_retreat(&mut self, wanted: &[i64]) -> ClimbDecision {
        self.qualified.insert(profile(self.history.units(), wanted));
        let decision = self.decide();
        assert!(
            matches!(decision.action, ClimbAction::Retreat(_)),
            "{decision:?}"
        );
        assert_eq!(
            decision.target(),
            Some(&profile(self.history.units(), wanted))
        );
        self.apply(&decision);
        decision
    }

    fn firmware_zero(&mut self) -> UnitProfile {
        let saved = self.history.current().clone();
        self.firmware = Firmware::Changed;
        let decision = self.decide();
        assert!(matches!(decision.action, ClimbAction::FirmwareZero { .. }));
        self.apply(&decision);
        self.firmware = Firmware::Current;
        self.qualified.clear();
        saved
    }
}

fn blocked(decision: &ClimbDecision, wanted: &climb::Blocker) -> bool {
    match &decision.action {
        ClimbAction::Hold { blocked_by } => blocked_by.contains(wanted),
        ClimbAction::FirmwareZero { .. }
        | ClimbAction::Retreat(_)
        | ClimbAction::Restore(_)
        | ClimbAction::Promote(_) => false,
    }
}
