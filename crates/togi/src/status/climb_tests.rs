use jiff::tz::TimeZone;
use test_case::test_case;
use togi_journal::{
    CampaignEventPhase, CampaignHistory, CampaignId, Certainty, Classification, ClimbBlocker,
    DayEventId, Edge, Provenance, Seq, UnitMapping, UnitMembership, UnitPhase, UnitStopReason,
    UnitVector,
};
use togi_machine::{BiosContext, CcdId, CoreId, ObservedOffset, TargetOffset};
use togi_render::{OutputFacts, Renderer, Style};
use togi_tuner::{
    CapProjection, ClimbCampaignProjection, ClimbDailyEvidence, ClimbEventProjection,
    ClimbGoalSupport, ClimbHoldProjection, ClimbProfiles, ClimbProjection, ClimbProposalProjection,
    ClimbQualificationProjection, ClimbQualificationState, ClimbUnitProjection, ClimbUnitState,
    DiagnosticStatisticalProjection, Phase, UnitId,
};

use super::policy::Where;

fn seq(n: u64) -> Seq {
    Seq::new(n).unwrap()
}
fn offset(n: i64) -> TargetOffset {
    TargetOffset::new(n).unwrap()
}
fn core(n: i64) -> CoreId {
    CoreId::new(n).unwrap()
}
fn vector(values: &[i64]) -> UnitVector {
    UnitVector {
        cores: values
            .iter()
            .enumerate()
            .map(|(id, &value)| (core(i64::try_from(id).unwrap()), offset(value)))
            .collect(),
        mapping: UnitMapping {
            phase: UnitPhase::Core,
            units: values
                .iter()
                .enumerate()
                .map(|(id, _)| UnitMembership {
                    unit: togi_journal::UnitId::Core(core(i64::try_from(id).unwrap())),
                    cores: [core(i64::try_from(id).unwrap())].into_iter().collect(),
                    parent: Some(togi_journal::UnitId::Ccd(CcdId::new(0).unwrap())),
                })
                .collect(),
        },
    }
}

fn fixture() -> ClimbProjection {
    let profile = vector(&[-5]);
    ClimbProjection {
        profiles: ClimbProfiles {
            applied: Some(profile.clone()),
            read_back: Some([(core(0), ObservedOffset(-5))].into_iter().collect()),
            proposed: None,
            paused_saved: None,
        },
        units: vec![ClimbUnitProjection {
            unit: UnitId::Core(core(0)),
            phase: Phase::Core,
            parent: Some(UnitId::Ccd(CcdId::new(0).unwrap())),
            cores: [core(0)].into_iter().collect(),
            inherited_start: offset(0),
            applied: offset(-5),
            edge: Edge::Observed {
                offset: offset(-30),
                witness: seq(4),
            },
            ceiling: Some(offset(-25)),
            remaining: Some(20),
            hold: ClimbHoldProjection {
                credited_s: 180_000,
                required_s: 180_000,
                witnesses: vec![seq(7)],
            },
            next: None,
            state: ClimbUnitState::Climbing,
            latest_step: None,
            diagnostic_s: Some(DiagnosticStatisticalProjection {
                cap: CapProjection {
                    offset: offset(0),
                    binding: togi_journal::Binding::Baseline {},
                    slope: togi_journal::Slope::Fallback {
                        beta: togi_journal::Finite::new(1.0).unwrap(),
                    },
                    adequate: vec![],
                    uncovered: vec![],
                    witness: seq(3),
                },
            }),
        }],
        campaign: Some(ClimbCampaignProjection {
            campaign: CampaignId(Provenance {
                session: "20261002T011407Z".parse().unwrap(),
                seq: seq(1),
            }),
            history: CampaignHistory::Complete {},
            limit: 2,
            consumed: 0,
            remaining: 2,
            provisional: 0,
            reclassified: 0,
            exhausted: false,
            events: vec![],
        }),
        daily: Some(ClimbDailyEvidence {
            profile,
            firmware: BiosContext {
                bios_version: "test".into(),
                board: "test".into(),
                cpu_model: "test".into(),
                microcode: "test".into(),
                boost_limit_mhz: 5000,
            },
            epoch: 1,
            observed_s: 180_000,
            clean_s: 180_000,
            crashes: 0,
            poisson_upper_per_h: Some(2.9957 / 50.0),
            goal: ClimbGoalSupport::InsufficientEvidence,
            witnesses: vec![seq(7)],
        }),
        ..ClimbProjection::default()
    }
}

#[derive(Clone, Copy)]
enum Case {
    Provisional,
    Exhausted,
    ZeroS,
    Blocked,
    Censored,
    Finished,
}

fn shown(policy: &ClimbProjection, color: bool) -> String {
    let mut text = String::new();
    super::climb::sections(
        &mut text,
        policy,
        Where {
            zone: &TimeZone::UTC,
            closed: None,
        },
    );
    // Status intentionally uses plain text in both terminal-color modes.
    Renderer::new(OutputFacts {
        is_terminal: color,
        no_color: !color,
        journald: false,
    })
    .text(Style::Plain, &text)
    .to_string()
}

fn case_fixture(case: Case) -> ClimbProjection {
    let mut policy = fixture();
    match case {
        Case::ZeroS => {
            policy.profiles.proposed = Some(vector(&[-10]));
            policy.units[0].next = Some(ClimbProposalProjection {
                profile: vector(&[-10]),
                qualification: ClimbQualificationProjection {
                    state: ClimbQualificationState::Complete {
                        certificate: seq(8),
                    },
                    outstanding: vec![],
                    witnesses: vec![seq(8)],
                },
            });
        }
        Case::Provisional | Case::Exhausted => {
            let campaign = policy.campaign.as_mut().unwrap();
            campaign.consumed = if matches!(case, Case::Exhausted) {
                2
            } else {
                1
            };
            campaign.remaining = 2 - campaign.consumed;
            campaign.provisional = 1;
            campaign.exhausted = matches!(case, Case::Exhausted);
            campaign.events.push(ClimbEventProjection {
                event: DayEventId::Boot {
                    boot: "day-a".to_owned().into(),
                },
                aliases: vec![],
                phase: CampaignEventPhase::Learning,
                unit_phase: Phase::Core,
                profile: vector(&[-5]),
                provisional: true,
                classification: Classification::CpuSuspect {
                    certainty: Certainty::Provisional,
                },
                reclassified: false,
                witnesses: vec![seq(9)],
            });
            if campaign.exhausted {
                campaign.events.push(ClimbEventProjection {
                    event: DayEventId::Boot {
                        boot: "day-b".to_owned().into(),
                    },
                    aliases: vec![],
                    phase: CampaignEventPhase::PostFinish,
                    unit_phase: Phase::Core,
                    profile: vector(&[-5]),
                    provisional: false,
                    classification: Classification::CpuSuspect {
                        certainty: Certainty::Definite,
                    },
                    reclassified: false,
                    witnesses: vec![seq(10)],
                });
                policy.units[0].state = ClimbUnitState::Finished {
                    reason: UnitStopReason::AllowanceExhausted {},
                    at: "2026-01-01T01:00:00Z".parse().unwrap(),
                    profile: vector(&[-5]),
                    last_clean: offset(-5),
                    witnesses: vec![seq(9), seq(10)],
                };
            }
        }
        Case::Blocked => {
            policy.units[0].state = ClimbUnitState::Held {
                reason: vec![ClimbBlocker::CertificateFailed {
                    profile: vector(&[-10]),
                    witnesses: vec![seq(8)],
                }],
            };
            policy.units[0].next = Some(ClimbProposalProjection {
                profile: vector(&[-10]),
                qualification: ClimbQualificationProjection {
                    state: ClimbQualificationState::Failed {
                        witnesses: vec![seq(8)],
                    },
                    outstanding: vec![],
                    witnesses: vec![seq(8)],
                },
            });
        }
        Case::Censored => {
            policy.units[0].edge = Edge::Censored {
                limit: offset(-50),
                witnesses: vec![seq(4)],
            };
            policy.units[0].ceiling = Some(offset(-45));
            policy.units[0].remaining = Some(40);
        }
        Case::Finished => {
            policy.units[0].state = ClimbUnitState::Finished {
                reason: UnitStopReason::Ceiling {},
                at: "2026-01-01T01:00:00Z".parse().unwrap(),
                profile: vector(&[-25]),
                last_clean: offset(-25),
                witnesses: vec![seq(4)],
            };
            policy.units[0].applied = offset(-25);
            policy.units[0].remaining = Some(0);
            policy.profiles.applied = Some(vector(&[-25]));
            policy.profiles.read_back =
                Some([(core(0), ObservedOffset(-25))].into_iter().collect());
            policy.daily.as_mut().unwrap().profile = vector(&[-25]);
        }
    }
    policy
}

#[test_case(Case::Provisional, include_str!("climb-provisional.txt"), false; "provisional_plain")]
#[test_case(Case::Provisional, include_str!("climb-provisional.txt"), true; "provisional_color")]
#[test_case(Case::Exhausted, include_str!("climb-exhausted.txt"), false; "exhausted_plain")]
#[test_case(Case::Exhausted, include_str!("climb-exhausted.txt"), true; "exhausted_color")]
#[test_case(Case::ZeroS, include_str!("climb-zero-s.txt"), false; "zero_s_permission_plain")]
#[test_case(Case::ZeroS, include_str!("climb-zero-s.txt"), true; "zero_s_permission_color")]
#[test_case(Case::Blocked, include_str!("climb-blocked.txt"), false; "blocked_qualification_plain")]
#[test_case(Case::Blocked, include_str!("climb-blocked.txt"), true; "blocked_qualification_color")]
#[test_case(Case::Censored, include_str!("climb-censored.txt"), false; "censored_plain")]
#[test_case(Case::Censored, include_str!("climb-censored.txt"), true; "censored_color")]
#[test_case(Case::Finished, include_str!("climb-finished.txt"), false; "finished_experimental_plain")]
#[test_case(Case::Finished, include_str!("climb-finished.txt"), true; "finished_experimental_color")]
fn d_policy_status_snapshots(case: Case, expected: &str, color: bool) {
    let actual = shown(&case_fixture(case), color);
    assert_eq!(actual, expected);
    assert!(!actual.contains("meets your goal"));
}

#[test]
fn sixteen_nonuniform_core_units_are_presented_without_ccd_averaging() {
    let mut policy = fixture();
    let template = policy.units[0].clone();
    policy.units = (0..16)
        .map(|id| ClimbUnitProjection {
            unit: UnitId::Core(core(id)),
            cores: [core(id)].into_iter().collect(),
            applied: offset(-id),
            ..template.clone()
        })
        .collect();
    let values: Vec<i64> = (0..16).map(|id| -id).collect();
    policy.profiles.applied = Some(vector(&values));
    let out = shown(&policy, false);
    assert_eq!(policy.units.len(), 16);
    for id in 0..16 {
        assert!(out.contains(&format!("core {id}")), "{out}");
    }
    assert!(out.contains("core 14 -14, core 15 -15"), "{out}");
}

#[test]
fn unavailable_campaign_history_is_not_an_unused_allowance() {
    let out = shown(&ClimbProjection::default(), false);
    assert!(out.contains("allowance: unavailable; campaign history must be reconstructed"));
    assert!(!out.contains("2 remaining"));
}

#[test]
fn only_current_exact_profile_evidence_can_report_the_goal() {
    let mut policy = fixture();
    let daily = policy.daily.as_mut().unwrap();
    daily.observed_s = 32_400_000;
    daily.clean_s = daily.observed_s;
    daily.poisson_upper_per_h = Some(2.9957 / 9000.0);
    daily.goal = ClimbGoalSupport::MeetsGoal;
    assert!(
        shown(&policy, false)
            .contains("meets your goal (current exact profile only; 95%, fixed-rate model)")
    );
}

#[test]
fn old_or_pooled_profile_evidence_never_borrows_the_current_claim() {
    let mut policy = fixture();
    let daily = policy.daily.as_mut().unwrap();
    daily.profile = vector(&[0]);
    daily.observed_s = 32_400_000;
    daily.poisson_upper_per_h = Some(2.9957 / 9000.0);
    daily.goal = ClimbGoalSupport::MeetsGoal;
    let out = shown(&policy, false);
    assert!(out.contains("daily evidence belongs to another profile"));
    assert!(!out.contains("meets your goal"));
    assert!(!out.contains("at most 1 crash per"));
}

#[test]
fn a_fallback_retreat_names_its_wanted_profile_and_adverse_ceiling() {
    let mut policy = fixture();
    policy.units[0].latest_step = Some(togi_tuner::ClimbStepProjection {
        application: togi_journal::ApplicationId(2),
        at: "2026-01-01T01:00:00Z".parse().unwrap(),
        action: togi_journal::ClimbAction::Retreat {
            units: vec![togi_journal::UnitId::Core(core(0))],
            cause: togi_journal::ClimbRetreatCause::AdverseCeiling {
                edges: vec![seq(4)],
            },
            wanted: Some(vector(&[-5])),
        },
        before: vector(&[-10]),
        after: vector(&[0]),
        witnesses: vec![seq(4)],
    });
    let out = shown(&policy, false);
    let words = out.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(words.contains(
        "retreat: adverse ceiling [#4]; wanted core 0 -5 is not qualified, so falls back to stock"
    ));
    assert!(words.contains("core 0 -10 -> core 0 0 [#4]"));
}

#[test]
fn noncontiguous_physical_cores_remain_visible_without_a_fixed_unit_limit() {
    let mut policy = fixture();
    policy.units[0].unit = UnitId::Ccd(CcdId::new(0).unwrap());
    policy.units[0].cores = [0, 2, 4, 6, 8, 10, 12, 14].map(core).into_iter().collect();
    let out = shown(&policy, false);
    assert!(out.contains("0,2,4,6,8,10,12,14"), "{out}");
    assert!(out.lines().all(|line| line.chars().count() <= 80), "{out}");
}

#[test_case(0.1, "10.0"; "nonzero_failure_bound")]
#[test_case(2.9957 / 50.0, "16.7"; "zero_failure_bound")]
fn the_report_uses_the_projected_exact_poisson_bound(rate: f64, reciprocal: &str) {
    let mut policy = fixture();
    policy.daily.as_mut().unwrap().poisson_upper_per_h = Some(rate);
    let out = shown(&policy, false);
    assert!(
        out.contains(&format!(
            "at most 1 crash per {reciprocal} h (95%, fixed-rate model)"
        )),
        "{out}"
    );
}

#[test]
fn missing_time_has_no_zero_rate_or_goal_claim() {
    let mut policy = fixture();
    let daily = policy.daily.as_mut().unwrap();
    daily.observed_s = 0;
    daily.clean_s = 0;
    daily.poisson_upper_per_h = None;
    let out = shown(&policy, false);
    assert!(out.contains("rate bound unavailable (95%, fixed-rate model)"));
    assert!(!out.contains("meets your goal"));
}

#[test]
fn full_profile_projection_data_round_trips_without_losing_any_unit() {
    let mut policy = fixture();
    let template = policy.units[0].clone();
    policy.units = (0..16)
        .map(|id| ClimbUnitProjection {
            unit: UnitId::Core(core(id)),
            cores: [core(id)].into_iter().collect(),
            ..template.clone()
        })
        .collect();
    policy.profiles.paused_saved = Some(vector(&[
        0, -1, -2, -3, -4, -5, -6, -7, -8, -9, -10, -11, -12, -13, -14, -15,
    ]));
    let json = serde_json::to_value(&policy).unwrap();
    let restored: ClimbProjection = serde_json::from_value(json).unwrap();
    assert_eq!(restored, policy);
}

#[test_case(togi_journal::FirmwareRestorationState::Saved {}, "saved; awaiting new-context qualification and stock hold"; "saved")]
#[test_case(togi_journal::FirmwareRestorationState::Qualified { certificate: seq(8) }, "qualified in new context [#8]"; "qualified")]
#[test_case(togi_journal::FirmwareRestorationState::Held { hold: togi_journal::ClimbHold { application: togi_journal::ApplicationId(2), credited: vec![seq(7)], eligible_s: 90_000 } }, "held for stock hours: 25 h [#7]"; "stock_hold")]
#[test_case(togi_journal::FirmwareRestorationState::Restored { application: togi_journal::ApplicationId(2) }, "restored in one whole-profile learning step; application 2"; "restored")]
#[test_case(togi_journal::FirmwareRestorationState::SteppedBack { event: DayEventId::Boot { boot: "day-a".to_owned().into() } }, "stepped back to stock; restored units stopped after boot day-a"; "stepped_back")]
fn firmware_restoration_names_its_state_and_exact_saved_vector(
    state: togi_journal::FirmwareRestorationState,
    expected: &str,
) {
    let mut policy = fixture();
    policy.firmware_restoration = Some(togi_journal::FirmwareRestoration {
        saved: vector(&[-5]),
        state,
    });
    let out = shown(&policy, false);
    let flat = out.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(flat.contains(expected), "{out}");
    assert!(flat.contains("saved exact profile: core 0 -5"), "{out}");
}

#[test]
fn a_night_ceiling_blocker_is_held_not_finished() {
    let mut policy = fixture();
    policy.units[0].state = ClimbUnitState::Held {
        reason: vec![ClimbBlocker::Ceiling {
            unit: policy.units[0].unit,
        }],
    };
    let out = shown(&policy, false);
    assert!(out.contains("held: core 0 is at its ceiling"), "{out}");
}

#[test]
fn a_predecessor_stop_preserves_its_original_event_identity() {
    let mut policy = case_fixture(Case::Finished);
    let ClimbUnitState::Finished { reason, .. } = &mut policy.units[0].state else {
        panic!("finished fixture");
    };
    *reason = UnitStopReason::Predecessor {
        original: serde_json::from_str(r#"{"session":"20261002T011407Z","seq":4}"#).unwrap(),
    };
    let out = shown(&policy, false);
    assert!(
        out.contains("stopped: predecessor event 20261002T011407Z:4"),
        "{out}"
    );
}
