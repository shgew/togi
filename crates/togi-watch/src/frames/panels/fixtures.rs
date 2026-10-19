//! Fixtures for the panel tests: the views the model lends, built by hand.

use std::num::NonZeroU32;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use ratatui::text::Line;
use togi_journal::Member;
use togi_machine::{CcdId, Regime, workloads};
use togi_tuner::{
    BiosProfile, Confirmed, CyclePart, CyclePlan, CycleStep, GroupState, HuntGroup, HuntPart,
    HuntPlan, Lengths, MemberProbe, PhaseNo, PhasePlan, Profile, Quota, RerunPlan, Topology,
    Unconfirmed,
};

use super::super::fit::plain;
use super::super::scene::Scene;
use crate::model::{CoreState, CoreView, Count, HuntFacts, Snapshot, TrialView};
use crate::screen::Colours;
use crate::tests::fixtures;
use crate::tests::kit::{core, infos, off, offsets, seq, set};

/// The rows as the screen shows them, without their style.
pub(super) fn texts(lines: &[Line<'_>], colours: Colours) -> Vec<String> {
    lines.iter().map(|line| plain(line, colours)).collect()
}

pub(super) fn at(second: i64) -> Timestamp {
    Timestamp::from_second(second).expect("a timestamp")
}

pub(super) fn core_view(id: u8, ccd: u8) -> CoreView {
    fixtures::core_view(id, ccd, CoreState::Waiting)
}

pub(super) fn trial_view(index: u32, of: u32) -> TrialView {
    TrialView {
        cores: set(&[0, 1]),
        profile: offsets(&[-20, -20]),
        started: Some(at(1000)),
        duration: Duration::from_secs(600),
        count: Count {
            passed: index.saturating_sub(1),
            index,
            of,
        },
        ..fixtures::trial_view(Regime::R7)
    }
}

pub(super) fn group(number: u32, cores: &[u8], outcome: GroupState) -> HuntGroup {
    fixtures::hunt_group(number, cores, outcome)
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
        parts,
        groups,
        rerun: RerunPlan {
            regime: Regime::R7,
            cores: set(&[0, 1, 2]),
            lengths: Lengths {
                short: Quota {
                    count: 5,
                    secs: 600,
                },
                long: None,
            },
        },
        ..fixtures::hunt_plan(&[0, 1, 2])
    }
}

pub(super) fn hunt_facts() -> HuntFacts {
    fixtures::hunt_facts(Some(at(1000)))
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
        ccd: ccd.map(|ccd| CcdId::try_from(ccd).unwrap()),
        full,
        partial: !full,
        lengths: Some(Lengths {
            short: Quota {
                count: 1,
                secs: 600,
            },
            long: None,
        }),
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
