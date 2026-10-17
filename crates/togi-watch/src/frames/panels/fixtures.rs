//! Fixtures for the panel tests: the views the model lends, built by hand.

use std::num::NonZeroU32;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use ratatui::text::Line;
use togi_journal::{HuntStage, Member, TrialId};
use togi_machine::{CcdId, CoreSet, ObservedOffset, Regime, workloads};
use togi_tuner::{
    BiosProfile, Confirmed, CyclePart, CyclePlan, CycleStep, GroupState, HuntGroup, HuntPart,
    HuntPlan, MemberProbe, PhaseNo, PhasePlan, Profile, RerunPlan, Topology, Unconfirmed,
};

use super::super::fit::plain;
use super::super::scene::Scene;
use crate::model::{
    CoreEvidence, CoreState, CoreView, Count, HuntCause, HuntFacts, Snapshot, TrialView,
};
use crate::screen::Colours;
use crate::tests::kit::{core, infos, off, offsets, seq, set};

/// The rows as the screen shows them, without their style.
pub(super) fn texts(lines: &[Line<'_>]) -> Vec<String> {
    lines.iter().map(plain).collect()
}

pub(super) fn at(second: i64) -> Timestamp {
    Timestamp::from_second(second).expect("a timestamp")
}

pub(super) fn core_view(id: u8, ccd: u8) -> CoreView {
    CoreView {
        id: core(id),
        ccd: CcdId(ccd),
        state: CoreState::Waiting,
        queued: None,
        applied: Some(ObservedOffset(0)),
        profile: off(0),
        solo: None,
        pass: None,
        next: None,
        fail: None,
        holder: None,
        gave_back: None,
        group_fails: Vec::new(),
        returns_to: None,
        confirm: None,
        loaded: false,
        judged: false,
        evidence: CoreEvidence::default(),
    }
}

pub(super) fn trial_view(index: u32, of: u32) -> TrialView {
    TrialView {
        id: TrialId::from("t1".to_owned()),
        regime: Regime::R7,
        workload: workloads(Regime::R7)[0].id(),
        cores: set(&[0, 1]),
        parked: CoreSet::default(),
        idle: CoreSet::default(),
        condition: togi_machine::Condition::Together,
        activity: None,
        profile: offsets(&[-20, -20]),
        started: Some(at(1000)),
        crashed: false,
        duration: Duration::from_secs(600),
        partial: false,
        rerun: false,
        retry: false,
        cycle: None,
        hunt: None,
        core: None,
        offset: None,
        round: None,
        count: Count {
            passed: index.saturating_sub(1),
            index,
            of,
        },
    }
}

pub(super) fn group(number: u32, cores: &[u8], outcome: GroupState) -> HuntGroup {
    HuntGroup {
        number,
        cores: set(cores),
        profile: offsets(&[]),
        stage: HuntStage::Full,
        probe: None,
        held: Vec::new(),
        outcome,
        passed: 0,
        needed: 5,
        carried: false,
        running: outcome == GroupState::Running,
    }
}

pub(super) fn part(
    failing: &[u8],
    parked: &[u8],
    group: Option<u32>,
    outcome: Option<GroupState>,
    running: bool,
) -> HuntPart {
    HuntPart {
        failing: set(failing),
        parked: set(parked),
        trials: 5,
        duration_s: 600,
        group,
        outcome,
        running,
    }
}

pub(super) fn probe(member: u8, offset: i64, running: bool, done: bool) -> MemberProbe {
    MemberProbe {
        member: core(member),
        offset: off(offset),
        failed_at: Vec::new(),
        passed_at: Vec::new(),
        carried: Vec::new(),
        running,
        done,
    }
}

pub(super) fn member(id: u8, offset: i64) -> Member {
    Member {
        core: core(id),
        offset: off(offset),
    }
}

pub(super) fn hunt_plan(groups: Vec<HuntGroup>, parts: Vec<HuntPart>) -> HuntPlan {
    HuntPlan {
        number: 1,
        regime: Regime::R7,
        failure: seq(1),
        trial: None,
        candidates: set(&[0, 1, 2]),
        cut: 0,
        parts,
        groups,
        probes: Vec::new(),
        rerun: RerunPlan {
            regime: Regime::R7,
            cores: set(&[0, 1, 2]),
            short: 5,
            short_s: 600,
            long: 0,
            long_s: 0,
        },
    }
}

pub(super) fn hunt_facts() -> HuntFacts {
    HuntFacts {
        started: Some(at(1000)),
        cause: HuntCause {
            at: None,
            trial: None,
            end: None,
            signal: None,
            core: None,
            known: false,
            carried: false,
            regime: None,
            rerun_of: false,
            trial_number: 0,
        },
        parking: None,
        group_signals: Vec::new(),
    }
}

pub(super) fn cycle_part(
    cores: &[u8],
    ccd: Option<u8>,
    full: bool,
    running: bool,
    done: bool,
) -> CyclePart {
    CyclePart {
        cores: set(cores),
        ccd: ccd.map(CcdId),
        full,
        partial: !full,
        short: 1,
        short_s: 600,
        long: 0,
        long_s: 0,
        passed: u32::from(done),
        failed: 0,
        done,
        running,
    }
}

pub(super) fn cycle_step(
    regime: Regime,
    parts: Vec<CyclePart>,
    done: bool,
    chains_complete: bool,
) -> CycleStep {
    CycleStep {
        regime,
        workload: workloads(regime)[0].id(),
        parts,
        chains_complete,
        done,
    }
}

pub(super) fn cycle_plan(steps: Vec<CycleStep>, current: usize, paused: bool) -> CyclePlan {
    CyclePlan {
        number: NonZeroU32::new(2),
        open: true,
        steps,
        current,
        paused,
    }
}

/// `s` as a plain frame at second 2000 in `zone`.
pub(super) fn scene<'a>(s: &Snapshot<'a>, zone: &'a TimeZone) -> Scene<'a> {
    Scene::new(*s, at(2000), zone, Colours::None)
}

pub(super) fn topology() -> Topology {
    Topology::new(&infos(&[(0, 0), (1, 0), (8, 1)])).expect("a topology")
}

/// The profile to enter in BIOS over cores 0, 1 and 8: confirmed by the cycle ending #12, and with core 1 held
/// at -25 since #15 when `unconfirmed`.
pub(super) fn bios(unconfirmed: bool) -> BiosProfile {
    let topology = topology();
    let profile = |values: &[i64]| {
        let offsets = [0u8, 1, 8]
            .iter()
            .zip(values)
            .map(|(&id, &value)| (core(id), off(value)))
            .collect();
        Profile::new(&topology, offsets).expect("a profile")
    };
    BiosProfile {
        confirmed: Some(Confirmed {
            profile: profile(&[-20, -20, -10]),
            seq: seq(12),
        }),
        shown: profile(&[-20, -25, -10]),
        unconfirmed: if unconfirmed {
            vec![Unconfirmed {
                core: core(1),
                since: seq(15),
            }]
        } else {
            Vec::new()
        },
        first_result: false,
    }
}

pub(super) fn phases(round: Option<u32>) -> PhasePlan {
    PhasePlan {
        phase: PhaseNo::Two,
        phase1_end: Some(seq(10)),
        concluded: None,
        confirming: false,
        rounds_left: 2,
        round,
        candidates: Vec::new(),
    }
}
