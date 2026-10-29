//! D permission, allowance and unit-layout fixtures through the complete frame renderer.

use super::super::hand::{self, Hand};
use super::*;
use crate::tests::kit::off;
use togi_journal::{CampaignEventPhase, Classification, ClimbBlocker, DayEventId, UnitStopReason};
use togi_tuner::policy::{
    ClimbDailyEvidence, ClimbEventProjection, ClimbGoalSupport, ClimbProposalProjection,
    ClimbQualificationProjection, ClimbQualificationState, ClimbUnitState,
};
use togi_tuner::unit::{Phase, UnitId};

#[path = "d_expected.rs"]
mod embedded;

#[derive(Clone, Copy, Debug)]
enum Case {
    Provisional,
    Exhausted,
    ZeroS,
    Blocked,
    Censored,
    Finished,
    Sixteen,
}

impl Case {
    const ALL: [Self; 7] = [
        Self::Provisional,
        Self::Exhausted,
        Self::ZeroS,
        Self::Blocked,
        Self::Censored,
        Self::Finished,
        Self::Sixteen,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Provisional => "allowance-provisional",
            Self::Exhausted => "allowance-exhausted",
            Self::ZeroS => "s-zero-permission",
            Self::Blocked => "qualification-blocked",
            Self::Censored => "censored-ceiling",
            Self::Finished => "finished-experimental",
            Self::Sixteen => "sixteen-units",
        }
    }

    fn hand(self) -> Hand {
        let mut hand = Staged::Day.hand();
        let climb = &mut hand.policy.climb;
        let profile = hand::unit_vector(&climb.units);
        climb.daily = Some(ClimbDailyEvidence {
            profile: profile.clone(),
            firmware: Default::default(),
            epoch: 1,
            observed_s: 50 * 3600,
            clean_s: 50 * 3600,
            crashes: 0,
            poisson_upper_per_h: Some(2.9957 / 50.0),
            goal: ClimbGoalSupport::InsufficientEvidence,
            witnesses: vec![togi_journal::Seq::new(3).unwrap()],
        });
        for unit in &mut climb.units {
            unit.hold.credited_s = 50 * 3600;
        }
        match self {
            Self::Provisional | Self::Exhausted => {
                let campaign = climb.campaign.as_mut().unwrap();
                campaign.consumed = if matches!(self, Self::Exhausted) {
                    3
                } else {
                    1
                };
                campaign.remaining = if matches!(self, Self::Exhausted) {
                    0
                } else {
                    1
                };
                campaign.exhausted = matches!(self, Self::Exhausted);
                campaign.provisional = 1;
                campaign.reclassified = u32::from(campaign.exhausted);
                campaign.events.push(ClimbEventProjection {
                    event: DayEventId::Boot {
                        boot: "boot-provisional".to_owned().into(),
                    },
                    aliases: vec![],
                    phase: if campaign.exhausted {
                        CampaignEventPhase::PostFinish
                    } else {
                        CampaignEventPhase::Learning
                    },
                    unit_phase: Phase::Coarse,
                    profile: profile.clone(),
                    provisional: true,
                    classification: if campaign.exhausted {
                        Classification::NonCpu {}
                    } else {
                        Classification::CpuSuspect {
                            certainty: togi_journal::Certainty::Provisional,
                        }
                    },
                    reclassified: campaign.exhausted,
                    witnesses: vec![],
                });
                if campaign.exhausted {
                    for unit in &mut climb.units {
                        unit.state = ClimbUnitState::Finished {
                            reason: UnitStopReason::AllowanceExhausted {},
                            at: FIXED_NOW,
                            profile: profile.clone(),
                            last_clean: unit.applied,
                            witnesses: vec![],
                        };
                    }
                }
            }
            Self::ZeroS => {
                climb.units[0].diagnostic_s.as_mut().unwrap().cap.offset = off(0);
                climb.units[0].ceiling = Some(off(-25));
                climb.units[0].edge = hand::observed(-30);
                climb.units[0].remaining = Some(15);
                let mut candidate = climb.units.clone();
                candidate[0].applied = off(-15);
                let candidate = hand::unit_vector(&candidate);
                climb.profiles.proposed = Some(candidate.clone());
                climb.units[0].next = Some(ClimbProposalProjection {
                    profile: candidate,
                    qualification: ClimbQualificationProjection {
                        state: ClimbQualificationState::Complete {
                            certificate: togi_journal::Seq::new(7).unwrap(),
                        },
                        outstanding: vec![],
                        witnesses: vec![],
                    },
                });
            }
            Self::Blocked => {
                let mut candidate = climb.units.clone();
                candidate[0].applied = off(-15);
                let candidate = hand::unit_vector(&candidate);
                climb.profiles.proposed = Some(candidate.clone());
                climb.units[0].state = ClimbUnitState::Held {
                    reason: vec![ClimbBlocker::CertificateFailed {
                        profile: candidate.clone(),
                        witnesses: vec![],
                    }],
                };
                climb.units[0].next = Some(ClimbProposalProjection {
                    profile: candidate,
                    qualification: ClimbQualificationProjection {
                        state: ClimbQualificationState::Failed { witnesses: vec![] },
                        outstanding: vec![hand::class(togi_machine::Regime::R1, &[0])],
                        witnesses: vec![],
                    },
                });
            }
            Self::Censored => {
                climb.units[0].edge = hand::censored(-50);
                climb.units[0].ceiling = Some(off(-45));
                climb.units[0].remaining = Some(35);
            }
            Self::Finished => {
                for unit in &mut climb.units {
                    unit.applied = unit.ceiling.unwrap();
                    unit.latest_step = None;
                }
                let profile = hand::unit_vector(&climb.units);
                climb.profiles.applied = Some(profile.clone());
                climb.daily.as_mut().unwrap().profile = profile.clone();
                for core in &mut hand.cores {
                    core.profile = profile.cores.get(core.id).unwrap();
                    core.applied = Some(core.profile.observed());
                }
                for unit in &mut climb.units {
                    unit.remaining = Some(0);
                    unit.state = ClimbUnitState::Finished {
                        reason: UnitStopReason::Ceiling {},
                        at: FIXED_NOW,
                        profile: profile.clone(),
                        last_clean: unit.applied,
                        witnesses: vec![],
                    };
                }
            }
            Self::Sixteen => {
                let template = climb.units[0].clone();
                climb.units = hand
                    .cores
                    .iter()
                    .map(|core| {
                        let mut unit = template.clone();
                        unit.unit = UnitId::Core(core.id);
                        unit.phase = Phase::Core;
                        unit.parent = Some(UnitId::Ccd(core.ccd));
                        unit.cores = [core.id].into_iter().collect();
                        unit.applied = core.profile;
                        unit.inherited_start = core.profile;
                        unit.remaining = Some(u32::try_from(core.profile.get() + 26).unwrap());
                        unit.latest_step = None;
                        unit
                    })
                    .collect();
                climb.profiles.applied = Some(hand::unit_vector(&climb.units));
                climb.daily.as_mut().unwrap().profile = hand::unit_vector(&climb.units);
            }
        }
        hand
    }
}

fn visit_case(case: Case, visit: &mut impl FnMut(&str, &Buffer, Form)) {
    let hand = case.hand();
    for size in MAIN_SIZES {
        for (suffix, colours) in [
            ("none", Colours::None),
            ("console", Colours::Console),
            ("full", Colours::Full),
            ("rich", Colours::Rich),
        ] {
            let screen = screen_of(View::Main, size, colours, 0, false);
            let (buf, _) = render_checked(&hand.journal(), &screen, hand.now);
            visit(
                &format!("main-{}-{}x{}-{suffix}", case.name(), size.0, size.1),
                &buf,
                Form::Styled(colours),
            );
        }
    }
}

pub(super) fn frames(visit: &mut impl FnMut(&str, &Buffer, Form)) {
    for case in Case::ALL {
        visit_case(case, visit);
    }
    permission_frames(visit);
}

fn permission_frames(visit: &mut impl FnMut(&str, &Buffer, Form)) {
    for (staged, view, prefix) in [
        (Staged::Day, View::Main, "main-day"),
        (Staged::Mapping, View::Main, "main-mapping"),
        (Staged::Qualifying, View::Main, "main-qualifying"),
        (Staged::Mapping, View::Help, "help"),
    ] {
        let hand = staged.hand();
        for size in MAIN_SIZES {
            for (suffix, colours) in [
                ("none", Colours::None),
                ("console", Colours::Console),
                ("full", Colours::Full),
                ("rich", Colours::Rich),
            ] {
                let screen = screen_of(view, size, colours, 0, false);
                let (buf, _) = render_checked(&hand.journal(), &screen, hand.now);
                visit(
                    &format!("{prefix}-{}x{}-{suffix}", size.0, size.1),
                    &buf,
                    Form::Styled(colours),
                );
            }
        }
    }
}

#[test_case::test_case(Case::Provisional; "provisional allowance")]
#[test_case::test_case(Case::Exhausted; "exhausted allowance and post finish event")]
#[test_case::test_case(Case::ZeroS; "s zero does not authorize or block")]
#[test_case::test_case(Case::Blocked; "qualification failure is held unfinished")]
#[test_case::test_case(Case::Censored; "censored physical limit has minus45 ceiling")]
#[test_case::test_case(Case::Finished; "finished remains experimental")]
#[test_case::test_case(Case::Sixteen; "sixteen core units retain physical ccds")]
fn d_state_snapshots(case: Case) {
    visit_case(case, &mut checked);
    let hand = case.hand();
    let text = super::super::story::climb_lines(&hand.policy).join("\n");
    assert!(text.contains("permission: experimental hold + exact night qualification"));
    assert!(text.contains(
        "experimental: 50.000 clean hours; at most 1 crash per 17 h (95%, fixed-rate model)"
    ));
    assert!(!text.contains("stable") && !text.contains("meets your goal"));
    match case {
        Case::Provisional => {
            assert!(text.contains("1 consumed / 2 limit; 1 remaining · 1 provisional"))
        }
        Case::Exhausted => {
            assert!(text.contains("3 consumed / 2 limit; 0 remaining"));
            assert!(
                text.contains("post-finish event")
                    && text.contains("reclassified; allowance not refunded")
            );
            assert!(text.contains("finished: allowance exhausted"));
        }
        Case::ZeroS => assert!(text.contains("proposed, not applied: 0:-15")),
        Case::Blocked => {
            assert!(text.contains("held/unfinished: exact candidate qualification blocked"))
        }
        Case::Finished => assert!(
            text.contains("finished: night ceiling reached")
                && text.contains("remains experimental")
        ),
        Case::Censored | Case::Sixteen => {}
    }
}

#[test]
fn permission_views_at_every_size_and_colour() {
    permission_frames(&mut checked);
}

#[test]
fn sixteen_core_units_are_all_visible_without_collapsing_them_to_ccds() {
    let hand = Case::Sixteen.hand();
    let (buf, _) = hand.main(240, 67);
    let text = rows_of(&buf).join("\n");
    for unit in &hand.policy.climb.units {
        let UnitId::Core(core) = unit.unit else {
            panic!("core fixture")
        };
        assert!(
            text.contains(&format!("core {} · D", core.get())),
            "core {core}: {text}"
        );
    }
    assert!(text.contains("CCD 0") && text.contains("CCD 1"));
}

pub(super) fn expected(name: &str) -> &'static str {
    embedded::expected(name)
}
