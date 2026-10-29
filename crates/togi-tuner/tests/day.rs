#![expect(
    clippy::unwrap_used,
    reason = "integration test fixtures and assertions panic on invalid evidence"
)]

mod day {
    use std::collections::{BTreeMap, BTreeSet};

    use jiff::Timestamp;
    use test_case::test_case;
    use togi_journal::{DayAnswerKind, EndClass, Seq};
    use togi_machine::{BootId, CcdId, CoreId, CoreInfo, ObservedOffset, PerCore, TargetOffset};
    use togi_tuner::day::{
        Answer, ApplicationKind, Blocker, Certainty, Classification, CoreProfile, CpuCause,
        DailyEvent, DayApplication, DayDecision, DayHistory, DayInputs, DayInterval, DayParams,
        DayStepError, EventId, Firmware, Freeze, HistoryError, HoldCredit, IntervalKind,
        Invocation, MceKind, Observation, Pause, ProvisionalCause, QuestionState, Readback,
        ReproductionOutcome, RetreatCause, Reverted, Witness, classify, credited_secs,
        daily_events, day_step, next_proposal, propose, vetoes,
    };
    use togi_tuner::unit::{UnitId, UnitProfile};

    const HOURS_50: u64 = 50 * 3600;

    fn ccd(n: u8) -> CcdId {
        CcdId::try_from(n).unwrap()
    }

    fn unit(n: u8) -> UnitId {
        UnitId::Ccd(ccd(n))
    }

    fn offset(value: i64) -> TargetOffset {
        TargetOffset::new(value).unwrap()
    }

    fn profile(values: [i64; 2]) -> UnitProfile {
        UnitProfile::try_from(BTreeMap::from([
            (unit(0), offset(values[0])),
            (unit(1), offset(values[1])),
        ]))
        .unwrap()
    }

    /// Two CCDs of two cores each.
    fn topology() -> Vec<CoreInfo> {
        [(0_i64, 0_u8), (1, 0), (2, 1), (3, 1)]
            .into_iter()
            .map(|(core, ccd_id)| CoreInfo {
                core: CoreId::new(core).unwrap(),
                ccd: ccd(ccd_id),
                cpus: Vec::new(),
            })
            .collect()
    }

    fn core_profile(values: [i64; 2]) -> CoreProfile {
        CoreProfile::of_units(&profile(values), &topology()).unwrap()
    }

    fn seq(value: u64) -> Seq {
        Seq::new(value).unwrap()
    }

    fn boot(name: &str) -> BootId {
        BootId::from(name.to_owned())
    }

    fn instant(second: i64) -> Timestamp {
        Timestamp::from_second(second).unwrap()
    }

    fn end(class: EndClass) -> Witness {
        Witness::UncleanEnd { class, mce: None }
    }

    /// One day: the applied history, the raw evidence and the night side's inputs, with the policy
    /// queried the way the cold-boot decision queries it.
    struct Day {
        history: DayHistory,
        observations: Vec<Observation>,
        answers: Vec<Answer>,
        reproductions: BTreeMap<EventId, ReproductionOutcome>,
        intervals: Vec<DayInterval>,
        caps: UnitProfile,
        qualified: BTreeSet<UnitProfile>,
        blocked: BTreeSet<UnitProfile>,
        firmware: Firmware,
        freeze: Freeze,
        invocation: Invocation,
        clock: u64,
    }

    impl Day {
        fn new(caps: [i64; 2]) -> Self {
            Self {
                history: DayHistory::baseline(seq(1), profile([0, 0])),
                observations: Vec::new(),
                answers: Vec::new(),
                reproductions: BTreeMap::new(),
                intervals: Vec::new(),
                caps: profile(caps),
                qualified: BTreeSet::new(),
                blocked: BTreeSet::new(),
                firmware: Firmware::Current,
                freeze: Freeze::Off,
                invocation: Invocation::ColdBoot {
                    pause: Pause::Running,
                },
                clock: 1,
            }
        }

        fn tick(&mut self) -> Seq {
            self.clock += 1;
            seq(self.clock)
        }

        fn qualify(&mut self, profiles: &[[i64; 2]]) {
            self.qualified.extend(profiles.iter().copied().map(profile));
        }

        fn push(&mut self, values: [i64; 2], kind: ApplicationKind) {
            let at = self.tick();
            self.history
                .push(DayApplication {
                    at,
                    profile: profile(values),
                    kind,
                })
                .unwrap();
        }

        fn promote(&mut self, values: [i64; 2]) {
            self.push(values, ApplicationKind::Promotion);
        }

        /// Applied, verified seconds at the committed D.
        fn run(&mut self, secs: u64) {
            let applied = self.history.current().profile.clone();
            self.interval(
                IntervalKind::Applied {
                    profile: applied,
                    readback: Readback::Verified,
                },
                secs,
            );
        }

        fn interval(&mut self, kind: IntervalKind, secs: u64) {
            let at = self.tick();
            self.intervals.push(DayInterval { at, secs, kind });
        }

        fn observe(&mut self, id: EventId, profile: CoreProfile, witness: Witness) {
            self.observations.push(Observation {
                id,
                profile,
                witness,
            });
        }

        /// An unclean end of boot `name` at `values`.
        fn crash(&mut self, name: &str, values: [i64; 2], class: EndClass) {
            self.observe(EventId::Boot(boot(name)), core_profile(values), end(class));
        }

        fn answer(&mut self, name: &str, kind: DayAnswerKind) {
            self.answers.push(Answer {
                boot: boot(name),
                kind,
            });
        }

        fn close(&mut self, id: &EventId) {
            self.reproductions
                .insert(id.clone(), ReproductionOutcome::Unreproduced);
        }

        fn events(&self) -> Vec<DailyEvent> {
            daily_events(&self.observations, &self.answers)
        }

        fn try_decide(&self) -> Result<DayDecision, DayStepError> {
            let topology = topology();
            let events = self.events();
            day_step(&DayInputs {
                topology: &topology,
                history: &self.history,
                events: &events,
                reproductions: &self.reproductions,
                intervals: &self.intervals,
                caps: &self.caps,
                qualified: &self.qualified,
                blocked: &self.blocked,
                firmware: self.firmware,
                freeze: self.freeze,
                invocation: self.invocation,
                params: DayParams::SPEC,
            })
        }

        fn decide(&self) -> DayDecision {
            self.try_decide().unwrap()
        }

        /// Commits a retreat or promotion decision the way the applying layer's verified readback does.
        fn apply(&mut self, decision: &DayDecision) {
            match decision {
                DayDecision::Retreat {
                    to,
                    causes,
                    unqualified,
                } => {
                    let mut events = Vec::new();
                    let mut reverted = Vec::new();
                    for cause in causes {
                        match cause {
                            RetreatCause::StepBack {
                                events: ids,
                                reverted: steps,
                            }
                            | RetreatCause::Escalation {
                                events: ids,
                                reverted: steps,
                            } => {
                                events.extend(ids.iter().map(|event| event.id.clone()));
                                reverted.extend(steps.iter().copied());
                            }
                            RetreatCause::Completion { events: ids, .. } => {
                                events.extend(ids.iter().map(|event| event.id.clone()));
                            }
                            RetreatCause::SafetyCap { .. } => {}
                        }
                    }
                    let kind = match unqualified {
                        Some(wanted) => ApplicationKind::Fallback {
                            events,
                            reverted,
                            wanted: wanted.clone(),
                        },
                        None if events.is_empty() => ApplicationKind::SafetyCap,
                        None => ApplicationKind::Retreat { events, reverted },
                    };
                    let at = self.tick();
                    self.history
                        .push(DayApplication {
                            at,
                            profile: to.clone(),
                            kind,
                        })
                        .unwrap();
                }
                DayDecision::Promote { proposal, .. } => {
                    let at = self.tick();
                    self.history
                        .push(DayApplication {
                            at,
                            profile: proposal.profile.clone(),
                            kind: ApplicationKind::Promotion,
                        })
                        .unwrap();
                }
                DayDecision::FirmwareZero { to, .. } => {
                    let at = self.tick();
                    self.history
                        .push(DayApplication {
                            at,
                            profile: to.clone(),
                            kind: ApplicationKind::FirmwareZero,
                        })
                        .unwrap();
                }
                DayDecision::Hold { .. } => unreachable!("a hold applies nothing"),
            }
        }
    }

    fn blockers(decision: DayDecision) -> Vec<Blocker> {
        match decision {
            DayDecision::Hold { blockers } => blockers,
            other @ (DayDecision::Promote { .. }
            | DayDecision::Retreat { .. }
            | DayDecision::FirmwareZero { .. }) => panic!("expected a hold, got {other:?}"),
        }
    }

    fn retreat(decision: DayDecision) -> (UnitProfile, Vec<RetreatCause>, Option<UnitProfile>) {
        match decision {
            DayDecision::Retreat {
                to,
                causes,
                unqualified,
            } => (to, causes, unqualified),
            other @ (DayDecision::Hold { .. }
            | DayDecision::Promote { .. }
            | DayDecision::FirmwareZero { .. }) => panic!("expected a retreat, got {other:?}"),
        }
    }

    // --- proposal --------------------------------------------------------------------------------

    #[test_case([0, 0], [-20, -20], Some((0, -10)); "the largest distance halves toward its cap")]
    #[test_case([-10, 0], [-20, -20], Some((1, -10)); "the other CCD has the larger distance")]
    #[test_case([-10, -10], [-20, -20], Some((0, -15)); "a tie goes to the lower CCD")]
    #[test_case([0, 0], [-5, 0], Some((0, -3)); "five counts round up to three")]
    #[test_case([-3, 0], [-5, 0], Some((0, -4)); "two counts move one")]
    #[test_case([-4, 0], [-5, 0], Some((0, -5)); "the last count stops exactly at the cap")]
    #[test_case([-5, 0], [-5, 0], None; "a CCD at its cap has no step")]
    #[test_case([-10, 0], [-5, -20], Some((1, -10)); "a CCD deeper than its cap is not promoted")]
    #[test_case([0, 0], [-1, -1], Some((0, -1)); "one count moves one count")]
    fn the_proposal_moves_the_widest_ccd_by_half_rounded_up(
        applied: [i64; 2],
        caps: [i64; 2],
        expected: Option<(u8, i64)>,
    ) {
        let proposal = propose(&profile(applied), &profile(caps)).unwrap();
        assert_eq!(
            proposal.as_ref().map(|step| (
                match step.unit {
                    UnitId::Ccd(ccd) => ccd.get(),
                    UnitId::Core(_) => panic!("coarse proposal expected"),
                },
                step.to.get()
            )),
            expected
        );
        if let (Some(step), Some((moved, _))) = (proposal, expected) {
            let other = u8::from(moved == 0);
            assert_eq!(
                step.profile.get(unit(other)),
                Some(offset(applied[usize::from(other)])),
                "the other CCD keeps D"
            );
        }
    }

    #[test]
    fn a_cap_over_other_ccds_is_an_error() {
        let caps = UnitProfile::uniform([unit(0)], offset(-10)).unwrap();
        assert_eq!(
            propose(&profile([0, 0]), &caps),
            Err(DayStepError::Unit(unit(1)))
        );
    }

    // --- hold ------------------------------------------------------------------------------------

    #[test]
    fn the_first_nonzero_step_needs_no_hold() {
        let mut day = Day::new([-20, -20]);
        day.qualify(&[[-10, 0]]);
        let DayDecision::Promote { proposal, hold, .. } = day.decide() else {
            panic!("expected a promotion");
        };
        assert_eq!(proposal.profile, profile([-10, 0]));
        assert_eq!(hold, HoldCredit::NotRequired);
    }

    #[test_case(HOURS_50 - 1, false; "one second short")]
    #[test_case(HOURS_50, true; "exactly fifty hours")]
    #[test_case(HOURS_50 + 1, true; "a second over")]
    fn fifty_applied_hours_at_the_exact_d_complete_the_hold(secs: u64, complete: bool) {
        let mut day = Day::new([-20, -20]);
        day.promote([-10, 0]);
        day.qualify(&[[-10, -10]]);
        day.run(secs);
        match day.decide() {
            DayDecision::Promote { hold, proposal, .. } => {
                assert!(complete);
                assert_eq!(proposal.profile, profile([-10, -10]));
                assert_eq!(
                    hold,
                    HoldCredit::Complete {
                        credited_secs: secs
                    }
                );
            }
            DayDecision::Hold { blockers } => {
                assert!(!complete);
                assert_eq!(
                    blockers,
                    [Blocker::HoldIncomplete {
                        credited_secs: secs,
                        required_secs: HOURS_50,
                    }]
                );
            }
            other @ (DayDecision::Retreat { .. } | DayDecision::FirmwareZero { .. }) => {
                panic!("unexpected {other:?}")
            }
        }
    }

    #[derive(Clone, Copy)]
    enum Excluded {
        Paused,
        Suspended,
        Unobserved,
        Ambiguous,
        OtherProfile,
        UnverifiedReadback,
    }

    impl Excluded {
        fn kind(self) -> IntervalKind {
            match self {
                Self::Paused => IntervalKind::Paused,
                Self::Suspended => IntervalKind::Suspended,
                Self::Unobserved => IntervalKind::Unobserved,
                Self::Ambiguous => IntervalKind::Ambiguous,
                Self::OtherProfile => IntervalKind::Applied {
                    profile: profile([-5, 0]),
                    readback: Readback::Verified,
                },
                Self::UnverifiedReadback => IntervalKind::Applied {
                    profile: profile([-10, 0]),
                    readback: Readback::Unverified,
                },
            }
        }
    }

    #[test_case(Excluded::Paused; "paused time")]
    #[test_case(Excluded::Suspended; "suspended time")]
    #[test_case(Excluded::Unobserved; "unobserved time")]
    #[test_case(Excluded::Ambiguous; "an interval that ended in a changed offset")]
    #[test_case(Excluded::OtherProfile; "another profile")]
    #[test_case(Excluded::UnverifiedReadback; "an unverified readback")]
    fn excluded_intervals_never_count_toward_the_hold(excluded: Excluded) {
        let mut day = Day::new([-20, -20]);
        day.promote([-10, 0]);
        day.qualify(&[[-10, -10]]);
        day.run(HOURS_50 - 3600);
        day.interval(excluded.kind(), 100 * 3600);
        assert_eq!(
            blockers(day.decide()),
            [Blocker::HoldIncomplete {
                credited_secs: HOURS_50 - 3600,
                required_secs: HOURS_50,
            }]
        );
        day.run(3600);
        assert!(matches!(day.decide(), DayDecision::Promote { .. }));
    }

    #[test]
    fn a_profile_change_starts_a_new_hold_and_a_revisit_reuses_no_hours() {
        let mut day = Day::new([-20, -20]);
        day.promote([-10, 0]);
        day.run(HOURS_50);
        day.promote([-10, -10]);
        day.qualify(&[[-15, -10]]);
        assert_eq!(
            blockers(day.decide()),
            [Blocker::HoldIncomplete {
                credited_secs: 0,
                required_secs: HOURS_50,
            }],
            "the earlier profile's hours belong to it"
        );
        day.push([-10, 0], ApplicationKind::SafetyCap);
        day.qualify(&[[-10, -10]]);
        assert_eq!(
            blockers(day.decide()),
            [Blocker::HoldIncomplete {
                credited_secs: 0,
                required_secs: HOURS_50,
            }],
            "re-visiting a historical profile does not reuse its old hours"
        );
    }

    #[test]
    fn reapplying_the_committed_d_preserves_its_hold_but_any_other_application_restarts_it() {
        let mut day = Day::new([-20, -20]);
        day.promote([-10, 0]);
        day.qualify(&[[-10, -10]]);
        day.run(HOURS_50);
        day.push([-10, 0], ApplicationKind::Reapplication);
        assert!(matches!(day.decide(), DayDecision::Promote { .. }));
        day.push([-10, 0], ApplicationKind::SafetyCap);
        assert_eq!(
            blockers(day.decide()),
            [Blocker::HoldIncomplete {
                credited_secs: 0,
                required_secs: HOURS_50,
            }]
        );
    }

    #[test]
    fn credited_seconds_count_only_after_the_origin() {
        let intervals = [
            DayInterval {
                at: seq(2),
                secs: 10,
                kind: IntervalKind::Applied {
                    profile: profile([-10, 0]),
                    readback: Readback::Verified,
                },
            },
            DayInterval {
                at: seq(4),
                secs: 20,
                kind: IntervalKind::Applied {
                    profile: profile([-10, 0]),
                    readback: Readback::Verified,
                },
            },
        ];
        assert_eq!(credited_secs(&intervals, &profile([-10, 0]), seq(2)), 20);
        assert_eq!(credited_secs(&intervals, &profile([-10, 0]), seq(1)), 30);
        assert_eq!(credited_secs(&intervals, &profile([-10, 0]), seq(4)), 0);
    }

    // --- eligibility -----------------------------------------------------------------------------

    #[test]
    fn a_vetoed_proposal_holds_and_reports_its_veto_without_trying_another_step() {
        let mut day = Day::new([-20, -20]);
        let id = EventId::Unobserved(instant(1));
        day.observe(id.clone(), core_profile([-10, 0]), Witness::Manual);
        day.close(&id);
        day.qualify(&[[-10, 0], [0, -10]]);
        let blockers = blockers(day.decide());
        assert!(
            matches!(
                blockers.as_slice(),
                [Blocker::Vetoed { proposal, veto }]
                    if proposal.profile == profile([-10, 0]) && veto.event == id
            ),
            "{blockers:?}"
        );
    }

    #[test]
    fn a_proposal_shallower_than_a_veto_in_some_ccd_is_allowed() {
        let mut day = Day::new([-20, -20]);
        let id = EventId::Unobserved(instant(1));
        day.observe(id.clone(), core_profile([-10, -10]), Witness::Manual);
        day.close(&id);
        day.qualify(&[[-10, 0]]);
        assert!(matches!(day.decide(), DayDecision::Promote { .. }));
    }

    #[test]
    fn an_unqualified_proposal_holds() {
        let day = Day::new([-20, -20]);
        let blockers = blockers(day.decide());
        assert!(
            matches!(
                blockers.as_slice(),
                [Blocker::NotQualified { proposal }] if proposal.profile == profile([-10, 0])
            ),
            "{blockers:?}"
        );
    }

    #[test]
    fn a_proposal_that_failed_qualification_holds_even_if_a_qualification_covers_it() {
        let mut day = Day::new([-20, -20]);
        day.qualify(&[[-10, 0]]);
        day.blocked.insert(profile([-10, 0]));
        let blockers = blockers(day.decide());
        assert!(
            matches!(
                blockers.as_slice(),
                [Blocker::QualificationFailed { proposal }] if proposal.profile == profile([-10, 0])
            ),
            "{blockers:?}"
        );
    }

    #[test]
    fn a_failed_qualification_blocks_only_its_exact_profile() {
        let mut day = Day::new([-20, -20]);
        day.qualify(&[[-10, 0]]);
        day.blocked.insert(profile([-12, 0]));
        assert!(matches!(day.decide(), DayDecision::Promote { .. }));
    }

    #[test]
    fn every_ccd_at_its_cap_leaves_nothing_to_promote() {
        let mut day = Day::new([-10, -10]);
        day.promote([-10, 0]);
        day.promote([-10, -10]);
        day.run(HOURS_50);
        assert_eq!(blockers(day.decide()), [Blocker::NothingToPromote]);
    }

    #[test]
    fn a_pending_question_blocks_even_at_co_zero_and_an_unrelated_answer_does_not_clear_it() {
        let mut day = Day::new([-20, -20]);
        day.qualify(&[[-10, 0]]);
        day.crash("a", [0, 0], EndClass::Unknown);
        day.crash("b", [0, 0], EndClass::Unknown);
        day.answer("b", DayAnswerKind::Graphics);
        assert_eq!(
            blockers(day.decide()),
            [Blocker::PendingQuestion {
                events: vec![EventId::Boot(boot("a"))]
            }]
        );
        day.answer("a", DayAnswerKind::PowerOff);
        assert!(matches!(day.decide(), DayDecision::Promote { .. }));
    }

    #[test]
    fn an_unexplained_end_without_a_provisional_rule_is_a_pending_question_not_an_event() {
        let mut day = Day::new([-20, -20]);
        day.promote([-10, 0]);
        day.qualify(&[[-10, -10]]);
        day.run(HOURS_50);
        day.crash("a", [-10, 0], EndClass::KernelFault);
        let events = day.events();
        assert_eq!(events[0].class, Classification::PendingQuestion);
        assert_eq!(events[0].question, QuestionState::Pending);
        assert_eq!(
            blockers(day.decide()),
            [Blocker::PendingQuestion {
                events: vec![EventId::Boot(boot("a"))]
            }]
        );
    }

    #[test]
    fn an_unresolved_event_blocks_until_its_retreat_is_applied_and_its_reproduction_closed() {
        let mut day = Day::new([0, -20]);
        day.promote([-10, 0]);
        day.crash("a", [-10, 0], EndClass::CpuFault);
        day.answer("a", DayAnswerKind::Frozen);
        let id = EventId::Boot(boot("a"));

        let decision = day.decide();
        let (to, causes, _) = retreat(decision.clone());
        assert_eq!(to, profile([0, 0]));
        assert!(matches!(causes.as_slice(), [RetreatCause::StepBack { .. }]));
        day.qualify(&[[0, -10]]);
        day.apply(&decision);
        assert_eq!(
            blockers(day.decide()),
            [Blocker::UnresolvedEvents {
                events: vec![id.clone()]
            }],
            "the retreat is applied; the reproduction is still open"
        );
        day.close(&id);
        assert!(matches!(day.decide(), DayDecision::Promote { .. }));
    }

    #[test]
    fn freeze_blocks_promotion_but_not_a_required_retreat() {
        let mut day = Day::new([-20, -20]);
        day.qualify(&[[-10, 0]]);
        day.freeze = Freeze::On;
        assert_eq!(blockers(day.decide()), [Blocker::Freeze]);

        let mut capped = Day::new([-5, -20]);
        capped.promote([-10, 0]);
        capped.promote([-10, -10]);
        capped.qualify(&[[-5, -10]]);
        capped.freeze = Freeze::On;
        let (to, causes, unqualified) = retreat(capped.decide());
        assert_eq!(to, profile([-5, -10]));
        assert_eq!(unqualified, None);
        assert!(matches!(
            causes.as_slice(),
            [RetreatCause::SafetyCap { caps }] if *caps == profile([-5, -20])
        ));
    }

    #[test]
    fn a_cold_boot_that_started_paused_never_promotes_but_retreats_apply() {
        let mut day = Day::new([-20, -20]);
        day.qualify(&[[-10, 0]]);
        day.invocation = Invocation::ColdBoot {
            pause: Pause::Paused,
        };
        assert_eq!(blockers(day.decide()), [Blocker::PausedColdBoot]);

        let mut crashed = Day::new([-20, -20]);
        crashed.promote([-10, 0]);
        crashed.crash("a", [-10, 0], EndClass::CpuFault);
        crashed.invocation = Invocation::ColdBoot {
            pause: Pause::Paused,
        };
        let (to, causes, _) = retreat(crashed.decide());
        assert_eq!(to, profile([0, 0]));
        assert!(matches!(causes.as_slice(), [RetreatCause::StepBack { .. }]));
    }

    #[test]
    fn a_same_boot_call_never_promotes() {
        let mut day = Day::new([-20, -20]);
        day.qualify(&[[-10, 0]]);
        day.invocation = Invocation::SameBoot;
        assert_eq!(blockers(day.decide()), [Blocker::SameBoot]);
    }

    #[test]
    fn a_firmware_change_zeroes_d_ahead_of_everything_else() {
        let mut day = Day::new([-20, -20]);
        day.promote([-10, 0]);
        day.crash("a", [-10, 0], EndClass::CpuFault);
        day.freeze = Freeze::On;
        day.invocation = Invocation::SameBoot;
        day.firmware = Firmware::Changed;
        assert_eq!(
            day.decide(),
            DayDecision::FirmwareZero {
                to: profile([0, 0]),
                previous: profile([-10, 0]),
            }
        );
    }

    #[test]
    fn after_the_firmware_zero_nothing_nonzero_returns_without_a_new_qualification() {
        let mut day = Day::new([-20, -20]);
        day.promote([-10, 0]);
        day.firmware = Firmware::Changed;
        let decision = day.decide();
        day.apply(&decision);
        let blockers = blockers(day.decide());
        assert!(
            matches!(blockers.as_slice(), [Blocker::NotQualified { .. }]),
            "{blockers:?}"
        );
    }

    #[test]
    fn a_cap_shallower_than_d_retreats_to_the_componentwise_cap_with_no_hold() {
        let mut day = Day::new([-5, -20]);
        day.promote([-10, 0]);
        day.promote([-10, -10]);
        day.qualify(&[[-5, -10]]);
        let (to, causes, unqualified) = retreat(day.decide());
        assert_eq!(to, profile([-5, -10]));
        assert_eq!(unqualified, None);
        assert!(matches!(
            causes.as_slice(),
            [RetreatCause::SafetyCap { .. }]
        ));
    }

    #[test]
    fn an_unqualified_retreat_profile_falls_back_to_co_zero() {
        let mut day = Day::new([-5, -20]);
        day.promote([-10, 0]);
        day.promote([-10, -10]);
        let (to, _, unqualified) = retreat(day.decide());
        assert_eq!(to, profile([0, 0]));
        assert_eq!(unqualified, Some(profile([-5, -10])));
    }

    // --- classification --------------------------------------------------------------------------

    const NO_ANSWER: Option<DayAnswerKind> = None;

    fn cpu(cause: CpuCause) -> Classification {
        Classification::CpuSuspect(cause)
    }

    fn provisional(cause: ProvisionalCause) -> Classification {
        Classification::ProvisionalCpuSuspect(cause)
    }

    #[test]
    fn every_end_class_classifies_per_answer() {
        use Classification::{NonCpu, PendingQuestion};
        let frozen = cpu(CpuCause::FrozenAnswer);
        let unsure = provisional(ProvisionalCause::UnsureAnswer);
        let answers = [
            NO_ANSWER,
            Some(DayAnswerKind::Frozen),
            Some(DayAnswerKind::Graphics),
            Some(DayAnswerKind::PowerOff),
            Some(DayAnswerKind::Unsure),
        ];
        let table = [
            (EndClass::Thermal, [NonCpu, frozen, NonCpu, NonCpu, NonCpu]),
            (EndClass::CpuFault, [cpu(CpuCause::CpuFaultEnd); 5]),
            (
                EndClass::GraphicsSurvived,
                [NonCpu, frozen, NonCpu, NonCpu, NonCpu],
            ),
            (
                EndClass::GraphicsLast,
                [NonCpu, frozen, NonCpu, NonCpu, NonCpu],
            ),
            (
                EndClass::KernelFault,
                [PendingQuestion, frozen, NonCpu, NonCpu, unsure],
            ),
            (
                EndClass::Watchdog,
                [PendingQuestion, frozen, NonCpu, NonCpu, unsure],
            ),
            (
                EndClass::PowerButton,
                [PendingQuestion, frozen, NonCpu, NonCpu, unsure],
            ),
            (
                EndClass::PowerLoss,
                [PendingQuestion, frozen, NonCpu, NonCpu, unsure],
            ),
            (
                EndClass::Unknown,
                [
                    provisional(ProvisionalCause::UnansweredUnknown),
                    frozen,
                    NonCpu,
                    NonCpu,
                    unsure,
                ],
            ),
        ];
        let nonzero = core_profile([-10, 0]);
        for (class, expected) in table {
            for (answer, expected) in answers.into_iter().zip(expected) {
                assert_eq!(
                    classify(&nonzero, end(class), &[], answer),
                    expected,
                    "{class:?} answered {answer:?}"
                );
            }
        }
    }

    #[test]
    fn an_end_at_co_zero_is_platform_evidence_whatever_it_shows() {
        let zero = core_profile([0, 0]);
        for witness in [
            end(EndClass::CpuFault),
            end(EndClass::Unknown),
            Witness::CleanBootMce(MceKind::Uncorrected),
            Witness::Manual,
            Witness::FailedRequalification,
        ] {
            assert_eq!(
                classify(&zero, witness, &[], Some(DayAnswerKind::Frozen)),
                Classification::Platform
            );
        }
    }

    #[test_case(Some(DayAnswerKind::Graphics); "graphics")]
    #[test_case(Some(DayAnswerKind::PowerOff); "power off")]
    #[test_case(Some(DayAnswerKind::Unsure); "unsure")]
    #[test_case(None; "unanswered")]
    fn machine_check_evidence_overrides_every_answer(answer: Option<DayAnswerKind>) {
        let nonzero = core_profile([-10, 0]);
        let with_mce = Witness::UncleanEnd {
            class: EndClass::Unknown,
            mce: Some(MceKind::CoreLocal),
        };
        assert_eq!(
            classify(&nonzero, with_mce, &[], answer),
            cpu(CpuCause::Mce(MceKind::CoreLocal))
        );
        assert_eq!(
            classify(
                &nonzero,
                end(EndClass::GraphicsLast),
                &[Witness::CleanBootMce(MceKind::Uncorrected)],
                answer
            ),
            cpu(CpuCause::Mce(MceKind::Uncorrected)),
            "a later witness's machine check still counts"
        );
    }

    #[test]
    fn a_clean_boot_with_a_core_local_machine_check_is_cpu_suspect() {
        let mut day = Day::new([-20, -20]);
        day.promote([-10, 0]);
        let id = EventId::Boot(boot("clean"));
        day.observe(
            id,
            core_profile([-10, 0]),
            Witness::CleanBootMce(MceKind::CoreLocal),
        );
        let events = day.events();
        assert_eq!(events[0].class, cpu(CpuCause::Mce(MceKind::CoreLocal)));
        assert_eq!(events[0].question, QuestionState::NotAsked);
        let (to, _, _) = retreat(day.decide());
        assert_eq!(to, profile([0, 0]));
    }

    #[test]
    fn the_last_answer_wins_and_an_answer_needs_an_unclean_end() {
        let mut day = Day::new([-20, -20]);
        day.crash("a", [-10, 0], EndClass::Unknown);
        day.answer("a", DayAnswerKind::Frozen);
        day.answer("a", DayAnswerKind::Graphics);
        day.observe(
            EventId::Boot(boot("manual")),
            core_profile([-10, 0]),
            Witness::Manual,
        );
        day.answer("manual", DayAnswerKind::Graphics);
        let events = day.events();
        assert_eq!(events[0].class, Classification::NonCpu);
        assert_eq!(
            events[0].question,
            QuestionState::Answered(DayAnswerKind::Graphics)
        );
        assert_eq!(events[1].class, cpu(CpuCause::ManualRecord));
        assert_eq!(events[1].question, QuestionState::NotAsked);
    }

    #[test]
    fn an_answer_never_erases_independent_machine_check_evidence_in_the_decision() {
        let mut day = Day::new([-20, -20]);
        day.promote([-10, 0]);
        day.observe(
            EventId::Boot(boot("a")),
            core_profile([-10, 0]),
            Witness::UncleanEnd {
                class: EndClass::Unknown,
                mce: Some(MceKind::CoreLocal),
            },
        );
        day.answer("a", DayAnswerKind::PowerOff);
        let (to, causes, _) = retreat(day.decide());
        assert_eq!(to, profile([0, 0]));
        assert!(matches!(
            causes.as_slice(),
            [RetreatCause::StepBack { events, .. }] if events[0].certainty == Certainty::Definite
        ));
    }

    // --- vetoes ----------------------------------------------------------------------------------

    #[test]
    fn a_nonuniform_profile_is_compared_on_every_core() {
        let cores: PerCore<ObservedOffset> = [(0_i64, -10_i64), (1, -5), (2, 0), (3, 0)]
            .into_iter()
            .map(|(core, value)| (CoreId::new(core).unwrap(), ObservedOffset(value)))
            .collect();
        let failed = CoreProfile::new(cores).unwrap();
        assert!(failed.forbids(&core_profile([-10, 0])));
        assert!(failed.forbids(&core_profile([-12, -3])));
        assert!(
            !failed.forbids(&core_profile([-8, 0])),
            "core 0 is shallower than the failed -10"
        );
    }

    #[test]
    fn a_co_zero_event_creates_no_veto_and_charges_no_ccd() {
        let mut day = Day::new([-20, -20]);
        let id = EventId::Unobserved(instant(7));
        day.observe(id.clone(), core_profile([0, 0]), Witness::Manual);
        day.close(&id);
        day.qualify(&[[-10, 0]]);
        assert_eq!(day.events()[0].class, Classification::Platform);
        assert!(vetoes(&day.events()).is_empty());
        assert!(matches!(day.decide(), DayDecision::Promote { .. }));
    }

    #[test]
    fn a_failed_requalification_enters_the_same_event_path() {
        let mut day = Day::new([-20, -20]);
        day.promote([-10, 0]);
        day.observe(
            EventId::Requalification(togi_journal::Seq::new(9).unwrap()),
            core_profile([-10, 0]),
            Witness::FailedRequalification,
        );
        assert_eq!(day.events()[0].class, cpu(CpuCause::FailedRequalification));
        let (to, causes, _) = retreat(day.decide());
        assert_eq!(to, profile([0, 0]));
        assert!(matches!(causes.as_slice(), [RetreatCause::StepBack { .. }]));
    }

    // --- escalation ------------------------------------------------------------------------------

    /// CCD 0 `0 → -8 → -12 → -14`, CCD 1 `0 → -10 → -15`, CCD 0's `-14` newest.
    fn spec_example() -> (Day, [Seq; 5]) {
        let mut day = Day::new([-20, -20]);
        let mut steps = Vec::new();
        for values in [[-8, 0], [-8, -10], [-12, -10], [-12, -15], [-14, -15]] {
            day.promote(values);
            steps.push(seq(day.clock));
        }
        (day, steps.try_into().unwrap())
    }

    #[test]
    fn escalation_reverts_the_newest_step_then_every_ccds_own_newest_remaining_step() {
        let (mut day, steps) = spec_example();
        day.qualify(&[[-12, -15], [-8, -10]]);

        day.crash("a", [-14, -15], EndClass::CpuFault);
        let first = day.decide();
        let (to, causes, _) = retreat(first.clone());
        assert_eq!(to, profile([-12, -15]));
        assert!(
            matches!(
                causes.as_slice(),
                [RetreatCause::StepBack { reverted, .. }]
                    if *reverted == [Reverted { step: steps[4], unit: unit(0) }]
            ),
            "{causes:?}"
        );
        day.apply(&first);

        day.crash("b", [-12, -15], EndClass::CpuFault);
        let second = day.decide();
        let (to, causes, _) = retreat(second.clone());
        assert_eq!(to, profile([-8, -10]));
        assert!(
            matches!(
                causes.as_slice(),
                [RetreatCause::Escalation { reverted, .. }]
                    if *reverted == [
                        Reverted { step: steps[2], unit: unit(0) },
                        Reverted { step: steps[3], unit: unit(1) },
                    ]
            ),
            "{causes:?}"
        );
        day.apply(&second);

        day.crash("c", [-8, -10], EndClass::CpuFault);
        let third = day.decide();
        let (to, causes, _) = retreat(third);
        assert_eq!(to, profile([0, 0]));
        assert!(matches!(
            causes.as_slice(),
            [RetreatCause::Escalation { .. }]
        ));
    }

    #[test]
    fn a_ccd_without_a_remaining_step_stays_at_zero() {
        let mut day = Day::new([-20, -20]);
        day.promote([-10, 0]);
        day.promote([-12, 0]);
        day.qualify(&[[-10, 0]]);
        day.crash("a", [-12, 0], EndClass::CpuFault);
        let first = day.decide();
        assert_eq!(retreat(first.clone()).0, profile([-10, 0]));
        day.apply(&first);
        day.crash("b", [-10, 0], EndClass::CpuFault);
        assert_eq!(retreat(day.decide()).0, profile([0, 0]));
    }

    #[test]
    fn the_escalation_never_expires() {
        let (mut day, _) = spec_example();
        day.qualify(&[[-12, -15], [-8, -10]]);
        day.crash("a", [-14, -15], EndClass::CpuFault);
        let first = day.decide();
        day.apply(&first);
        day.run(1000 * 3600);
        day.crash("later", [-12, -15], EndClass::CpuFault);
        assert!(matches!(
            retreat(day.decide()).1.as_slice(),
            [RetreatCause::Escalation { .. }]
        ));
    }

    #[test]
    fn several_observations_of_one_end_are_one_event_and_one_strike() {
        let (mut day, _) = spec_example();
        day.qualify(&[[-12, -15], [-8, -10]]);
        day.crash("a", [-14, -15], EndClass::CpuFault);
        day.crash("a", [-14, -15], EndClass::CpuFault);
        day.observe(
            EventId::Boot(boot("a")),
            core_profile([-14, -15]),
            Witness::Manual,
        );
        assert_eq!(day.events().len(), 1);
        let first = day.decide();
        let (to, causes, _) = retreat(first.clone());
        assert_eq!(to, profile([-12, -15]));
        assert!(matches!(
            causes.as_slice(),
            [RetreatCause::StepBack { events, .. }] if events.len() == 1
        ));
        day.apply(&first);
        day.observe(
            EventId::Boot(boot("a")),
            core_profile([-14, -15]),
            Witness::Manual,
        );
        assert!(
            !matches!(day.decide(), DayDecision::Retreat { .. }),
            "a late copy of the same end is no further strike"
        );
    }

    #[test]
    fn a_manual_record_deduplicates_against_its_boots_unclean_end() {
        let mut day = Day::new([-20, -20]);
        day.observe(
            EventId::Boot(boot("a")),
            core_profile([-8, 0]),
            Witness::Manual,
        );
        day.crash("a", [-10, 0], EndClass::Unknown);
        let events = day.events();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].profile,
            core_profile([-10, 0]),
            "the observed profile wins"
        );
        assert_eq!(events[0].class, cpu(CpuCause::ManualRecord));
    }

    #[test]
    fn a_retreat_target_a_veto_still_forbids_steps_back_further() {
        let mut day = Day::new([-20, -20]);
        day.promote([-10, 0]);
        day.promote([-12, 0]);
        day.qualify(&[[0, 0]]);
        let id = EventId::Unobserved(instant(3));
        day.observe(id, core_profile([-10, 0]), Witness::Manual);
        let (to, _, _) = retreat(day.decide());
        assert_eq!(to, profile([0, 0]));
    }

    // --- provisional events ----------------------------------------------------------------------

    #[test]
    fn an_unanswered_unknown_end_at_nonzero_d_vetoes_and_retreats_and_resolves_like_any_event() {
        let mut day = Day::new([-20, -20]);
        day.promote([-10, 0]);
        day.crash("a", [-10, 0], EndClass::Unknown);
        let id = EventId::Boot(boot("a"));
        let events = day.events();
        assert_eq!(
            events[0].class,
            provisional(ProvisionalCause::UnansweredUnknown)
        );
        assert_eq!(events[0].question, QuestionState::Parked);
        assert_eq!(vetoes(&events).len(), 1);

        let decision = day.decide();
        let (to, causes, _) = retreat(decision.clone());
        assert_eq!(to, profile([0, 0]));
        assert!(matches!(
            causes.as_slice(),
            [RetreatCause::StepBack { events, .. }] if events[0].certainty == Certainty::Provisional
        ));
        day.apply(&decision);
        day.qualify(&[[-10, 0]]);
        let held = blockers(day.decide());
        assert!(
            held.contains(&Blocker::UnresolvedEvents {
                events: vec![id.clone()]
            }),
            "the event is unresolved: {held:?}"
        );
        assert!(
            !held
                .iter()
                .any(|blocker| matches!(blocker, Blocker::PendingQuestion { .. })),
            "its question no longer counts as pending: {held:?}"
        );
        day.close(&id);
        let held = blockers(day.decide());
        assert!(
            matches!(held.as_slice(), [Blocker::Vetoed { veto, .. }] if veto.certainty == Certainty::Provisional),
            "resolved events no longer block, but their veto and step back persist: {held:?}"
        );
    }

    #[test]
    fn a_reclassifying_answer_undoes_the_provisional_veto_and_recomputes_from_raw_intervals() {
        let mut day = Day::new([-20, -20]);
        day.promote([-10, 0]);
        day.qualify(&[[-10, -10], [-10, 0]]);
        day.run(HOURS_50);
        day.crash("a", [-10, 0], EndClass::Unknown);
        assert!(
            matches!(day.decide(), DayDecision::Retreat { .. }),
            "provisional: step back, whatever the hold"
        );
        day.answer("a", DayAnswerKind::Graphics);
        let DayDecision::Promote { hold, .. } = day.decide() else {
            panic!("the reclassified event leaves the hold intact");
        };
        assert_eq!(
            hold,
            HoldCredit::Complete {
                credited_secs: HOURS_50
            }
        );
    }

    #[test]
    fn a_reclassified_provisional_veto_stops_forbidding_its_profile() {
        let mut day = Day::new([-20, -20]);
        day.promote([-10, 0]);
        day.crash("a", [-10, 0], EndClass::Unknown);
        let decision = day.decide();
        day.apply(&decision);
        let id = EventId::Boot(boot("a"));
        day.close(&id);
        day.qualify(&[[-10, 0]]);
        assert!(matches!(
            blockers(day.decide()).as_slice(),
            [Blocker::Vetoed { .. }]
        ));
        day.answer("a", DayAnswerKind::PowerOff);
        let DayDecision::Promote { proposal, .. } = day.decide() else {
            panic!("the veto is gone with its event's classification");
        };
        assert_eq!(proposal.profile, profile([-10, 0]));
    }

    /// D at [-14, -15] after promotions [-12, -15] and [-14, -15], an unanswered Unknown end there,
    /// and its provisional step back to the qualified [-12, -15] applied and held for 50 hours.
    fn stepped_back_provisionally() -> Day {
        let mut day = Day::new([-20, -20]);
        day.promote([-12, -15]);
        day.promote([-14, -15]);
        day.crash("a", [-14, -15], EndClass::Unknown);
        day.qualify(&[[-12, -15]]);
        let decision = day.decide();
        assert_eq!(retreat(decision.clone()).0, profile([-12, -15]));
        day.apply(&decision);
        day.close(&EventId::Boot(boot("a")));
        day.run(HOURS_50);
        day
    }

    #[test]
    fn a_reclassified_applied_step_back_is_restored_before_the_half_distance_step() {
        let mut day = stepped_back_provisionally();
        day.qualify(&[[-14, -15], [-16, -15]]);
        day.answer("a", DayAnswerKind::Graphics);
        let decision = day.decide();
        let DayDecision::Promote { proposal, hold, .. } = &decision else {
            panic!("expected the restoration, got {decision:?}");
        };
        assert_eq!(proposal.profile, profile([-14, -15]));
        let restores = proposal.restores.as_ref().unwrap();
        assert_eq!(restores.events, [EventId::Boot(boot("a"))]);
        assert_eq!(
            *hold,
            HoldCredit::Complete {
                credited_secs: HOURS_50
            },
            "the restoration passes the ordinary hold at the stepped-back D"
        );
        // Applied, the restoration is consumed: the next step is the ordinary one.
        day.apply(&decision);
        day.run(HOURS_50);
        day.qualify(&[[-17, -15]]);
        let DayDecision::Promote { proposal, .. } = day.decide() else {
            panic!("expected the half-distance step");
        };
        assert_eq!(proposal.profile, profile([-17, -15]));
        assert_eq!(proposal.restores, None);
    }

    #[test]
    fn a_reclassified_step_back_completed_from_co_zero_is_restored() {
        let mut day = Day::new([-20, -20]);
        day.promote([-12, -15]);
        day.promote([-14, -15]);
        day.crash("a", [-14, -15], EndClass::Unknown);
        let fallback = day.decide();
        let (to, _, unqualified) = retreat(fallback.clone());
        assert_eq!(to, profile([0, 0]));
        assert_eq!(unqualified, Some(profile([-12, -15])));
        day.apply(&fallback);
        day.close(&EventId::Boot(boot("a")));
        day.qualify(&[[-12, -15]]);
        let completion = day.decide();
        assert_eq!(retreat(completion.clone()).0, profile([-12, -15]));
        day.apply(&completion);
        day.run(HOURS_50);

        day.qualify(&[[-14, -15], [-16, -15]]);
        day.answer("a", DayAnswerKind::Graphics);
        let decision = day.decide();
        let DayDecision::Promote { proposal, .. } = &decision else {
            panic!("expected the restoration, got {decision:?}");
        };
        assert_eq!(
            proposal.profile,
            profile([-14, -15]),
            "the completed step back owns the step its CO 0 fallback consumed"
        );
        let restores = proposal.restores.as_ref().unwrap();
        assert_eq!(restores.events, [EventId::Boot(boot("a"))]);
    }

    #[test]
    fn a_completion_restores_its_fallback_only_once_every_fallback_event_is_reclassified() {
        let mut day = Day::new([-20, -20]);
        day.promote([-12, -15]);
        day.promote([-14, -15]);
        day.crash("a", [-14, -15], EndClass::Unknown);
        day.crash("b", [-14, -15], EndClass::Unknown);
        let fallback = day.decide();
        assert_eq!(retreat(fallback.clone()).0, profile([0, 0]));
        day.apply(&fallback);
        // Only `b`'s completion reaches [-12, -15]; `a` is answered graphics, then revised to frozen.
        let at = day.tick();
        day.history
            .push(DayApplication {
                at,
                profile: profile([-12, -15]),
                kind: ApplicationKind::Retreat {
                    events: vec![EventId::Boot(boot("b"))],
                    reverted: Vec::new(),
                },
            })
            .unwrap();
        day.answer("a", DayAnswerKind::Graphics);
        day.answer("a", DayAnswerKind::Frozen);
        day.answer("b", DayAnswerKind::Graphics);
        let events = day.events();
        assert!(events[0].class.is_cpu_suspect(), "{:?}", events[0]);
        assert_eq!(events[1].class, Classification::NonCpu);

        day.caps = profile([-13, -30]);
        let proposal = next_proposal(&day.history, &events, &day.caps)
            .unwrap()
            .unwrap();
        assert_eq!(
            proposal.restores, None,
            "`a` still blames the fallback's consumed step"
        );
        assert_eq!(proposal.unit, unit(1), "{proposal:?}");

        // Once `a` is reclassified as well, the completion owns the fallback's step again.
        day.answer("a", DayAnswerKind::Graphics);
        let events = day.events();
        let proposal = next_proposal(&day.history, &events, &day.caps)
            .unwrap()
            .unwrap();
        assert!(proposal.restores.is_some(), "{proposal:?}");
        assert_eq!(proposal.profile, profile([-13, -15]));
    }

    #[test]
    fn a_blocked_restoration_holds_instead_of_taking_the_half_distance_step() {
        let mut day = stepped_back_provisionally();
        day.qualify(&[[-16, -15]]);
        day.answer("a", DayAnswerKind::PowerOff);
        let held = blockers(day.decide());
        assert!(
            matches!(
                held.as_slice(),
                [Blocker::NotQualified { proposal }] if proposal.profile == profile([-14, -15])
            ),
            "{held:?}"
        );
    }

    #[test]
    fn a_restoration_with_no_room_under_its_cap_holds_instead_of_moving_another_ccd() {
        let mut day = stepped_back_provisionally();
        day.caps = profile([-12, -20]);
        let ordinary = propose(&profile([-12, -15]), &day.caps).unwrap().unwrap();
        assert_eq!(ordinary.unit, unit(1), "another CCD has an ordinary step");
        day.qualify(&[[-14, -15], [-12, ordinary.to.get()]]);
        day.answer("a", DayAnswerKind::Graphics);
        assert_eq!(
            blockers(day.decide()),
            [Blocker::NothingToPromote],
            "CCD 0 owes a restoration it has no room for under its cap"
        );
    }

    /// A reclassified step back to [-12, -12] that reverted `reverted`, after `promotions` from
    /// [0, 0]; the next proposal under caps of -20.
    fn restored_after(promotions: &[[i64; 2]], reverted: &[(u64, u8)]) -> UnitProfile {
        let mut day = Day::new([-20, -20]);
        for &values in promotions {
            day.promote(values);
        }
        day.crash("a", *promotions.last().unwrap(), EndClass::Unknown);
        day.push(
            [-12, -12],
            ApplicationKind::Retreat {
                events: vec![EventId::Boot(boot("a"))],
                reverted: reverted
                    .iter()
                    .map(|&(step, id)| Reverted {
                        step: seq(step),
                        unit: unit(id),
                    })
                    .collect(),
            },
        );
        day.answer("a", DayAnswerKind::Graphics);
        let proposal = next_proposal(&day.history, &day.events(), &day.caps)
            .unwrap()
            .unwrap();
        assert!(proposal.restores.is_some(), "{proposal:?}");
        proposal.profile
    }

    #[test]
    fn of_several_owed_restorations_the_newest_reverted_promotion_comes_first() {
        // Promotion 3 moved unit 0 and promotion 4 unit 1; the newer wins over the lower unit.
        let restored = restored_after(&[[-12, -12], [-14, -12], [-14, -14]], &[(3, 0), (4, 1)]);
        assert_eq!(restored, profile([-12, -14]));
    }

    #[test]
    fn restorations_owed_by_the_same_promotion_go_to_the_lower_unit_first() {
        let restored = restored_after(&[[-12, -12], [-14, -14]], &[(3, 1), (3, 0)]);
        assert_eq!(restored, profile([-14, -12]));
    }

    #[test]
    fn a_reclassification_before_its_step_back_applies_restores_nothing() {
        let mut day = Day::new([-20, -20]);
        day.promote([-12, -15]);
        day.promote([-14, -15]);
        day.run(HOURS_50);
        day.crash("a", [-14, -15], EndClass::Unknown);
        day.answer("a", DayAnswerKind::Graphics);
        day.qualify(&[[-17, -15]]);
        let DayDecision::Promote { proposal, .. } = day.decide() else {
            panic!("expected the half-distance step");
        };
        assert_eq!(proposal.profile, profile([-17, -15]));
        assert_eq!(proposal.restores, None);
    }

    #[test]
    fn an_unsure_answer_stays_an_unresolved_event_after_its_question_is_answered() {
        let mut day = Day::new([-20, -20]);
        day.promote([-10, 0]);
        day.crash("a", [-10, 0], EndClass::Watchdog);
        day.answer("a", DayAnswerKind::Unsure);
        let events = day.events();
        assert_eq!(events[0].class, provisional(ProvisionalCause::UnsureAnswer));
        assert_eq!(
            events[0].question,
            QuestionState::Answered(DayAnswerKind::Unsure)
        );
        let decision = day.decide();
        assert!(matches!(decision, DayDecision::Retreat { .. }));
        day.apply(&decision);
        day.qualify(&[[-10, 0]]);
        let held = blockers(day.decide());
        assert!(
            held.contains(&Blocker::UnresolvedEvents {
                events: vec![EventId::Boot(boot("a"))]
            }),
            "{held:?}"
        );
    }

    #[test]
    fn an_unknown_end_at_co_zero_forms_no_event() {
        let mut day = Day::new([-20, -20]);
        day.crash("a", [0, 0], EndClass::Unknown);
        day.answer("a", DayAnswerKind::Unsure);
        assert_eq!(day.events()[0].class, Classification::Platform);
        assert!(vetoes(&day.events()).is_empty());
    }

    // --- retreat completion ----------------------------------------------------------------------

    #[test]
    fn co_zero_holds_an_unqualified_retreat_profile_until_it_qualifies() {
        let mut day = Day::new([-20, -20]);
        day.promote([-8, 0]);
        day.promote([-8, -10]);
        day.crash("a", [-8, -10], EndClass::CpuFault);
        let fallback = day.decide();
        let (to, _, unqualified) = retreat(fallback.clone());
        assert_eq!(to, profile([0, 0]));
        assert_eq!(unqualified, Some(profile([-8, 0])));
        day.apply(&fallback);

        day.close(&EventId::Boot(boot("a")));
        let held = blockers(day.decide());
        assert!(
            held.iter()
                .any(|blocker| matches!(blocker, Blocker::UnresolvedEvents { .. })),
            "{held:?}"
        );

        day.qualify(&[[-8, 0]]);
        let completion = day.decide();
        let (to, causes, unqualified) = retreat(completion.clone());
        assert_eq!(to, profile([-8, 0]));
        assert_eq!(unqualified, None);
        assert!(matches!(
            causes.as_slice(),
            [RetreatCause::Completion { .. }]
        ));
        day.apply(&completion);
        assert!(
            !matches!(day.decide(), DayDecision::Retreat { .. }),
            "applying the retreat profile completes it"
        );
    }

    // --- history ---------------------------------------------------------------------------------

    #[test]
    fn the_history_refuses_an_application_out_of_order_or_over_other_ccds() {
        let mut history = DayHistory::baseline(seq(5), profile([0, 0]));
        assert_eq!(
            history.push(DayApplication {
                at: seq(5),
                profile: profile([-10, 0]),
                kind: ApplicationKind::Promotion,
            }),
            Err(HistoryError::OutOfOrder {
                previous: seq(5),
                next: seq(5)
            })
        );
        assert_eq!(
            history.push(DayApplication {
                at: seq(6),
                profile: UnitProfile::uniform([unit(0)], offset(-10)).unwrap(),
                kind: ApplicationKind::Promotion,
            }),
            Err(HistoryError::Units { at: seq(6) })
        );
    }

    #[test]
    fn promotions_before_a_firmware_reset_are_never_reverted() {
        let mut day = Day::new([-20, -20]);
        day.promote([-10, 0]);
        day.push([0, 0], ApplicationKind::FirmwareZero);
        day.promote([-5, 0]);
        assert_eq!(day.history.promotions().len(), 1);
    }
}
