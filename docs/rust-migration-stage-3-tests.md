# Stage 3 Go-test mapping

Every `func Test*`, `Benchmark*`, `Fuzz*` and `Example*` in the Go packages ported in stage 3 (#638) is mapped to the Rust tests that pin the same behavior: `internal/config` → `togi-config`, `internal/defect` → `togi-defect`, `internal/facts` → `togi-facts`, `internal/carry` → `togi-carry`, `internal/tuner` → `togi-tuner`. There are no Fuzz or Example functions and one Benchmark.

Rust names are `crate::module::fn`, each resolved to a test that `cargo nextest list` reports on `main` at 113b73ee with this pull request's three `togi-journal` tests added (02559d73: `togi_journal::tests::strict::a_line_without_a_kind_is_an_envelope_error`, `togi_journal::tests::strict::open_refuses_a_journal_line_without_a_kind_and_leaves_the_file_unchanged` and `togi_journal::journal::tests::archive::an_archive_target_that_is_a_directory_is_refused_before_anything_is_recorded`); a parameterised `#[test_case]` function counts by its name. The pull requests that ported each package are #770 (config), #775 (defect), #776 (facts), #783 (carry), #777 (tuner G1, G2), #799 (tuner G3 to G5) and #801 (tuner G6: projection, forecast and dashboard).

Status:

- **A**: a Rust test pins the behavior.
- **O(reason)**: obsolete under the native contract (Rust never reads Go journals).
- **J**: owned by togi-journal; the journal tests are named.
- **S4(reason)**: stage 4 simulator scope.

## Summary

| Package | Go tests | A | O | J | S4 |
|---|---:|---:|---:|---:|---:|
| config | 4 | 4 | 0 | 0 | 0 |
| defect | 5 | 3 | 2 | 0 | 0 |
| facts | 15 | 8 | 6 | 1 | 0 |
| carry | 45 | 31 | 3 | 11 | 0 |
| tuner | 272 | 268 | 0 | 0 | 4 |
| **total** | **341** | **314** | **11** | **12** | **4** |

Nine `internal/session` tests that #770 and #775 also map (one config, eight defect) are outside these packages and not counted.

## config

### `internal/config/config_test.go` (4)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestLoad` | `togi_config::tests::empty_object_is_the_defaults`<br>`togi_config::tests::customised_values_survive`<br>`togi_config::tests::core_keys_accepted_in_both_tables`<br>`togi_config::tests::partial_files_keep_other_defaults`<br>`togi_config::tests::empty_override_tables_are_none`<br>`togi_config::tests::overrides_by_core`<br>`togi_config::tests::candidate_solo_limit_beside_another_cores_start_offset`<br>`togi_config::tests::cycle_replaced_with_repeats_in_any_order`<br>`togi_config::tests::backends_and_user`<br>`togi_config::tests::duration_bounds_accepted`<br>`togi_config::tests::duration_bounds_rejected`<br>`togi_config::tests::thresholds_rejected`<br>`togi_config::tests::evidence_rejected`<br>`togi_config::tests::cycles_rejected`<br>`togi_config::tests::unknown_members`<br>`togi_config::tests::duplicate_members`<br>`togi_config::tests::wrong_types`<br>`togi_config::tests::malformed_documents`<br>`togi_config::tests::null_offsets_rejected`<br>`togi_config::tests::duplicate_core_keys_rejected`<br>`togi_config::tests::a_core_with_both_overrides_is_rejected`<br>`togi_config::tests::non_canonical_core_keys_rejected`<br>`togi_config::tests::offsets_out_of_range_rejected`<br>`togi_config::tests::relative_backend_paths_rejected` | A — the tailored "removed confirmation key", "null root" and "array root" messages are gone; `unknown_members` and `malformed_documents` pin them as generic rejections |
| `TestLoadMissingFile` | `togi_config::tests::load_of_a_missing_file_stays_inspectable` | A |
| `TestEvidenceTrials` | `togi_config::tests::evidence_trials` | A |
| `TestDefaultCycle` | `togi_config::tests::default_cycle` | A |

## defect

### `internal/defect/defect_test.go` (5)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestPowerOffDefect` | `togi_defect::tests::fix_watermark_strict`<br>`togi_defect::tests::watermark_below_every_entry_is_affected_and_equal_is_not`<br>`togi_defect::tests::finding_once_per_id`<br>`togi_defect::tests::cause_kind_must_match` | O(the power-off entry matches only Go-written journals and the registry starts empty; its generic cases run on synthetic entries in the tests listed) |
| `TestPowerOffFixtureReplaysBackoff` | — | O(a Go journal fixture and Go backoff replay; Rust reads no Go journal) |
| `TestFailuresWithKeepsDefectEvidenceAfterFinding` | `togi_defect::tests::excluded_failures_persist_after_finding`<br>`togi_defect::tests::excluded_failures_follow_fixes_filtering` | A |
| `TestUnansweredMatchesAnswersByDefectID` | `togi_defect::tests::unanswered_matches_by_id`<br>`togi_defect::tests::unanswered_is_in_journal_order` | A |
| `TestIndexExtendedEventByEventAnswersLikeAFreshScan` | `togi_defect::tests::fold_prefix_equals_fresh_fold` | A |

## facts

### `internal/facts/carried_test.go` (4)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestCarriedFactsKeepOriginalProvenance` | `togi_facts::tests::carried::carried_facts_keep_original_provenance`<br>`togi_facts::tests::carried::carried_payload_round_trip_matches_the_recorded_idle_failure`<br>`togi_facts::tests::carried::carried_facts_must_be_decisive_well_formed` | A |
| `TestCarriedEventsDoNotChangeTuning` | — | O(carried passes and failures count as evidence (tuner.md#evidence)) |
| `TestCarryReaderReconstructsHistoricalProfiles` | — | O(the native trial.intent records the full profile) |
| `TestRecordOnlyFactsRetainTrialClassAndOutcomes` | `togi_facts::tests::decisive::decisive_facts` | O(there is no record_only; class and outcome are pinned by the test listed) |

### `internal/facts/facts_test.go` (9)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestReadJournalDecisiveFacts` | `togi_facts::tests::decisive::decisive_facts`<br>`togi_facts::tests::decisive::failure_keeps_attribution_and_telemetry` | A |
| `TestIdleFailures` | `togi_facts::tests::idle::idle_failures`<br>`togi_facts::tests::idle::idle_class_wildcard_independent_of_diagnostic_regime` | A |
| `TestResetHistoryPreservesFacts` | `togi_facts::tests::resets::reset_history_preserves_facts`<br>`togi_facts::tests::resets::archive_applies_outstanding_all_resets_without_double_effect`<br>`togi_facts::tests::resets::latest_reset_per_core_wins` | A |
| `TestReadDirNumericSessionOrder` | `togi_journal::tests::session_id::order_is_time_then_numeric_suffix`<br>`togi_journal::dir::tests::archives_list_regular_journals_in_numeric_session_order` | J — session order is `SessionId` ordering; facts consumes an ordered stream |
| `TestHistoricalConfigurationAndReadErrors` | — | O(there is no historical config or unknown kind to translate; corruption belongs to the journal reader) |
| `TestUnstampedConfigKeepsRecordedBuild` | `togi_facts::tests::decisive::stamps_and_markers_come_from_their_own_events` | O(build stamps are mandatory; the test listed pins the stamps) |
| `TestTrialEvidenceTracksOnlyOpenStartedTrialInItsBoot` | `togi_facts::tests::activity::trial_activity_scoped_to_open_started_trial`<br>`togi_facts::tests::activity::trial_start_from_the_wrong_boot_does_not_start_the_trial`<br>`togi_facts::tests::activity::trial_start_after_end_does_nothing` | A |
| `TestCrashEvidenceMatchesIntentSequenceAndPreviousBoot` | `togi_facts::tests::activity::crash_evidence_matches_intent_sequence_and_previous_boot` | A |
| `TestReadDirRejectsBrokenSources` | `togi_facts::tests::stream::extract_propagates_read_error_no_partial_result` | A — directory listing and decoding are journal reader tests |

### `internal/facts/known_failure_skip_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestKnownFailureSkipDoesNotCreateIdleFact` | `togi_facts::tests::idle::known_failure_skip_does_not_create_idle_fact` | A |

### `internal/facts/requests_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestReadJournalSummarizesPersistedRequests` | — | O(sample backfill is deleted (trial.end records the request summary); summary thresholds stay with togi-requests) |

## carry

### `internal/carry/backend_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestCarriedPassesKeyedByBackendStorePath` | `togi_carry::tests::resolution::resolve_drops_only_changed_backend_passes_keeps_failures` | A |
| `TestCopiedPassKeepsItsOriginalBackend` | `togi_carry::tests::resolution::copied_pass_keeps_original_backend` | A — original-archive repair is obsolete; the original identity wins on the native payload |

### `internal/carry/carry_test.go` (17)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestPrepareSeedsSoloLimitsAndFailurePoints` | `togi_carry::tests::seeds::seeds_solo_limits_and_failure_points` | A |
| `TestPrepareBIOSChange` | `togi_carry::tests::seeds::bios_change_archives_metadata_carry_available` | A — the archive verdict is the journal's; the "no current context" case is obsolete (the context is a required argument) |
| `TestPrepareResumesInterruptedBIOSArchive` | `togi_carry::tests::seeds::select_idempotent` | A — the marker and archive protocol is the journal's |
| `TestPrepareCarriesHuntCulprit` | `togi_carry::tests::seeds::hunt_culprit_seed_sparse_topology`<br>`togi_carry::tests::seeds::only_a_singleton_culprit_of_a_known_hunt_seeds` | A |
| `TestPrepareDropsWhatAResetCleared` | `togi_carry::tests::seeds::resets_drop_seeds` | A |
| `TestPrepareSkipsFailuresBehindADefect` | `togi_carry::tests::seeds::defect_excluded_failure_not_seeded`<br>`togi_carry::tests::seeds::excluded_failure_falls_back_to_the_next_shallowest` | A |
| `TestPrepareRecordsFailurePointForTrialInFlight` | `togi_carry::tests::seeds::interrupted_alone_nonzero_seeds_crash_failure` | A — the record-only case is obsolete |
| `TestPrepareIgnoresATrialStoppedByAShutdown` | `togi_carry::tests::seeds::shutdown_stops_in_flight_seed` | A |
| `TestPrepareChainsACarryAndWalksNoFurther` | `togi_carry::tests::seeds::commitment_stops_seed_walk` | A |
| `TestPrepareWalksBackThroughUnseededTransitions` | `togi_carry::tests::seeds::walk_back_through_unseeded_transitions`<br>`togi_carry::tests::seeds::reset_all_in_source_stops_walk` | A — the schema-1 input is obsolete |
| `TestPrepareLifecycle` | `togi_journal::journal::tests::archive::a_transition_leaves_a_pending_carry_and_no_boundary`<br>`togi_journal::journal::tests::archive::the_next_session_starts_empty_and_the_archive_stays_immutable` | J — archive, carry marker and immutable source; carry selection is idempotent (`select_idempotent`) |
| `TestPrepareLeavesANewerJournal` | `togi_journal::tests::build::resume_uses_current_archives_older_and_refuses_any_newer_dimension` | J — the verdict for a newer journal |
| `TestPrepareCarriesNothingFromAnArchivedJournal` | `togi_journal::journal::tests::archive::open_finishes_a_recorded_archive_even_when_a_torn_tail_follows_it` | J — a recorded archive, with and without a torn tail |
| `TestPrepareRefusesALineWithoutAKind` | `togi_journal::tests::strict::a_line_without_a_kind_is_an_envelope_error`<br>`togi_journal::tests::strict::open_refuses_a_journal_line_without_a_kind_and_leaves_the_file_unchanged`<br>`togi_journal::tests::strict::envelope_errors_without_a_known_kind_do_not_invent_one`<br>`togi_journal::tests::strict::unknown_kind_is_named` | J — envelope decode errors; a line with the `kind` member removed is refused, and open leaves the file unchanged |
| `TestPrepareDoesNotSeedAfterCorePhases` | `togi_carry::tests::seeds::settled_source_not_seeded_after_core_phase` | A — marker clearing is the journal's |
| `TestPrepareWalkStopsAtOlderCarryCommitment` | `togi_carry::tests::seeds::walk_stops_at_older_commitment` | A |
| `TestPrepareRefusesBrokenArchiveLayoutWithoutChangingJournal` | `togi_journal::journal::tests::an_archive_that_is_not_a_directory_fails_the_lookup`<br>`togi_journal::journal::tests::archive::an_archive_that_already_exists_is_refused_before_anything_is_recorded`<br>`togi_journal::journal::tests::archive::an_archive_target_that_is_a_directory_is_refused_before_anything_is_recorded`<br>`togi_journal::journal::tests::repair::a_contiguous_journal_that_does_not_begin_with_session_start_is_refused`<br>`togi_journal::tests::strict::open_refuses_a_journal_line_without_a_kind_and_leaves_the_file_unchanged` | J — archive is a file, archive target is a file or a directory, invalid session header; each leaves the live journal unchanged |

### `internal/carry/epoch_chain_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestInterruptedEpochTransitionRetainsOriginalSoloLimitsAndFailurePoints` | `togi_carry::tests::facts_walk::interrupted_epoch_transition_retains_original_seeds_and_failures` | A |

### `internal/carry/epoch_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestPrepareEvidenceEpochTransition` | `togi_carry::tests::facts_walk::same_epoch_transition_retains_bios_evidence` | A — deferral is the type; the unknown-kind policy is the journal's; the unstamped-ruleset default is obsolete |

### `internal/carry/fact_resets_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestResetOfLoadedCoreFiltersCopiedFacts` | `togi_carry::tests::facts_walk::reset_of_loaded_core_filters_copied_facts` | A |

### `internal/carry/facts_test.go` (8)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestFactEligibilityAcrossArchiveChain` | `togi_carry::tests::facts_walk::fact_eligibility_across_archive_chain` | A — 6 of 7 cases; "unknown current BIOS" is obsolete (the context is an argument) |
| `TestFactCopyForwardPreservesProvenanceAndStopsWalk` | `togi_carry::tests::facts_walk::fact_copy_forward_preserves_provenance_and_stops_walk` | A |
| `TestCopiedFailuresRecheckedAgainstOriginalDefects` | `togi_carry::tests::facts_walk::copied_failures_rechecked_against_original_defects`<br>`togi_carry::tests::facts_walk::original_outside_the_walk_is_rechecked_when_present`<br>`togi_carry::tests::facts_walk::copied_failures_without_the_original_archive_are_retained`<br>`togi_carry::tests::facts_walk::copied_failures_with_an_unreadable_original_stop_carry`<br>`togi_carry::tests::facts_walk::an_original_that_cannot_be_opened_stops_carry`<br>`togi_carry::tests::facts_walk::empty_registry_skips_original_lookup` | A |
| `TestDeferredFactsRequireContextAndRetryFailedRead` | `togi_carry::tests::facts_walk::resolve_requires_context_and_retries` | A |
| `TestFactWalkRejectsCorruptOlderArchive` | `togi_carry::tests::facts_walk::corrupt_older_source_is_error`<br>`togi_carry::tests::facts_walk::missing_listed_source_is_error` | A |
| `TestRecordOnlyFactsSurviveTwoTransitionsWithoutFailurePoints` | — | O(there is no record_only) |
| `TestRulesetSevenToEightRetainsSameBIOSEvidenceAndFailurePoints` | `togi_carry::tests::facts_walk::same_epoch_transition_retains_bios_evidence` | A — release numbering is obsolete |
| `TestCarryRequestTelemetryFromRecordedFieldsAndSamples` | `togi_carry::tests::facts_walk::copied_telemetry_dedup_prefix` | A — legacy sample repair is obsolete |

### `internal/carry/identity_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestArchiveSuffixesSortNumerically` | `togi_carry::tests::facts_walk::facts_sorted_by_session_id_then_seq`<br>`togi_journal::tests::session_id::order_is_time_then_numeric_suffix`<br>`togi_journal::dir::tests::suffixes_order_numerically_so_ten_follows_nine` | A — suffix ordering itself is `SessionId` ordering in togi-journal |

### `internal/carry/known_failure_hunt_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestPrepareCarriesHuntCulpritFromKnownFailure` | `togi_carry::tests::facts_walk::culprit_from_known_failure_keeps_source_signal` | A |

### `internal/carry/lineage_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestInterruptedTransitionRetainsOriginalPendingLineage` | `togi_journal::journal::tests::archive::a_transition_keeps_an_earlier_pending_carry_as_the_source` | J — pending-carry lineage |

### `internal/carry/lock_test.go` (3)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestCompetingTransitionCannotClearCarry` | `togi_journal::dir::tests::a_second_writer_is_refused_until_the_first_drops` | J — the state lock |
| `TestTransitionLockPrecedesPreparation` | `togi_journal::journal::tests::the_writer_lock_is_held_until_close_or_drop`<br>`togi_journal::dir::tests::a_second_writer_is_refused_until_the_first_drops` | J — the state lock |
| `TestTransitionRefusesCorruptCurrentSchemaWithoutMutation` | `togi_journal::journal::tests::repair::a_complete_line_that_does_not_decode_is_corruption_and_nothing_is_rewritten`<br>`togi_journal::journal::tests::repair::a_gap_and_a_forward_cause_are_refused` | J — a malformed complete line and a discontinuous sequence, refused without mutation |

### `internal/carry/real_journals_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestPrepareRealArchiveChain` | — | O(three real Go archives; Rust never reads Go journals) |

### `internal/carry/reset_boundary_test.go` (4)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestFactPrefixNeedsCarryCommitmentToStopTraversal` | `togi_carry::tests::boundary::fact_prefix_needs_commitment_to_stop_walk` | A |
| `TestFactResetBoundaryExcludesDiscardedSourcesAndCopies` | `togi_carry::tests::boundary::reset_boundary_excludes_discarded_sources_and_copies` | A |
| `TestResetBoundaryFiltersCopiedCandidateValues` | `togi_carry::tests::boundary::reset_boundary_filters_copied_seed_values` | A |
| `TestResetBoundaryFinishesInterruptedPendingDrop` | `togi_journal::dir::tests::a_fault_anywhere_in_dropping_a_carry_never_loses_the_boundary_or_the_carry`<br>`togi_journal::dir::tests::pending_markers_clear_but_the_reset_boundary_is_permanent` | J — the reset boundary and the pending drop |

### `internal/carry/reset_recovery_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestInterruptedResetArchivesDiscardedLiveSessionBeforeResume` | `togi_journal::journal::tests::archive::the_boundary_is_durable_before_a_recorded_reset_all_moves_anything`<br>`togi_journal::journal::tests::archive::a_recorded_reset_all_still_leaves_the_permanent_boundary`<br>`togi_journal::journal::tests::archive::reset_all_archives_the_session_behind_a_durable_boundary` | J — an interrupted `reset --all` archive |

### `internal/carry/state_dir_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestPrepareReadsArchivesOfPatternLikeStateDir` | `togi_journal::dir::tests::archives_list_regular_journals_in_numeric_session_order` | O(a regression test for Go globbing the state path; the journal lists archives with `read_dir` and never reads the path as a pattern (listing pinned by the test listed)) |

### `internal/carry/zero_carry_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestPrepareCarriesOnlyConfirmedFailurePointsAtZero` | `togi_carry::tests::zero::only_confirmed_zero_failure_points` | A |
| `TestPrepareChecksCarriedFailurePointsAtZeroAgainstTheirSession` | `togi_carry::tests::zero::zero_proof_persisted_in_seed` | A — a missing original no longer loses the proof |

## tuner

### `internal/tuner/backend_identity_test.go` (3)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestEvidenceIsKeyedByBackendStorePath` | `togi_tuner::ledger_tests::evidence_is_keyed_by_backend_store_path`<br>`togi_tuner::evidence::tests::evidence_is_keyed_by_backend_store_path` | A |
| `TestBackendUpdateRestartsCarriedSoloLimitEvidence` | `togi_tuner::ledger_tests::a_backend_update_restarts_carried_solo_limit_evidence` | A |
| `TestBackendUpdateRestartsUnfinishedSearchStep` | `togi_tuner::ledger_tests::a_backend_update_restarts_an_unfinished_search_step` | A |

### `internal/tuner/carried_test.go` (9)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestCarriedEvidenceConsumers` | `togi_tuner::ledger_tests::carried_evidence_answers_a_candidate_solo_limit_but_not_an_ordinary_step`<br>`togi_tuner::checking_tests::carried_passes_never_fulfill_a_checking_cycle_step`<br>`togi_tuner::rerun_tests::carried_rerun_citation_replay`<br>`togi_tuner::rounds_tests::carried_passes_satisfy_every_round_check_and_are_cited`<br>`togi_tuner::chain_tests::partial_passes_are_ordinary_evidence`<br>`togi_tuner::hunt::locate_tests::carried_passes_establish_a_hunt_group`<br>`togi_tuner::hunt::locate_tests::carried_evidence_before_the_hunt_establishes_a_member_probe`<br>`togi_tuner::dashboard_tests::carried_evidence_consumers`<br>`togi_tuner::projection_tests::carried_passes_count_in_the_deepening_projection` | A |
| `TestCarriedFailureInvalidation` | `togi_tuner::ledger_tests::carried_failure_invalidation`<br>`togi_tuner::evidence::tests::carried_failure_invalidation` | A |
| `TestResetClearsCarriedCoreEvidence` | `togi_tuner::ledger_tests::reset_clears_carried_core_evidence`<br>`togi_tuner::evidence::tests::reset_clears_carried_core_evidence` | A |
| `TestCarriedSoloLimitEligibility` | `togi_tuner::ledger_tests::carried_solo_limit_eligibility` | A |
| `TestCarriedFailureEstablishesLaterHuntGroup` | `togi_tuner::hunt::locate_tests::carried_failure_establishes_later_hunt_group`<br>`togi_tuner::hunt::locate_tests::carried_evidence_before_the_hunt_establishes_a_member_probe` | A |
| `TestCarriedPassMonotonicityWarning` | `togi_tuner::ledger_tests::carried_pass_monotonicity_warning` | A |
| `TestResetClearsCarriedIdleFailure` | `togi_tuner::ledger_tests::reset_clears_carried_idle_failure`<br>`togi_tuner::evidence::tests::reset_clears_carried_idle_failure` | A |
| `TestCarriedPassesDoNotAddSessionExposure` | `togi_tuner::ledger_tests::carried_passes_do_not_add_session_exposure`<br>`togi_tuner::evidence::tests::carried_passes_do_not_add_session_exposure`<br>`togi_tuner::evidence::tests::exposure_counts_live_non_alone_active_backend_passes_after_the_latest_failure` | A |
| `TestCarriedFailureSurvivesNewerPreBoundaryLiveFailure` | `togi_tuner::evidence::tests::carried_failure_survives_newer_pre_boundary_live_failure`<br>`togi_tuner::ledger_tests::carried_failure_survives_newer_pre_boundary_live_failure`<br>`togi_tuner::hunt::locate_tests::a_carried_failure_is_admitted_to_a_group_past_a_newer_live_failure_before_the_hunt` | A |

### `internal/tuner/checking_partial_test.go` (11)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestR7ChainDerivesFromCompletedPredecessor` | `togi_tuner::chain_tests::r7_chain_derives_from_completed_predecessor` | A |
| `TestR7ChainRequiresAllPredecessorPasses` | `togi_tuner::chain_tests::r7_chain_requires_all_predecessor_passes` | A |
| `TestR7ChainRederivesAfterProfileChange` | `togi_tuner::chain_tests::r7_chain_rederives_after_profile_change_unstarted_node`<br>`togi_tuner::chain_tests::r7_chain_rederives_after_profile_change_inconclusive_set_unchanged`<br>`togi_tuner::chain_tests::r7_chain_rederives_after_profile_change_inconclusive_set_changed`<br>`togi_tuner::chain_tests::r7_chain_rederives_after_profile_change_passed_set_unchanged`<br>`togi_tuner::chain_tests::r7_chain_rederives_after_profile_change_passed_set_changed`<br>`togi_tuner::chain_tests::r7_chain_rederives_after_profile_change_satisfied_parent_and_started_child`<br>`togi_tuner::chain_tests::r7_chain_rederives_after_profile_change_on_the_other_ccd_only`<br>`togi_tuner::chain_tests::r7_chain_rederives_after_profile_change_several_backoffs_before_revisiting`<br>`togi_tuner::chain_tests::r7_chain_rederives_after_profile_change_one_ccd`<br>`togi_tuner::chain_tests::r7_chain_rederives_after_profile_change_repeated_step_of_one_workload` | A |
| `TestR7ChainRerunsAFailedClassThatLeftTheChain` | `togi_tuner::chain_tests::r7_chain_reruns_a_failed_class_that_left_the_chain` | A |
| `TestR7ChainRetestsReorderedPartAfterBackoff` | `togi_tuner::chain_tests::r7_chain_retests_reordered_part_after_backoff` | A |
| `TestLegacyPartialPassesAreOrdinaryEvidence` | `togi_tuner::chain_tests::partial_passes_are_ordinary_evidence` | A — the legacy marker is gone |
| `TestR7FullCycleIncludesEveryPartial` | `togi_tuner::chain_tests::r7_full_cycle_includes_every_partial` | A |
| `TestR7OneCCDRunsFullThenChainOnce` | `togi_tuner::chain_tests::r7_one_ccd_runs_full_then_chain_once` | A |
| `TestR7ChainIdlesRequestTiesTogether` | `togi_tuner::chain_tests::r7_chain_idles_request_ties_together` | A |
| `TestR7PartialFailureDoesNotCompleteItsRequirement` | `togi_tuner::chain_tests::r7_partial_failure_does_not_complete_its_requirement` | A |
| `TestRepeatedR7PartialClassesAddTrials` | `togi_tuner::chain_tests::repeated_r7_partial_classes_add_trials` | A |

### `internal/tuner/checking_state_test.go` (6)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestConcludedPhasesReplay` | `togi_tuner::phase_tests::phases_run_to_a_conclusion_and_count_clean_cycles` | A |
| `TestUnpassedCycleStepsDoneReplays` | `togi_tuner::checking_state_tests::unpassed_cycle_steps_done_replays` | A |
| `TestCleanCyclesCountFromPhase2Conclusion` | `togi_tuner::phase_tests::phases_run_to_a_conclusion_and_count_clean_cycles` | A |
| `TestResetReturnsToPhase1` | `togi_tuner::phase_tests::phases_run_to_a_conclusion_and_count_clean_cycles` | A |
| `TestTemperaturePeakSinceLastProfileChange` | `togi_tuner::checking_state_tests::temperature_peak_since_last_profile_change` | A |
| `TestTemperatureCountsOnlyTogetherPasses` | `togi_tuner::checking_state_tests::temperature_counts_only_together_passes_with_a_reading` | A |

### `internal/tuner/checking_test.go` (14)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestCycleCoverage` | `togi_tuner::checking_tests::cycle_coverage` | A |
| `TestR7TrialsAndSharedDuration` | `togi_tuner::checking_tests::r7_trials_and_shared_duration` | A |
| `TestAttributionAtGroupParkedOffsetsAndAlreadyShallower` | `togi_tuner::hunt::locate_tests::attribution_at_group_parked_offsets_and_already_shallower` | A |
| `TestRerunLongFailedPart` | `togi_tuner::rerun_tests::rerun_long_failed_part` | A |
| `TestRerunRepeatedCommitmentCitesLatestFailure` | `togi_tuner::rerun_tests::rerun_repeated_commitment_cites_latest_failure` | A |
| `TestCheckingCarriesDeeperPassAcrossBackoff` | `togi_tuner::rerun_tests::checking_carries_deeper_pass_across_backoff` | A |
| `TestTogetherSingleNonzeroAttribution` | `togi_tuner::checking_tests::together_single_nonzero_attribution` | A |
| `TestCycleStartInvalidatesProjectedChecking` | `togi_tuner::projection_tests::cycle_start_invalidates_projected_checking` | A — cache invalidation is obsolete; the observable behaviour is kept |
| `TestTogetherMultipleMCECoresRemainUnattributed` | `togi_tuner::checking_tests::together_multiple_mce_cores_remain_unattributed`<br>`togi_tuner::rule_tests::together_failures_name_the_core_the_end_or_one_cited_mce_names` | A |
| `TestRerunDerivedCommitments` | `togi_tuner::rerun_tests::rerun_derived_commitments`<br>`togi_tuner::hunt::locate_tests::rerun_derived_hunt_commitments` | A |
| `TestRerunFIFOAndSharedDuration` | `togi_tuner::rerun_tests::rerun_fifo_and_shared_duration` | A |
| `TestRepeatedCycleClassesAddTrials` | `togi_tuner::checking_tests::repeated_cycle_classes_add_trials` | A |
| `TestDrainStopsAtTrialBoundary` | `togi_tuner::drain_tests::drain_stops_at_trial_boundary` | A |
| `TestDrainRejectsUnattributedFailureAtZero` | `togi_tuner::drain_tests::drain_rejects_unattributed_failure_at_zero` | A |

### `internal/tuner/dashboard_test.go` (13)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestCoreLimitHolders` | `togi_tuner::dashboard_tests::core_limit_holders` | A |
| `TestCyclePlanDynamicChainRequirements` | `togi_tuner::dashboard_tests::cycle_plan_dynamic_chain_requirements` | A |
| `TestCyclePlanPartialFailureNeedsPasses` | `togi_tuner::dashboard_tests::cycle_plan_partial_failure_needs_passes` | A |
| `TestCyclePlanPartialProfileChanges` | `togi_tuner::dashboard_tests::cycle_plan_partial_profile_changes` | A |
| `TestCyclePlanRepeatedPartialRequirements` | `togi_tuner::dashboard_tests::cycle_plan_repeated_partial_requirements` | A |
| `TestCyclePlanPartialEvidenceWindows` | `togi_tuner::dashboard_tests::cycle_plan_partial_evidence_windows` | A |
| `TestCyclePlanOneCCDMergesCoincidentDurations` | `togi_tuner::dashboard_tests::cycle_plan_one_ccd_merges_coincident_durations` | A |
| `TestRequirementCountsOnlyFreshSearchTrials` | `togi_tuner::dashboard_tests::requirement_counts_only_fresh_search_trials` | A |
| `TestRequirementCountsAloneDeepeningChecksAgainstTheirRound` | `togi_tuner::dashboard_tests::requirement_counts_alone_deepening_checks_against_their_round` | A |
| `TestCyclePlanRunsNoPartDuringARerun` | `togi_tuner::dashboard_tests::cycle_plan_runs_no_part_during_a_rerun` | A |
| `TestCyclePlanRunsOnlyTheCurrentStepsPart` | `togi_tuner::dashboard_tests::cycle_plan_runs_only_the_current_steps_part` | A |
| `TestHuntCutCountsSplitsAcrossTheHunt` | `togi_tuner::dashboard_tests::hunt_cut_counts_splits_across_the_hunt`<br>`togi_tuner::dashboard_tests::hunt_plan_cut_counts_an_upcoming_distinct_set_as_a_new_cut` | A |
| `TestHuntPlanCutAdvancesWhenPassingSplitsReturnToTheSingletonGranularity` | `togi_tuner::dashboard_tests::hunt_plan_cut_advances_when_passing_splits_return_to_the_singleton_granularity` | A |

### `internal/tuner/deepening_r7_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestDeepeningRechecksOwnPartialForEveryR7Workload` | `togi_tuner::rounds_tests::deepening_rechecks_its_own_partial_for_every_r7_workload` | A |
| `TestDeepeningRequestRankChangeAddsCCDWholePart` | `togi_tuner::rounds_tests::a_request_rank_change_adds_the_ccd_whole_part` | A |

### `internal/tuner/deepening_test.go` (3)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestIdleFailureEndsDeepeningBeforeMoves` | `togi_tuner::rounds_tests::an_idle_failure_ends_deepening_before_moves` | A |
| `TestActiveDeepeningProjection` | `togi_tuner::projection_tests::active_deepening_projection`<br>`togi_tuner::drain_tests::active_deepening_projection` | A |
| `TestLiveDeepeningFailureEndsRoundBeforeBackoff` | `togi_tuner::rounds_tests::a_live_deepening_failure_ends_the_round_before_the_backoff` | A |

### `internal/tuner/escalate_test.go` (4)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestEscalationBoundaries` | `togi_tuner::r7_escalation_tests::escalation_boundaries` | A |
| `TestEscalationCountSurvivesResume` | `togi_tuner::r7_escalation_tests::escalation_count_survives_resume` | A |
| `TestEscalationWhenLoadedCoresCannotBackOff` | `togi_tuner::r7_escalation_tests::escalation_when_loaded_cores_cannot_back_off` | A |
| `TestEscalationResetDropsDiscardedBackoffs` | `togi_tuner::r7_escalation_tests::escalation_reset_drops_discarded_backoffs` | A |

### `internal/tuner/evidence_test.go` (4)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestEvidenceValidity` | `togi_tuner::ledger_tests::evidence_validity` | A |
| `TestMonotonicityWarning` | `togi_tuner::ledger_tests::monotonicity_warning`<br>`togi_tuner::evidence::tests::monotonicity_warning` | A |
| `TestIdleMonotonicityWarning` | `togi_tuner::evidence::tests::idle_monotonicity_warning`<br>`togi_tuner::ledger_tests::idle_monotonicity_warning`<br>`togi_tuner::hunt::hunt_tests::a_warned_idle_failure_still_opens_its_hunt` | A |
| `TestIdleMonotonicityChoosesEarliestPassingClass` | `togi_tuner::ledger_tests::idle_monotonicity_chooses_earliest_passing_class`<br>`togi_tuner::evidence::tests::idle_monotonicity_chooses_earliest_passing_class` | A |

### `internal/tuner/forecast_sim_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestForecastMatchesSimulatedTrials` | — | S4(simulated-trial forecast check, stage 4 simulator) |
| `BenchmarkDashboardProjectionForecast` | — | S4(fixture-replay benchmark, stage 4 simulator) |

### `internal/tuner/forecast_test.go` (18)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestForecastFormerRecordOnlyPartialRequiresBackoff` | `togi_tuner::forecast_tests::forecast_partial_failure_requires_backoff` | A |
| `TestForecastAllPassCountsPartialPasses` | `togi_tuner::forecast_tests::forecast_all_pass_counts_partial_passes` | A |
| `TestForecastNeedsRankingWithoutRecordedRead` | `togi_tuner::forecast_tests::forecast_needs_ranking_without_recorded_read` | A |
| `TestForecastSingleLoadedTogetherMayBeUnattributed` | `togi_tuner::forecast_tests::forecast_single_loaded_together_may_be_unattributed` | A |
| `TestHuntPlanRunningMemberIsNotDone` | `togi_tuner::dashboard_tests::hunt_plan_running_member_is_not_done` | A |
| `TestForecastInconclusiveRetainsTrial` | `togi_tuner::forecast_tests::forecast_inconclusive_retains_trial` | A |
| `TestForecastReusesRecordedRanking` | `togi_tuner::forecast_tests::forecast_reuses_recorded_ranking` | A |
| `TestForecastWaitsForInitialCorePhases` | `togi_tuner::forecast_tests::forecast_waits_for_initial_core_phases` | A — the "preparing" intent half is obsolete: an unscheduled intent is a fold error |
| `TestForecastDetectsRecurringHuntPlan` | `togi_tuner::forecast_tests::forecast_detects_recurring_hunt_plan` | A |
| `TestHuntProbeWithoutRecordedCheckingProfile` | `togi_tuner::dashboard_tests::hunt_probe_without_recorded_checking_profile`<br>`togi_tuner::forecast_tests::hunt_probe_without_recorded_checking_profile_forecast` | A |
| `TestForecastAllPassStaysWithinItsRequirement` | `togi_tuner::forecast_tests::forecast_all_pass_stays_within_its_requirement` | A |
| `TestForecastNamesCoreAtZeroSeparately` | `togi_tuner::forecast_tests::forecast_names_core_at_zero_separately` | A |
| `TestForecastR7PremisesFoldDistinctOutcomesWithoutInventedTelemetry` | `togi_tuner::forecast_tests::forecast_r7_premises_fold_distinct_outcomes_without_invented_telemetry` | A |
| `TestForecastR7UnnamedUsesOffsetFallbackWithoutRequests` | `togi_tuner::forecast_tests::forecast_r7_unnamed_uses_offset_fallback_without_requests` | A |
| `TestForecastR7NamesACoreAtZeroOnEachCCD` | `togi_tuner::forecast_tests::forecast_r7_names_a_core_at_zero_on_each_ccd` | A |
| `TestForecastR7BasisFollowsTheRunningLoadsMeasurement` | `togi_tuner::forecast_tests::forecast_r7_basis_follows_the_running_loads_measurement` | A |
| `TestForecastUnmeasuredChainEndingFollowsOffsetOrder` | `togi_tuner::forecast_tests::forecast_unmeasured_chain_ending_follows_offset_order` | A |
| `TestForecastResumeNamesNoStepOfALaterCycle` | `togi_tuner::forecast_tests::forecast_resume_names_no_step_of_a_later_cycle` | A |

### `internal/tuner/harness_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestTrialCompletionSelectsReducerWorkload` | `togi_tuner::schedule_tests::trial_plan_intent_rotation` | A — the invalid-workload case is unrepresentable |

### `internal/tuner/hunt_carried_completion_test.go` (3)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestRunningHuntGroupCitesMixedCarriedAndLivePasses` | `togi_tuner::hunt::hunt_tests::running_hunt_group_cites_mixed_carried_and_live_passes` | A |
| `TestHuntPlanMarksOnlyGroupsAnsweredByCarriedEvidence` | `togi_tuner::hunt::hunt_tests::hunt_plan_marks_only_groups_answered_by_carried_evidence` | A |
| `TestHuntPlanKeepsACarriedFailureAnswerAfterLaterLiveFailures` | `togi_tuner::hunt::hunt_tests::hunt_plan_keeps_a_carried_failure_answer_after_later_live_failures` | A |

### `internal/tuner/hunt_fixed_order_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestHuntKeepsOnePartOrderWhenRecentMovesMidHunt` | `togi_tuner::hunt::provenance_tests::hunt_keeps_one_part_order_when_recent_moves_mid_hunt` | A |

### `internal/tuner/hunt_provenance_test.go` (5)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestHuntCommitmentsCiteDecisiveLiveFailures` | `togi_tuner::hunt::provenance_tests::hunt_commitments_cite_decisive_live_failures` | A |
| `TestHuntCausesOmitAFailureAfterTheGroupPassed` | `togi_tuner::hunt::provenance_tests::hunt_causes_omit_a_failure_after_the_group_passed` | A |
| `TestHuntCausesOmitAFailureCoveredBeforeTheGroupPassed` | `togi_tuner::hunt::provenance_tests::hunt_causes_omit_a_failure_covered_before_the_group_passed` | A |
| `TestHuntEndCitesALiveFailureInferredGroup` | `togi_tuner::hunt::provenance_tests::hunt_end_cites_a_live_failure_inferred_group` | A |
| `TestHuntLiveFailureCausesFixtureReplay` | `togi_tuner::hunt::provenance_tests::hunt_live_failure_causes_native_fixture` | A — the Go fixture is replaced by a native causal graph |

### `internal/tuner/hunt_test.go` (28)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestHuntParkedOffsetsRaisesThePassedFullCycleProfile` | `togi_tuner::hunt::hunt_tests::hunt_parked_offsets_raises_the_passed_full_cycle_profile` | A |
| `TestHuntParkedOffsetsSelectionAndReset` | `togi_tuner::hunt::hunt_tests::hunt_parked_offsets_selection_and_reset` | A |
| `TestAllZeroHuntParkedOffsetsOmitsZeroCause` | `togi_tuner::hunt::hunt_tests::all_zero_hunt_parked_offsets_omits_zero_cause` | A |
| `TestHuntParkedOffsetsSkipsExactPassedFullCycleProfile` | `togi_tuner::hunt::hunt_tests::hunt_parked_offsets_skips_exact_passed_full_cycle_profile` | A |
| `TestHuntSkipsPreviouslyReachedFailure` | `togi_tuner::hunt::hunt_tests::hunt_skips_previously_reached_failure` | A |
| `TestHuntLoadedIdleSplit` | `togi_tuner::hunt::hunt_tests::hunt_loaded_idle_split` | A |
| `TestNextHuntReusesEstablishedPartGroups` | `togi_tuner::hunt::hunt_tests::next_hunt_reuses_established_part_groups` | A |
| `TestHuntGroupReuseAfterReset` | `togi_tuner::hunt::hunt_tests::hunt_group_reuse_after_reset` | A |
| `TestRunningHuntGroupReusesEarlierPasses` | `togi_tuner::hunt::hunt_tests::running_hunt_group_reuses_earlier_passes` | A |
| `TestHuntGroupDoesNotReuseShallowerOrIncomparablePasses` | `togi_tuner::hunt::hunt_tests::hunt_group_does_not_reuse_shallower_or_incomparable_passes` | A |
| `TestHuntSkipsReachedGroup` | `togi_tuner::hunt::hunt_tests::hunt_skips_reached_group` | A |
| `TestRepeatedParkedCoreProbeReturnsToBinaryPartsAfterPass` | `togi_tuner::hunt::hunt_tests::repeated_parked_core_probe_returns_to_binary_parts_after_pass` | A |
| `TestRepeatedParkedCoreProbeRequiresMatchingAdjacentFailuresSinceReset` | `togi_tuner::hunt::hunt_tests::repeated_parked_core_probe_requires_matching_adjacent_failures_since_reset` | A |
| `TestHuntPairAndCommitmentResume` | `togi_tuner::hunt::hunt_tests::hunt_pair_and_commitment_resume` | A |
| `TestCombinationBackoffMovesToATestedProbe` | `togi_tuner::hunt::hunt_tests::combination_backoff_moves_to_a_tested_probe` | A |
| `TestHuntFallbackAndFullCheck` | `togi_tuner::hunt::hunt_tests::hunt_fallback_and_full_check` | A |
| `TestHuntCombinationAlreadyBroken` | `togi_tuner::hunt::hunt_tests::hunt_combination_already_broken` | A |
| `TestHuntCulpritDiscardsContradictedPass` | `togi_tuner::hunt::hunt_tests::hunt_culprit_discards_contradicted_pass` | A |
| `TestSixteenCoreCulpritAndPair` | `togi_tuner::hunt::hunt_tests::sixteen_core_culprit_and_pair` | A |
| `TestHuntDurationPriorRespectsEvidenceAndShortFailures` | `togi_tuner::hunt::hunt_tests::hunt_duration_prior_respects_evidence_and_short_failures` | A |
| `TestHuntDurationEscalationOnlyForLongerFailure` | `togi_tuner::hunt::hunt_tests::hunt_duration_escalation_only_for_longer_failure` | A |
| `TestHuntEscalationAndInferredGroup` | `togi_tuner::hunt::hunt_tests::hunt_escalation_and_inferred_group` | A |
| `TestActiveHuntProjection` | `togi_tuner::hunt::hunt_tests::active_hunt_projection`<br>`togi_tuner::dashboard_tests::active_hunt_projection`<br>`togi_tuner::dashboard_tests::project_assembles_the_active_hunt` | A |
| `TestDrainCommitsResolvedHuntWithoutStartingGroups` | `togi_tuner::hunt::hunt_tests::drain_commits_resolved_hunt_without_starting_groups`<br>`togi_tuner::drain_tests::drain_commits_resolved_hunt_without_starting_groups` | A |
| `TestDrainRecordsFallbackCombinationBeforeBackoff` | `togi_tuner::hunt::hunt_tests::drain_records_fallback_combination_before_backoff`<br>`togi_tuner::drain_tests::drain_records_fallback_combination_before_backoff` | A |
| `TestLocatedHuntResumesDuringLocate` | `togi_tuner::hunt::hunt_tests::located_hunt_resumes_during_locate` | A |
| `TestParkedFailureNamingACoreAtZeroRerunsAllZero` | `togi_tuner::hunt::hunt_tests::parked_failure_naming_a_core_at_zero_reruns_all_zero` | A |
| `TestTogetherFailureNamingACoreAtZeroIsHuntedAfterAPassedRerun` | `togi_tuner::hunt::hunt_tests::together_failure_naming_a_core_at_zero_is_hunted_after_a_passed_rerun` | A |

### `internal/tuner/known_failure_probe_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestKnownFailureSkipCompletesRunningMemberProbeGroup` | `togi_tuner::hunt::hunt_tests::known_failure_skip_completes_running_member_probe_group` | A |

### `internal/tuner/later_test.go` (29)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestLaterGateClosedStepsBackAtOnce` | `togi_tuner::later_tests::gate_closed_steps_back_at_once` | A |
| `TestLaterFirstFailureHoldsAndSecondStepsBackNamingBoth` | `togi_tuner::later_tests::first_failure_holds_and_second_steps_back_naming_both` | A |
| `TestLaterHoldKeepsShowingItsStepBackAfterAPassedCycle` | `togi_tuner::later_tests::hold_keeps_showing_its_step_back_after_a_passed_cycle` | A |
| `TestLaterFailuresOnDifferentCoresHoldSeparately` | `togi_tuner::later_tests::failures_on_different_cores_hold_separately` | A |
| `TestLaterWindowPairsWithinKCyclesAndExpiresAtK` | `togi_tuner::later_tests::window_pairs_within_k_cycles_and_expires_at_k` | A |
| `TestLaterStrikeIsInvalidWhenItsCoreMovedForAnotherReason` | `togi_tuner::later_tests::strike_is_invalid_when_its_core_moved_for_another_reason` | A |
| `TestLaterGateReopensOnlyAfterTheSteppedBackProfilePasses` | `togi_tuner::later_tests::gate_reopens_only_after_the_stepped_back_profile_passes` | A |
| `TestLaterStaleFailureAtADeeperOffsetIsNotAHold` | `togi_tuner::later_tests::stale_failure_at_a_deeper_offset_is_not_a_hold` | A |
| `TestLaterDrainEmitsTheSameHoldAndPair` | `togi_tuner::drain_tests::later_drain_emits_the_same_hold_and_pair` | A |
| `TestLaterUnattributedFailureIsHeldBeforeItsHunt` | `togi_tuner::later_tests::an_unattributed_first_failure_is_held_by_skipping_its_hunt` | A |
| `TestLaterHeldUnattributedFailurePairsWithAnAttributedOneOnALoadedCore` | `togi_tuner::later_tests::a_held_unattributed_failure_pairs_with_an_attributed_one_on_a_loaded_core` | A |
| `TestLaterMultiCoreR7FailuresHoldWithoutCountingTowardEscalation` | `togi_tuner::later_tests::multi_core_r7_failures_hold_without_counting_toward_escalation` | A |
| `TestBIOSProfileTracksTheConfirmedProfile` | `togi_tuner::bios_tests::bios_profile_tracks_the_confirmed_profile` | A |
| `TestBIOSProfileMarksAStepBackSinceItsDecision` | `togi_tuner::bios_tests::bios_profile_marks_a_step_back_since_its_decision` | A |
| `TestLaterOffsetsNeverDeepenAfterConclusion` | `togi_tuner::later_tests::offsets_never_deepen_after_conclusion` | A |
| `TestBIOSProfileShowsAHeldFailureSteppedBackAtOnce` | `togi_tuner::bios_tests::bios_profile_shows_a_held_failure_stepped_back_at_once` | A |
| `TestBIOSProfileShowsAMultiCountR7HoldAtTheVoltageTarget` | `togi_tuner::bios_tests::bios_profile_shows_a_multi_count_r7_hold_at_the_voltage_target` | A |
| `TestBIOSProfileFollowsTheActualStepBackAtASecondFailure` | `togi_tuner::bios_tests::bios_profile_follows_the_actual_step_back_at_a_second_failure` | A |
| `TestBIOSProfileReturnsToTheHeldOffsetWhenTheHoldExpires` | `togi_tuner::bios_tests::bios_profile_returns_to_the_held_offset_when_the_hold_expires` | A |
| `TestLaterLocatedGroupFailureNamingALoadedCoreIsNotHeld` | `togi_tuner::hunt::locate_tests::a_located_group_failure_naming_a_loaded_core_is_not_held` | A |
| `TestLaterPairedHuntEndingDirectNamesBothFailuresInTheBackoff` | `togi_tuner::later_tests::a_paired_hunt_ending_direct_names_both_failures_in_the_backoff` | A |
| `TestBIOSProfileKeepsAPairedHuntsHoldShownUntilItsCommitment` | `togi_tuner::bios_tests::a_paired_hunts_hold_shows_until_its_commitment` | A |
| `TestBIOSProfileDropsAHoldTargetOnceItsCoreIsConfirmedAtIt` | `togi_tuner::bios_tests::a_hold_target_stops_showing_for_a_core_that_stepped_back_and_was_confirmed` | A |
| `TestBIOSProfileProjectsAPairedHuntsPendingBackoffPastItsCombination` | `togi_tuner::bios_tests::a_paired_hunts_pending_backoff_shows_past_its_combination` | A |
| `TestBIOSProfileProjectsAPairedHuntsDirectBackoffBeforeItIsRecorded` | `togi_tuner::bios_tests::a_paired_hunts_direct_backoff_shows_before_it_is_recorded` | A |
| `TestBIOSProfileNeverShowsWhatAPairedHuntFoundFailingWhileItsCommitmentIsPending` | `togi_tuner::bios_tests::the_shown_profile_never_reaches_what_a_paired_hunt_found_failing_while_its_commitment_is_pending` | A |
| `TestBIOSProfileNeverReachesAFailureAPairedHuntObservesBeforeItEnds` | `togi_tuner::bios_tests::the_shown_profile_never_reaches_a_failure_a_paired_hunt_observes_before_it_ends` | A |
| `TestBIOSProfileStepsOnlyTheHeldCoresOfAnObservedHuntFailure` | `togi_tuner::bios_tests::an_observed_hunt_failure_steps_only_the_cores_it_held` | A |
| `TestFirstResultNeedsAConfirmedProfile` | `togi_tuner::bios_tests::first_result_needs_a_confirmed_profile` | A |

### `internal/tuner/locate_test.go` (3)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestLocatedHuntRetestsTheFullFailingProfileBeforeEndingLoaded` | `togi_tuner::hunt::locate_tests::located_hunt_retests_the_full_failing_profile_before_ending_loaded` | A |
| `TestEscalatedLocatedHuntRetestsTheFullFailingProfileAtTheFailedDuration` | `togi_tuner::hunt::locate_tests::escalated_located_hunt_retests_the_full_failing_profile_at_the_failed_duration` | A |
| `TestResetOfACandidateCancelsTheLocatedHunt` | `togi_tuner::hunt::locate_tests::reset_of_a_candidate_cancels_the_located_hunt` | A |

### `internal/tuner/memo_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestMemosMatchRecomputation` | — | S4(stage 4 simulator; memo epochs are obsolete, `query_tests::fresh_fold_equals_incremental_every_prefix` checks fresh against incremental) |

### `internal/tuner/phase_plan_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestPhasePlan` | `togi_tuner::phase_plan_tests::phase_plan_phase1_has_no_candidates`<br>`togi_tuner::phase_plan_tests::phase_plan_a_passed_cycle_that_is_not_full_does_not_end_phase_1`<br>`togi_tuner::phase_plan_tests::phase_plan_no_core_can_move_leaves_the_confirmation_cycle`<br>`togi_tuner::phase_plan_tests::phase_plan_rounds_left_are_the_largest_gap_not_the_sum`<br>`togi_tuner::phase_plan_tests::phase_plan_a_failure_point_excludes_the_counts_it_blocks`<br>`togi_tuner::phase_plan_tests::phase_plan_an_open_round_counts_as_one_and_marks_its_movers`<br>`togi_tuner::phase_plan_tests::phase_plan_a_round_that_ended_unpassed_without_blame_carries_its_cores`<br>`togi_tuner::phase_plan_tests::phase_plan_a_round_rechecking_carried_cores_in_place_does_not_move_them`<br>`togi_tuner::phase_plan_tests::phase_plan_a_fresh_mover_shows_the_rounds_baseline`<br>`togi_tuner::phase_plan_tests::phase_plan_the_confirmation_cycle_leaves_no_rounds`<br>`togi_tuner::phase_plan_tests::phase_plan_after_conclusion_there_is_no_phase`<br>`togi_tuner::phase_plan_tests::phase_plan_a_reset_returns_to_phase_1`<br>`togi_tuner::phase_plan_tests::phase_plan_no_cores_no_plan` | A — one Rust test per Go subtest |
| `TestProjectFillsPhasesAndBIOS` | `togi_tuner::projection_tests::project_fills_phases_and_bios` | A |

### `internal/tuner/phases_test.go` (20)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestSearchExitStartsOneCountShallowerThanTheSoloLimit` | `togi_tuner::phase_tests::search_exit_starts_one_count_shallower_than_the_solo_limit` | A |
| `TestSoloLimitSurvivesAFailureDiscardingPass` | `togi_tuner::phase_tests::solo_limit_survives_a_failure_discarding_pass` | A |
| `TestPhase1NeverDeepensAndEndsAtItsFirstPassedFullCycle` | `togi_tuner::phase_tests::phase1_never_deepens_and_ends_at_its_first_passed_full_cycle`<br>`togi_tuner::phase_tests::phases_run_to_a_conclusion_and_count_clean_cycles`<br>`togi_tuner::phase_tests::a_cycle_cannot_pass_before_a_profile_change`<br>`togi_tuner::phase_tests::no_deepening_round_is_proposed_during_phase_1` | A |
| `TestPhase2Moves` | `togi_tuner::phase_tests::phase2_moves` | A |
| `TestPhase2RoundMovesEveryCandidateOneCountAndResumes` | `togi_tuner::rounds_tests::phase2_round_1_moves_every_candidate_one_count_from_the_phase1_cycle` | A |
| `TestPhase2RunsRoundsThenAlwaysTheConfirmationCycle` | `togi_tuner::rounds_tests::phase2_runs_rounds_then_always_the_confirmation_cycle` | A |
| `TestZeroMovableCandidatesStillRunTheFullConfirmationCycle` | `togi_tuner::rounds_tests::zero_movable_candidates_still_run_the_full_confirmation_cycle` | A |
| `TestZeroMovableCandidatesNeverConcludeAtPhase1End` | `togi_tuner::rounds_tests::zero_movable_candidates_never_conclude_at_phase1_end`<br>`togi_tuner::drain_tests::zero_movable_candidates_never_conclude_at_phase1_end` | A |
| `TestRoundFailureBlamesTheLoadedDeepenedCoreAndEveryOtherMoverYieldsAndIsFinished` | `togi_tuner::rounds_tests::a_blamed_round_failure_yields_every_unloaded_mover_then_backs_off_the_loaded_one` | A |
| `TestRoundThatEndsUnpassedWithoutBlameKeepsItsMoversForARecheckInPlace` | `togi_tuner::rounds_tests::a_round_ended_without_blame_keeps_its_movers_for_a_recheck_in_place` | A |
| `TestRoundFailureMovesNothingIntoARecordedFailurePoint` | `togi_tuner::rounds_tests::a_round_failure_moves_nothing_into_a_recorded_failure_point` | A |
| `TestConfirmationFailureRevertsDeepenedCoresTopRequesterFirst` | `togi_tuner::rounds_tests::confirmation_failure_reverts_deepened_cores_top_requester_first` | A |
| `TestConfirmationResumesAcrossRevertAndRerun` | `togi_tuner::rounds_tests::confirmation_resumes_across_revert_and_rerun` | A |
| `TestPhase2BlameInDrainMatchesNext` | `togi_tuner::drain_tests::phase2_blame_in_drain_matches_next` | A |
| `TestRoundFailureBlamesTopRequestGroupOfAnR7Load` | `togi_tuner::rounds_tests::a_failed_r7_round_check_blames_the_least_preferred_loaded_core`<br>`togi_tuner::rounds::blame::tests::a_multi_core_r7_failure_without_an_open_record_is_charged_by_depth_not_by_requests`<br>`togi_tuner::r7_escalation_tests::deepening_hold_and_named_backoffs_are_never_counted` | A |
| `TestYieldedMoverIsFinishedAndNeverMovesAgain` | `togi_tuner::rounds_tests::a_yielded_mover_is_finished_and_never_moves_again` | A |
| `TestConfirmationFailureRevertsTheDeepestMovedCoreFirst` | `togi_tuner::rounds_tests::confirmation_failure_reverts_the_deepest_moved_core_first` | A |
| `TestIdleFailureAfterTheMovedProfileIsAppliedBlamesTheDeepenedCores` | `togi_tuner::rounds_tests::an_idle_failure_after_the_moved_profile_is_applied_blames_the_deepened_cores` | A |
| `TestRoundBaselineOfAFreshMoverIsItsCurrentOffset` | `togi_tuner::rounds_tests::a_fresh_movers_round_baseline_is_its_current_offset` | A |
| `TestConfirmationFailureNamingAPhase1CoreRevertsOnlyTheDeepenedCore` | `togi_tuner::rounds_tests::a_confirmation_failure_naming_a_phase1_core_reverts_only_the_deepened_core` | A |

### `internal/tuner/property_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestSearchConverges` | `togi_tuner::prop_tests::search_converges` | A |
| `TestRandomOutcomesPreserveProfiles` | `togi_tuner::projection_tests::random_outcomes_preserve_profiles` | A |

### `internal/tuner/r7_status_test.go` (4)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestR7StatusSeparatesWorkloadsOffsetsAndTopRequesters` | `togi_tuner::r7_escalation_tests::r7_status_separates_workloads_offsets_and_top_requesters` | A |
| `TestR7StatusIgnoresFailedStartsAsTopRequester` | `togi_tuner::r7_escalation_tests::r7_status_ignores_failed_starts_as_top_requester` | A |
| `TestR7StatusUsesSortedProfileAndOnlyCCDParts` | `togi_tuner::r7_escalation_tests::r7_status_uses_sorted_profile_and_only_ccd_parts` | A |
| `TestR7StatusWithoutTopology` | `togi_tuner::r7_escalation_tests::r7_status_empty_without_topology` | A |

### `internal/tuner/r7_test.go` (33)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestR7FailuresMoveAfterLongCleanLedger` | `togi_tuner::r7_attribution_tests::r7_failures_move_after_long_clean_ledger` | A |
| `TestR7AttributionAndTieBackoff` | `togi_tuner::r7_attribution_tests::r7_attribution_and_tie_backoff` | A |
| `TestR7VoltageTargetClockFilter` | `togi_tuner::r7_target_tests::r7_voltage_target_clock_filter` | A |
| `TestR7TransitionRecordOnlyFailuresMoveBeforeTrial` | `togi_tuner::r7_attribution_tests::r7_carried_failures_move_before_any_trial` | A — record_only is gone; native carried failures keep the routing |
| `TestR7RequestOrderShiftsAndFallsBack` | `togi_tuner::r7_target_tests::r7_request_order_shifts_and_falls_back` | A |
| `TestR7LiveNamedAndMCEAttribution` | `togi_tuner::r7_attribution_tests::r7_live_named_and_mce_attribution` | A |
| `TestR7UnattributedVoltageTarget` | `togi_tuner::r7_target_tests::r7_unattributed_voltage_target` | A |
| `TestR7NamedTargetTransfersAcrossLoadsWithoutFailClock` | `togi_tuner::r7_target_tests::r7_named_target_transfers_across_loads_without_fail_clock` | A |
| `TestR7AllCoreTargetsUseEachCCD` | `togi_tuner::r7_attribution_tests::r7_all_core_targets_use_each_ccd` | A |
| `TestR7ZeroFailureDrainsWithoutReadingRanking` | `togi_tuner::r7_attribution_tests::r7_zero_failure_drains_without_reading_ranking` | A |
| `TestR7BackoffReason` | `togi_tuner::r7_attribution_tests::r7_backoff_reason` | A — structured reason fields, not reason strings |
| `TestR7ZeroTopStepsDownWithinCCD` | `togi_tuner::r7_attribution_tests::r7_zero_top_steps_down_within_ccd` | A |
| `TestR7ZeroTopUsesShiftedFullMeasurement` | `togi_tuner::r7_attribution_tests::r7_zero_top_uses_shifted_full_measurement` | A |
| `TestR7ZeroAffectedCCDDeadEndsAfterItsAllZeroRerun` | `togi_tuner::r7_attribution_tests::r7_zero_affected_ccd_dead_ends_after_its_all_zero_rerun` | A |
| `TestR7NamedZeroAttributionUsesStartTop` | `togi_tuner::r7_attribution_tests::r7_named_zero_attribution_uses_start_top` | A |
| `TestR7StepDownTargetExceedsOriginalTop` | `togi_tuner::r7_target_tests::r7_step_down_target_exceeds_original_top` | A |
| `TestR7FailureUsesRequestsAsOfItsStart` | `togi_tuner::r7_target_tests::r7_failure_uses_requests_as_of_its_start` | A |
| `TestR7VoltageTargetCitesItsLowestPass` | `togi_tuner::r7_target_tests::r7_voltage_target_cites_its_lowest_pass` | A |
| `TestR7BackoffCountsEachCarriedFactOnce` | `togi_tuner::r7_target_tests::r7_backoff_counts_each_carried_fact_once` | A |
| `TestR7AllCoreFailureSkipsCCDAtZero` | `togi_tuner::r7_attribution_tests::r7_all_core_failure_skips_ccd_at_zero` | A |
| `TestR7OffsetStandInMovesOneCount` | `togi_tuner::r7_target_tests::r7_offset_stand_in_moves_one_count` | A |
| `TestR7FailureNeedsNoMoveWhileMovedCoreIsShallower` | `togi_tuner::r7_attribution_tests::r7_failure_needs_no_move_while_moved_core_is_shallower`<br>`togi_tuner::r7_attribution_tests::r7_failure_needs_no_move_during_a_deepening_round_while_the_named_core_is_shallower` | A |
| `TestR7NamedIdleZeroCoreCountsAgainstLoadedCCD` | `togi_tuner::r7_attribution_tests::r7_named_idle_zero_core_counts_against_loaded_ccd`<br>`togi_tuner::r7_attribution_tests::r7_named_idle_zero_core_on_loaded_ccd_at_zero_waits_to_be_located` | A |
| `TestR7EscalatedFailureLocatesWithIdleCoresAtZero` | `togi_tuner::hunt::locate_tests::r7_escalated_failure_locates_with_idle_cores_at_zero` | A |
| `TestR7PassedLocateHuntsTheUnloadedCulprit` | `togi_tuner::hunt::locate_tests::r7_passed_locate_hunts_the_unloaded_culprit` | A |
| `TestR7FailedLocateKeepsTheVoltageTargetedBackoff` | `togi_tuner::hunt::locate_tests::r7_failed_locate_keeps_the_voltage_targeted_backoff` | A |
| `TestR7LocatedGroupFailureNamingACore` | `togi_tuner::hunt::locate_tests::r7_located_group_failure_naming_a_core` | A |
| `TestR7ZeroLoadedCCDDeadEndsOnlyAfterFailedLocate` | `togi_tuner::hunt::locate_tests::r7_zero_loaded_ccd_dead_ends_only_after_failed_locate` | A |
| `TestR7LocatedFailureNamingALoadedCoreChargesThatCore` | `togi_tuner::hunt::locate_tests::r7_located_failure_naming_a_loaded_core_charges_that_core` | A |
| `TestR7PassedAllZeroLocateNeverChargesLoadedCoresAtZero` | `togi_tuner::hunt::locate_tests::r7_passed_all_zero_locate_never_charges_loaded_cores_at_zero` | A |
| `TestR7LocateFailureNamingALoadedCoreAtZero` | `togi_tuner::hunt::locate_tests::r7_locate_failure_naming_a_loaded_core_at_zero` | A |
| `TestR7NamedUnloadedCoreOffZeroIsNotLocated` | `togi_tuner::r7_attribution_tests::r7_named_unloaded_core_off_zero_is_not_located` | A |
| `TestR7PassedZeroRerunLocatesCitingTheRerun` | `togi_tuner::hunt::locate_tests::r7_passed_zero_rerun_locates_citing_the_rerun` | A |

### `internal/tuner/requirement_sim_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestStoredRequirementMatchesScheduledSimulatedTrials` | — | S4(simulated scheduled trials, stage 4 simulator) |

### `internal/tuner/requirement_test.go` (7)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestStoredRequirementSurvivesCrashAndChangedConfig` | `togi_tuner::ledger_tests::stored_requirement_survives_crash_and_changed_config`<br>`togi_tuner::forecast_tests::stored_requirement_survives_crash_and_changed_config_forecast`<br>`togi_tuner::dashboard_tests::stored_requirement_survives_crash_and_changed_config` | A |
| `TestStoredRequirementSeparatesClassCountAndPart` | `togi_tuner::rerun_tests::stored_requirement_separates_class_count_and_part`<br>`togi_tuner::dashboard_tests::stored_requirement_separates_class_count_and_part` | A |
| `TestRetryRequirementMatchesFold` | `togi_tuner::ledger_tests::retry_requirement_matches_fold`<br>`togi_tuner::ledger_tests::a_search_trial_needs_one_pass_and_a_check_needs_n`<br>`togi_tuner::rerun_tests::cycle_retries_attach_the_requirement_the_fold_stores` | A |
| `TestZeroRerunRequirementIsOneTrial` | `togi_tuner::zero_tests::zero_rerun_requirement_is_one_trial` | A |
| `TestRequirementChecksHuntAndRoundIdentity` | `togi_tuner::ledger_tests::intents_that_match_no_open_context_are_unscheduled`<br>`togi_tuner::hunt::locate_tests::requirement_checks_hunt_identity`<br>`togi_tuner::rounds_tests::a_round_intent_is_scheduled_only_for_the_open_rounds_own_checks` | A |
| `TestRequirementTrialNumberIsBounded` | `togi_tuner::ledger_tests::requirement_trial_number_is_bounded` | A |
| `TestCrashRecoveryKeepsTheRunningPart` | `togi_tuner::rule_tests::a_crash_keeps_the_trial_in_flight_until_its_end`<br>`togi_tuner::dashboard_tests::crash_recovery_keeps_the_running_part` | A |

### `internal/tuner/rerun_carried_replay_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestCarriedRerunCitationReplay` | `togi_tuner::rerun_tests::carried_rerun_citation_replay`<br>`togi_tuner::rerun_tests::carried_rerun_citation_replay_ends_a_deepening_round` | A |
| `TestCarriedRerunBoundaryPreservesPendingChecks` | `togi_tuner::rerun_tests::a_carried_rerun_boundary_preserves_the_pending_checks` | A |

### `internal/tuner/reset_test.go` (3)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestResetClearsCombinationAndReevaluatesOtherCores` | `togi_tuner::phase_tests::reset_clears_combination_and_reevaluates_other_cores` | A |
| `TestResetAfterHuntEndDropsCommitment` | `togi_tuner::hunt::locate_tests::reset_after_hunt_end_drops_commitment` | A |
| `TestResetClosesRoundAndCycleBeforeSearching` | `togi_tuner::rounds_tests::a_reset_closes_the_open_round_or_cycle_before_the_core_searches` | A |

### `internal/tuner/retry_context_test.go` (8)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestCancelledRoundCheckRetryDoesNotRun` | `togi_tuner::rounds_tests::a_cancelled_round_check_retry_does_not_run_after_its_round_ends` | A |
| `TestAllZeroRerunRetryBelongsToItsFailure` | `togi_tuner::zero_tests::all_zero_rerun_retry_belongs_to_its_failure` | A |
| `TestRerunRetryBelongsToItsObligation` | `togi_tuner::rerun_tests::rerun_retry_belongs_to_its_obligation` | A |
| `TestResumedRequirementChangeDropsRetry` | `togi_tuner::ledger_tests::resumed_requirement_change_drops_retry`<br>`togi_tuner::rerun_tests::resumed_short_trial_change_drops_a_rerun_retry` | A |
| `TestCycleRetryEndsWithItsCycle` | `togi_tuner::rerun_tests::cycle_retry_ends_with_its_cycle` | A |
| `TestSearchRetryBelongsToItsTurn` | `togi_tuner::ledger_tests::search_retry_belongs_to_its_turn` | A |
| `TestHuntGroupRetryBelongsToItsGroup` | `togi_tuner::hunt::locate_tests::hunt_group_retry_belongs_to_its_group` | A |
| `TestHuntRetryStaysWithItsGroup` | `togi_tuner::hunt::locate_tests::hunt_retry_stays_with_its_group` | A |

### `internal/tuner/rules_test.go` (13)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestConstraints` | `togi_tuner::tests::constraints_reach_and_at_limit`<br>`togi_tuner::tests::constraints_check_failure_points_before_combinations_in_core_order` | A — the `SoleNonzero` index cases are gone: `sole_nonzero` returns a `CoreId` |
| `TestTogetherCrashNamesTheSoleNonzeroCoreByID` | `togi_tuner::tests::together_crash_names_the_sole_nonzero_core_by_id`<br>`togi_tuner::rule_tests::together_failures_name_the_core_the_end_or_one_cited_mce_names`<br>`togi_tuner::checking_tests::together_crash_names_sole_nonzero_core_by_id` | A |
| `TestOptimum` | `togi_tuner::tests::optimum`<br>`togi_tuner::tests::optimum_with_staircase_combinations`<br>`togi_tuner::tests::four_core_cycle_needs_global_yield`<br>`togi_tuner::tests::optimum_keeps_failure_point_outside_safe_bounds` | A |
| `TestOptimumWithStaircaseCombinations` | `togi_tuner::tests::optimum_with_staircase_combinations`<br>`togi_tuner::tests::four_core_cycle_needs_global_yield`<br>`togi_tuner::tests::optimum_keeps_failure_point_outside_safe_bounds` | A |
| `TestOptimumMatchesExhaustiveSearch` | `togi_tuner::tests::optimum_matches_exhaustive_search` | A |
| `TestOptimumWithAccumulatedCombinations` | `togi_tuner::optimum_tests::optimum_with_accumulated_combinations` | A — a bound on search nodes replaces wall time |
| `TestFourCoreCycleNeedsGlobalYield` | `togi_tuner::tests::optimum_with_staircase_combinations`<br>`togi_tuner::tests::four_core_cycle_needs_global_yield`<br>`togi_tuner::tests::optimum_keeps_failure_point_outside_safe_bounds` | A |
| `TestClassifyCrash` | `togi_tuner::tests::classify_crash_cases`<br>`togi_tuner::tests::other_reset_kinds_classify_by_what_was_applied` | A |
| `TestDirectFailureAtZero` | `togi_tuner::ledger_tests::a_carried_all_zero_idle_failure_stops_before_any_trial`<br>`togi_tuner::zero_tests::direct_failure_at_zero`<br>`togi_tuner::hunt::locate_tests::direct_failure_at_zero_in_a_hunt_group` | A |
| `TestThermalReasonYieldsToMCEEvidence` | `togi_tuner::tests::thermal_reason_yields_to_mce_evidence`<br>`togi_tuner::rule_tests::thermal_reason_yields_to_mce_evidence` | A |
| `TestOptimumKeepsFailurePointOutsideSafeBounds` | `togi_tuner::tests::optimum_with_staircase_combinations`<br>`togi_tuner::tests::four_core_cycle_needs_global_yield`<br>`togi_tuner::tests::optimum_keeps_failure_point_outside_safe_bounds` | A |
| `TestCarriedZeroFailurePointStopsBeforeTrial` | `togi_tuner::rule_tests::persistent_zero_point_stops_before_trial`<br>`togi_tuner::rule_tests::an_alone_failure_at_zero_is_a_dead_end` | A |
| `TestClassTargetsPreserveHuntAndRerunLookup` | `togi_tuner::rule_tests::class_key_distinguishes_single_full_ccd_loads`<br>`togi_tuner::hunt::locate_tests::class_targets_preserve_hunt_and_rerun_lookup` | A |

### `internal/tuner/schedule_test.go` (6)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestSearchAndSoloLimitCheck` | `togi_tuner::schedule_tests::search_and_solo_limit_check` | A |
| `TestSearchFailureAndRetry` | `togi_tuner::schedule_tests::search_failure_and_retry` | A |
| `TestDecisionSupersedesAloneRetry` | `togi_tuner::schedule_tests::decision_supersedes_alone_retry` | A |
| `TestSearchSkipsCoresWithRoom` | `togi_tuner::schedule_tests::search_skips_cores_with_room` | A |
| `TestSeededCandidateSoloLimitFreezesWorkloads` | `togi_tuner::schedule_tests::seeded_candidate_solo_limit_freezes_workloads` | A |
| `TestSeededCandidateSoloLimitAdvancesNextWorkloadPair` | `togi_tuner::schedule_tests::seeded_candidate_solo_limit_advances_next_workload_pair` | A |

### `internal/tuner/skip_test.go` (6)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestSkipKnownFailure` | `togi_tuner::skip_tests::skip_known_failure`<br>`togi_tuner::skip_tests::a_live_alone_failure_skips_citing_its_trial_end` | A |
| `TestKnownSingleCoreR7FailureMatchesWorkload` | `togi_tuner::skip_tests::a_known_single_core_r7_failure_matches_its_workload`<br>`togi_tuner::skip_tests::a_multi_core_r7_trial_is_never_skipped` | A |
| `TestSkippedTogetherFailureMakesProgress` | `togi_tuner::hunt::locate_tests::skipped_together_failure_makes_progress` | A |
| `TestSkippedDeepeningAndRerunFailures` | `togi_tuner::rerun_tests::skipped_deepening_and_rerun_failures`<br>`togi_tuner::rounds_tests::a_skipped_deepening_check_closes_the_round_before_its_attributed_backoff` | A |
| `TestKnownAloneFailureUsesRecordedOffset` | `togi_tuner::skip_tests::a_known_alone_failure_uses_the_recorded_offset`<br>`togi_tuner::ledger_tests::a_known_alone_failure_backs_off_from_its_recorded_offset` | A |
| `TestKnownTogetherFailureInfersSoleNonzeroCore` | `togi_tuner::skip_tests::a_known_together_failure_infers_the_sole_nonzero_core`<br>`togi_tuner::skip_tests::a_known_together_failure_backs_the_sole_nonzero_core_off_its_recorded_combination` | A |

### `internal/tuner/zero_carry_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestCarriedUnconfirmedFailureAtZeroIsHunted` | `togi_tuner::ledger_tests::a_carried_all_zero_idle_failure_stops_before_any_trial`<br>`togi_tuner::zero_tests::carried_unconfirmed_failure_at_zero_is_hunted`<br>`togi_carry::tests::zero::only_confirmed_zero_failure_points`<br>`togi_carry::tests::zero::zero_proof_persisted_in_seed` | A |
| `TestCarriedFailureAtZeroWithAllZeroProfileDeadEnds` | `togi_tuner::ledger_tests::a_carried_all_zero_idle_failure_stops_before_any_trial`<br>`togi_tuner::zero_tests::carried_failure_at_zero_with_all_zero_profile_dead_ends`<br>`togi_carry::tests::zero::only_confirmed_zero_failure_points`<br>`togi_carry::tests::zero::zero_proof_persisted_in_seed` | A |

### `internal/tuner/zero_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestKnownFailureSharingAZeroRerunTrialIsRouted` | `togi_tuner::zero_tests::known_failure_sharing_a_zero_rerun_trial_is_routed` | A |
| `TestSingleCoreZeroRerunKeepsItsShape` | `togi_tuner::zero_tests::single_core_zero_rerun_keeps_its_shape` | A |
