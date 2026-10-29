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
    use togi_tuner::climb::{
        self, ApplicationId, ApplyPrerequisites, Blocker, Campaign, Ceiling, Ceilings,
        ClimbAction, ClimbDecision, ClimbHistory, ClimbInputs, EdgeState, EventPhase,
        EvidenceId, HoldInput,
    };
    use togi_tuner::day::{
        Answer, ApplicationKind, Classification, CoreProfile, CpuCause, DailyEvent,
        DayApplication, DayHistory, DayInterval, EventId, Firmware, Freeze, HistoryError,
        IntervalKind, Invocation, MceKind, Observation, Pause, ProvisionalCause,
        QuestionState, Readback, ReproductionOutcome, RetreatState, Witness, classify,
        credited_secs, daily_events, event_statuses, vetoes,
    };
    use togi_tuner::unit::{UnitId, UnitProfile, Units};
    use togi_tuner::Topology;

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

    /// Raw day evidence supplies hold credit and classified events to the canonical climb.
    struct Day {
        history: DayHistory,
        climb: ClimbHistory,
        campaign: Campaign,
        observations: Vec<Observation>,
        answers: Vec<Answer>,
        reproductions: BTreeMap<EventId, ReproductionOutcome>,
        intervals: Vec<DayInterval>,
        qualified: BTreeSet<UnitProfile>,
        failed: BTreeSet<UnitProfile>,
        ceilings: Ceilings,
        clock: u64,
    }

    impl Day {
        fn new() -> Self {
            let units = Units::coarse(&Topology::new(&topology()).unwrap());
            Self {
                history: DayHistory::baseline(seq(1), profile([0, 0])),
                ceilings: units.iter().map(|unit| (unit.id, Ceiling::new(EdgeState::Observed {
                    offset: offset(-30), witness: EvidenceId::new(1).unwrap(),
                }))).collect(),
                climb: ClimbHistory::stock(units),
                campaign: Campaign::approved_fresh(),
                observations: Vec::new(),
                answers: Vec::new(),
                reproductions: BTreeMap::new(),
                intervals: Vec::new(),
                qualified: BTreeSet::new(),
                failed: BTreeSet::new(),
                clock: 1,
            }
        }

        fn tick(&mut self) -> Seq {
            self.clock += 1;
            seq(self.clock)
        }

        fn push(&mut self, values: [i64; 2], kind: ApplicationKind) {
            let at = self.tick();
            self.history.push(DayApplication { at, profile: profile(values), kind }).unwrap();
        }

        fn run(&mut self, secs: u64) {
            self.interval(IntervalKind::Applied {
                profile: self.history.current().profile.clone(), readback: Readback::Verified,
            }, secs);
        }

        fn interval(&mut self, kind: IntervalKind, secs: u64) {
            let at = self.tick();
            self.intervals.push(DayInterval { at, secs, kind });
        }

        fn credit(&self) -> u64 {
            credited_secs(&self.intervals, &self.history.current().profile, self.history.hold_origin())
        }

        fn observe(&mut self, id: EventId, profile: CoreProfile, witness: Witness) {
            self.observations.push(Observation { id, profile, witness });
        }

        fn crash(&mut self, name: &str, values: [i64; 2], class: EndClass) {
            self.observe(EventId::Boot(boot(name)), core_profile(values), end(class));
        }

        fn answer(&mut self, name: &str, kind: DayAnswerKind) {
            self.answers.push(Answer { boot: boot(name), kind });
        }

        fn events(&self) -> Vec<DailyEvent> {
            daily_events(&self.observations, &self.answers)
        }

        fn close(&mut self, id: EventId) {
            self.reproductions.insert(id, ReproductionOutcome::Unreproduced);
        }

        fn decide(&self) -> ClimbDecision {
            let events = self.events();
            let vetoes = vetoes(&events);
            climb::decide(&ClimbInputs {
                history: &self.climb,
                campaign: &self.campaign,
                ceilings: &self.ceilings,
                events: &events,
                vetoes: &vetoes,
                qualified: &self.qualified,
                failed: &self.failed,
                hold: HoldInput { profile: self.climb.current(), credited_secs: self.credit() },
                firmware: Firmware::Current,
                freeze: Freeze::Off,
                invocation: Invocation::ColdBoot { pause: Pause::Running },
                pending_question: events.iter().any(|event| event.question == QuestionState::Pending),
                unresolved_event: event_statuses(&topology(), &self.history, &events, &self.reproductions)
                    .unwrap().iter().any(|event| event.class.is_cpu_suspect() && !event.is_resolved()),
                apply: ApplyPrerequisites { firmware_valid: true, activation_open: true, safe_apply: true },
            }).unwrap()
        }

        fn apply(&mut self, decision: &ClimbDecision) {
            for event in self.events() {
                self.campaign.observe(&event, &[], EventPhase::Learning, self.climb.units().phase());
            }
            let at = self.tick();
            self.climb.apply(ApplicationId::new(at.get()).unwrap(), instant(i64::try_from(at.get()).unwrap()), decision).unwrap();
            let kind = match &decision.action {
                ClimbAction::Promote(_) | ClimbAction::Restore(_) => ApplicationKind::Promotion,
                ClimbAction::Retreat(retreat) if retreat.profile != retreat.wanted => ApplicationKind::Fallback {
                    events: retreat.events.clone(), reverted: Vec::new(), wanted: retreat.wanted.clone(),
                },
                ClimbAction::Retreat(retreat) => ApplicationKind::Retreat {
                    events: retreat.events.clone(), reverted: Vec::new(),
                },
                ClimbAction::FirmwareZero { .. } => ApplicationKind::FirmwareZero,
                ClimbAction::Hold { .. } => panic!("cannot apply a hold"),
            };
            self.history.push(DayApplication { at, profile: self.climb.current().clone(), kind }).unwrap();
        }

        fn advance(&mut self) {
            self.run(HOURS_50);
            let candidate = climb::propose(self.climb.current(), &self.ceilings, self.climb.finishes()).unwrap();
            self.qualified.insert(candidate.profile);
            let decision = self.decide();
            assert!(matches!(decision.action, ClimbAction::Promote(_)), "{decision:?}");
            self.apply(&decision);
        }
    }

    fn blockers(decision: ClimbDecision) -> Vec<Blocker> {
        let ClimbAction::Hold { blocked_by } = decision.action else {
            panic!("expected hold, got {decision:?}");
        };
        blocked_by
    }

    fn retreat(decision: ClimbDecision) -> climb::Retreat {
        let ClimbAction::Retreat(retreat) = decision.action else {
            panic!("expected retreat, got {decision:?}");
        };
        retreat
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
    fn the_last_answer_wins_and_an_answer_needs_an_unclean_end() {
        let mut day = Day::new();
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
    fn a_manual_record_deduplicates_against_its_boots_unclean_end() {
        let mut day = Day::new();
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
    fn an_unknown_end_at_co_zero_forms_no_event() {
        let mut day = Day::new();
        day.crash("a", [0, 0], EndClass::Unknown);
        day.answer("a", DayAnswerKind::Unsure);
        assert_eq!(day.events()[0].class, Classification::Platform);
        assert!(vetoes(&day.events()).is_empty());
    }

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

    #[test_case(HOURS_50 - 1, false; "one second short")]
    #[test_case(HOURS_50, true; "exactly fifty hours")]
    #[test_case(HOURS_50 + 1, true; "a second over")]
    fn fifty_applied_hours_at_the_exact_d_complete_the_hold(secs: u64, complete: bool) {
        let mut day = Day::new();
        day.advance();
        day.qualified.insert(profile([-5, -5]));
        day.run(secs);
        assert_eq!(day.credit(), secs);
        if complete {
            assert_eq!(day.decide().target(), Some(&profile([-5, -5])));
            assert!(matches!(day.decide().action, ClimbAction::Promote(_)));
        } else {
            assert_eq!(blockers(day.decide()), [Blocker::Hold {
                credited_secs: secs, required_secs: HOURS_50,
            }]);
        }
    }

    #[test_case(Excluded::Paused; "paused time")]
    #[test_case(Excluded::Suspended; "suspended time")]
    #[test_case(Excluded::Unobserved; "unobserved time")]
    #[test_case(Excluded::Ambiguous; "an interval that ended in a changed offset")]
    #[test_case(Excluded::OtherProfile; "another profile")]
    #[test_case(Excluded::UnverifiedReadback; "an unverified readback")]
    fn excluded_intervals_never_count_toward_the_hold(excluded: Excluded) {
        let mut day = Day::new();
        day.push([-10, 0], ApplicationKind::Promotion);
        day.run(HOURS_50 - 3600);
        day.interval(excluded.kind(), 100 * 3600);
        assert_eq!(day.credit(), HOURS_50 - 3600);
        day.run(3600);
        assert_eq!(day.credit(), HOURS_50);
    }

    #[test]
    fn a_profile_change_starts_a_new_hold_and_a_revisit_reuses_no_hours() {
        let mut day = Day::new();
        day.push([-10, 0], ApplicationKind::Promotion);
        day.run(HOURS_50);
        assert_eq!(day.credit(), HOURS_50);
        day.push([-10, -10], ApplicationKind::Promotion);
        assert_eq!(day.credit(), 0, "the earlier profile's hours belong to it");
        day.push([-10, 0], ApplicationKind::SafetyCap);
        assert_eq!(day.credit(), 0, "revisiting a historical profile reuses no hours");
    }

    #[test]
    fn reapplying_the_committed_d_preserves_its_hold_but_any_other_application_restarts_it() {
        let mut day = Day::new();
        day.push([-10, 0], ApplicationKind::Promotion);
        day.run(HOURS_50);
        day.push([-10, 0], ApplicationKind::Reapplication);
        assert_eq!(day.credit(), HOURS_50);
        day.push([-10, 0], ApplicationKind::SafetyCap);
        assert_eq!(day.credit(), 0);
    }

    #[test]
    fn a_failed_qualification_blocks_only_its_exact_profile() {
        let mut day = Day::new();
        day.run(HOURS_50);
        day.qualified.insert(profile([-5, 0]));
        day.failed.insert(profile([-6, 0]));
        assert!(matches!(day.decide().action, ClimbAction::Promote(_)));
    }

    #[test]
    fn a_vetoed_proposal_holds_and_reports_its_veto_without_trying_another_step() {
        let mut day = Day::new();
        day.run(HOURS_50);
        let id = EventId::Unobserved(instant(1));
        day.observe(id.clone(), core_profile([-5, 0]), Witness::Manual);
        day.close(id.clone());
        day.qualified.extend([profile([-5, 0]), profile([0, -5])]);
        assert_eq!(blockers(day.decide()), [Blocker::Veto]);
        assert_eq!(vetoes(&day.events())[0].event, id);
    }

    #[test]
    fn a_proposal_shallower_than_a_veto_in_some_ccd_is_allowed() {
        let mut day = Day::new();
        day.run(HOURS_50);
        day.observe(EventId::Unobserved(instant(1)), core_profile([-5, -5]), Witness::Manual);
        day.close(EventId::Unobserved(instant(1)));
        day.qualified.insert(profile([-5, 0]));
        assert!(matches!(day.decide().action, ClimbAction::Promote(_)));
    }

    #[test]
    fn a_pending_question_blocks_even_at_co_zero_and_an_unrelated_answer_does_not_clear_it() {
        let mut day = Day::new();
        day.run(HOURS_50);
        day.qualified.insert(profile([-5, 0]));
        day.crash("a", [0, 0], EndClass::Unknown);
        day.crash("b", [0, 0], EndClass::Unknown);
        day.answer("b", DayAnswerKind::Graphics);
        assert_eq!(blockers(day.decide()), [Blocker::PendingQuestion]);
        assert_eq!(day.events()[0].id, EventId::Boot(boot("a")));
        assert_eq!(day.events()[0].question, QuestionState::Pending);
        day.answer("a", DayAnswerKind::PowerOff);
        assert!(matches!(day.decide().action, ClimbAction::Promote(_)));
    }

    #[test]
    fn an_unexplained_end_without_a_provisional_rule_is_a_pending_question_not_an_event() {
        let mut day = Day::new();
        day.advance();
        day.run(HOURS_50);
        day.qualified.insert(profile([-5, -5]));
        day.crash("a", [-5, 0], EndClass::KernelFault);
        assert_eq!(day.events()[0].class, Classification::PendingQuestion);
        assert_eq!(day.events()[0].question, QuestionState::Pending);
        assert_eq!(blockers(day.decide()), [Blocker::PendingQuestion]);
    }

    #[test]
    fn a_clean_boot_with_a_core_local_machine_check_is_cpu_suspect() {
        let mut day = Day::new();
        day.advance();
        day.observe(EventId::Boot(boot("clean")), core_profile([-5, 0]), Witness::CleanBootMce(MceKind::CoreLocal));
        let events = day.events();
        assert_eq!(events[0].class, cpu(CpuCause::Mce(MceKind::CoreLocal)));
        assert_eq!(events[0].question, QuestionState::NotAsked);
        assert!(retreat(day.decide()).profile.is_zero());
    }

    #[test]
    fn an_answer_never_erases_independent_machine_check_evidence_in_the_decision() {
        let mut day = Day::new();
        day.advance();
        day.observe(EventId::Boot(boot("a")), core_profile([-5, 0]), Witness::UncleanEnd {
            class: EndClass::Unknown, mce: Some(MceKind::CoreLocal),
        });
        day.answer("a", DayAnswerKind::PowerOff);
        let retreat = retreat(day.decide());
        assert!(retreat.profile.is_zero());
        assert_eq!(retreat.events, [EventId::Boot(boot("a"))]);
        assert_eq!(vetoes(&day.events())[0].certainty, togi_tuner::day::Certainty::Definite);
    }

    #[test]
    fn a_co_zero_event_creates_no_veto_and_charges_no_ccd() {
        let mut day = Day::new();
        day.observe(EventId::Unobserved(instant(7)), core_profile([0, 0]), Witness::Manual);
        assert_eq!(day.events()[0].class, Classification::Platform);
        assert!(vetoes(&day.events()).is_empty());
        day.advance();
        assert_eq!(day.campaign.consumed(), Some(0));
    }

    #[test]
    fn a_failed_requalification_enters_the_same_event_path() {
        let mut day = Day::new();
        day.advance();
        day.observe(EventId::Requalification(seq(9)), core_profile([-5, 0]), Witness::FailedRequalification);
        assert_eq!(day.events()[0].class, cpu(CpuCause::FailedRequalification));
        let retreat = retreat(day.decide());
        assert!(retreat.profile.is_zero());
        assert_eq!(retreat.events, [EventId::Requalification(seq(9))]);
    }

    #[test]
    fn several_observations_of_one_end_are_one_event_and_one_strike() {
        let mut day = Day::new();
        day.advance();
        day.crash("a", [-5, 0], EndClass::CpuFault);
        day.crash("a", [-5, 0], EndClass::CpuFault);
        day.observe(EventId::Boot(boot("a")), core_profile([-5, 0]), Witness::Manual);
        assert_eq!(day.events().len(), 1);
        let decision = day.decide();
        assert!(retreat(decision.clone()).profile.is_zero());
        assert_eq!(retreat(decision.clone()).events, [EventId::Boot(boot("a"))]);
        day.apply(&decision);
        day.observe(EventId::Boot(boot("a")), core_profile([-5, 0]), Witness::Manual);
        assert!(!matches!(day.decide().action, ClimbAction::Retreat(_)));
        assert_eq!(day.campaign.consumed(), Some(1));
    }

    #[test]
    fn a_reclassifying_answer_undoes_the_provisional_veto_and_recomputes_from_raw_intervals() {
        let mut day = Day::new();
        day.advance();
        day.run(HOURS_50);
        day.qualified.insert(profile([-5, -5]));
        day.crash("a", [-5, 0], EndClass::Unknown);
        assert!(matches!(day.decide().action, ClimbAction::Retreat(_)));
        day.answer("a", DayAnswerKind::Graphics);
        assert!(vetoes(&day.events()).is_empty());
        assert_eq!(day.credit(), HOURS_50);
        assert!(matches!(day.decide().action, ClimbAction::Promote(_)));
    }

    #[test]
    fn a_reclassified_provisional_veto_stops_forbidding_its_profile() {
        let mut day = Day::new();
        day.advance();
        day.crash("a", [-5, 0], EndClass::Unknown);
        assert!(vetoes(&day.events())[0].profile.forbids(&core_profile([-5, 0])));
        day.answer("a", DayAnswerKind::PowerOff);
        assert!(vetoes(&day.events()).is_empty());
    }

    #[test]
    fn an_unresolved_event_blocks_until_its_retreat_is_applied_and_its_reproduction_closed() {
        let mut day = Day::new();
        day.advance();
        day.advance();
        day.qualified.insert(profile([-5, 0]));
        day.crash("a", [-5, -5], EndClass::CpuFault);
        day.answer("a", DayAnswerKind::Frozen);
        let decision = day.decide();
        assert_eq!(retreat(decision.clone()).profile, profile([-5, 0]));
        day.apply(&decision);
        day.run(HOURS_50);
        day.qualified.insert(profile([-10, 0]));
        assert_eq!(blockers(day.decide()), [Blocker::UnresolvedEvent]);
        day.close(EventId::Boot(boot("a")));
        assert!(matches!(day.decide().action, ClimbAction::Promote(_)));
    }

    #[test_case(EndClass::Unknown, None, ProvisionalCause::UnansweredUnknown, QuestionState::Parked; "unanswered unknown")]
    #[test_case(EndClass::Watchdog, Some(DayAnswerKind::Unsure), ProvisionalCause::UnsureAnswer, QuestionState::Answered(DayAnswerKind::Unsure); "answered unsure")]
    fn provisional_events_veto_retreat_and_remain_unresolved_until_reproduction_closes(
        class: EndClass,
        answer: Option<DayAnswerKind>,
        cause: ProvisionalCause,
        question: QuestionState,
    ) {
        let mut day = Day::new();
        day.advance();
        day.crash("a", [-5, 0], class);
        if let Some(answer) = answer {
            day.answer("a", answer);
        }
        let events = day.events();
        assert_eq!(events[0].class, provisional(cause));
        assert_eq!(events[0].question, question);
        assert_eq!(vetoes(&events).len(), 1);
        assert_eq!(vetoes(&events)[0].certainty, togi_tuner::day::Certainty::Provisional);
        let decision = day.decide();
        assert!(retreat(decision.clone()).profile.is_zero());
        day.apply(&decision);
        day.run(HOURS_50);
        day.qualified.insert(profile([0, -5]));
        assert_eq!(blockers(day.decide()), [Blocker::UnresolvedEvent]);
        day.close(EventId::Boot(boot("a")));
        let status = event_statuses(&topology(), &day.history, &day.events(), &day.reproductions).unwrap();
        assert!(status[0].is_resolved());
        assert_eq!(status[0].retreat, RetreatState::Applied);
        assert!(vetoes(&day.events())[0].profile.forbids(&core_profile([-5, 0])));
        assert!(day.climb.finishes().is_finished(unit(0)));
        assert_eq!(day.campaign.consumed(), Some(1));
    }

    #[test]
    fn co_zero_holds_an_unqualified_retreat_profile_until_it_qualifies() {
        let mut day = Day::new();
        day.advance();
        day.advance();
        day.qualified.clear();
        day.crash("a", [-5, -5], EndClass::CpuFault);
        let fallback = day.decide();
        assert!(retreat(fallback.clone()).profile.is_zero());
        assert_eq!(retreat(fallback.clone()).wanted, profile([-5, 0]));
        day.apply(&fallback);
        day.close(EventId::Boot(boot("a")));
        let status = event_statuses(&topology(), &day.history, &day.events(), &day.reproductions).unwrap();
        assert_eq!(status[0].retreat, RetreatState::AwaitingQualification { wanted: profile([-5, 0]) });
        assert!(!status[0].is_resolved(), "closing reproduction cannot qualify the retreat target");
        assert!(matches!(day.decide().action, ClimbAction::Hold { .. }));
        day.qualified.insert(profile([-5, 0]));
        let completion = day.decide();
        assert_eq!(retreat(completion.clone()).profile, profile([-5, 0]));
        assert_eq!(retreat(completion.clone()).wanted, profile([-5, 0]));
        day.apply(&completion);
        assert!(!matches!(day.decide().action, ClimbAction::Retreat(_)));
    }

    fn stepped_back_provisionally(fallback: bool) -> Day {
        let mut day = Day::new();
        day.advance();
        day.advance();
        day.advance();
        day.qualified.clear();
        if !fallback {
            day.qualified.insert(profile([-5, -5]));
        }
        day.crash("a", [-10, -5], EndClass::Unknown);
        let decision = day.decide();
        assert_eq!(retreat(decision.clone()).wanted, profile([-5, -5]));
        day.apply(&decision);
        day.close(EventId::Boot(boot("a")));
        if fallback {
            assert!(day.climb.current().is_zero());
            day.qualified.insert(profile([-5, -5]));
            let completion = day.decide();
            assert_eq!(retreat(completion.clone()).profile, profile([-5, -5]));
            day.apply(&completion);
        }
        day.run(HOURS_50);
        day
    }

    #[test]
    fn a_reclassified_step_back_completed_from_co_zero_is_restored() {
        let mut day = stepped_back_provisionally(true);
        day.answer("a", DayAnswerKind::Graphics);
        day.qualified.insert(profile([-10, -5]));
        let decision = day.decide();
        assert!(matches!(decision.action, ClimbAction::Restore(_)));
        assert_eq!(decision.target(), Some(&profile([-10, -5])));
        day.apply(&decision);
        assert!(day.climb.finishes().is_finished(unit(0)));
        assert_eq!(day.campaign.consumed(), Some(1));
    }

    #[test]
    fn a_blocked_restoration_holds_instead_of_taking_another_step() {
        let mut day = stepped_back_provisionally(false);
        day.answer("a", DayAnswerKind::PowerOff);
        day.qualified.insert(profile([-5, -10]));
        assert_eq!(blockers(day.decide()), [Blocker::Certificate]);
    }

    #[test]
    fn a_restoration_beyond_its_ceiling_holds_instead_of_moving_another_ccd() {
        let mut day = stepped_back_provisionally(false);
        day.ceilings.insert(unit(0), Ceiling::new(EdgeState::Observed {
            offset: offset(-10), witness: EvidenceId::new(2).unwrap(),
        }));
        day.answer("a", DayAnswerKind::Graphics);
        let ordinary = climb::propose(day.climb.current(), &day.ceilings, day.climb.finishes()).unwrap();
        assert_eq!(ordinary.unit, unit(1));
        day.qualified.extend([profile([-10, -5]), ordinary.profile]);
        assert_eq!(blockers(day.decide()), [Blocker::Ceiling { unit: unit(0) }]);
    }

    #[test]
    fn a_reclassification_before_its_step_back_applies_restores_nothing() {
        let mut day = Day::new();
        day.advance();
        day.run(HOURS_50);
        day.crash("a", [-5, 0], EndClass::Unknown);
        day.answer("a", DayAnswerKind::Graphics);
        day.qualified.insert(profile([-5, -5]));
        let decision = day.decide();
        assert!(matches!(decision.action, ClimbAction::Promote(_)));
        assert_eq!(decision.target(), Some(&profile([-5, -5])));
    }

    #[test]
    fn the_escalation_never_expires() {
        let mut day = Day::new();
        day.advance();
        day.advance();
        day.advance();
        day.qualified.insert(profile([-5, -5]));
        day.crash("a", [-10, -5], EndClass::CpuFault);
        let first = day.decide();
        day.apply(&first);
        day.run(1000 * 3600);
        day.crash("later", [-5, -5], EndClass::CpuFault);
        let later = retreat(day.decide());
        assert!(later.profile.is_zero());
        assert_eq!(later.cursors.len(), 2);
    }

    #[test]
    fn a_completion_restores_its_fallback_only_once_every_fallback_event_is_reclassified() {
        let mut day = Day::new();
        day.advance();
        day.advance();
        day.advance();
        day.qualified.clear();
        day.crash("a", [-10, -5], EndClass::Unknown);
        day.crash("b", [-10, -5], EndClass::Unknown);
        let fallback = day.decide();
        assert!(retreat(fallback.clone()).profile.is_zero());
        day.apply(&fallback);
        let wanted = retreat(fallback).wanted;
        day.qualified.insert(wanted.clone());
        let completion = day.decide();
        assert_eq!(retreat(completion.clone()).profile, wanted);
        day.apply(&completion);
        day.answer("a", DayAnswerKind::Graphics);
        day.answer("a", DayAnswerKind::Frozen);
        day.answer("b", DayAnswerKind::Graphics);
        assert!(day.events()[0].class.is_cpu_suspect());
        assert_eq!(day.events()[1].class, Classification::NonCpu);
        assert_eq!(climb::restoration_candidate(&day.climb, &day.events()).unwrap(), None);
        day.answer("a", DayAnswerKind::Graphics);
        let restoration = climb::restoration_candidate(&day.climb, &day.events()).unwrap().unwrap();
        assert_eq!(restoration.profile, profile([-10, -5]));
        day.run(HOURS_50);
        day.qualified.insert(restoration.profile);
        assert!(blockers(day.decide()).contains(&Blocker::AllowanceExhausted));
    }

    #[test]
    fn a_retreat_target_a_veto_still_forbids_steps_back_further() {
        let mut day = Day::new();
        day.ceilings.insert(unit(1), Ceiling::new(EdgeState::Observed {
            offset: offset(-5), witness: EvidenceId::new(2).unwrap(),
        }));
        day.advance();
        day.advance();
        assert_eq!(day.climb.current(), &profile([-10, 0]));
        day.qualified.insert(profile([-5, 0]));
        day.observe(EventId::Unobserved(instant(3)), core_profile([-5, 0]), Witness::Manual);
        let retreat = retreat(day.decide());
        assert!(retreat.profile.is_zero());
        assert!(retreat.wanted.is_zero(), "veto requires further backsteps, not qualification fallback");
    }
}
