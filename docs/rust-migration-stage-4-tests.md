# Stage 4 Go-test mapping

Every `func Test*`, `Benchmark*`, `Fuzz*` and `Example*` in the Go packages ported in stage 4 (#639) is mapped to the Rust tests that pin the same behavior: `internal/tuningboot` → `togi-tuningboot`, `internal/sim` → `togi-sim`, `tools/trialfacts` → `togi-tool-trialfacts`, `internal/session` → `togi-session`, `internal/simrun` → `togi-simrun`, `tools/audit` → `togi-tool-audit` and `tools/sim` → `togi-tool-sim`, with `forecast_sim_test.go` and `memo_test.go` of `internal/tuner` (#741), which `togi-simrun` pins. There are no Fuzz or Example functions and one Benchmark.

Rust names are `crate::module::fn`; an integration test binary adds its name after the crate (`togi_sim::suite::…`), and the tests inside the `togi-tool-sim` binary add `bin`. Each name resolves to a test that `cargo nextest list` reports on `main` at 97eba4cc with this pull request's three tests added (`togi_session::boot::tests::a_resumed_dead_end_acknowledges_the_pending_tuning_boot_reason_once_and_leaves_its_own`, `togi_session::boot::tests::a_resumed_dead_end_whose_saved_entry_append_fails_keeps_the_acknowledgement_and_its_own_reason` and `togi_sim::suite::trials::a_replayed_crash_resets_by_watchdog_whatever_the_models_reset_mix`); a parameterised `#[test_case]` function counts by its name. The pull requests that ported each package are #748 (tuningboot), #773 (sim), #790 (trialfacts), #810 (session), #812 (simrun) and #813 (audit, tool sim).

Status:

- **A**: a test of the port's own crate pins the behavior.
- **X**: a test of another Rust crate pins it.
- **O(reason)**: obsolete under the native contract.
- **G(reason)**: not pinned by any Rust test and not obsolete.

## Summary

| Package | Go tests | A | X | O | G |
|---|---:|---:|---:|---:|---:|
| tuningboot | 8 | 8 | 0 | 0 | 0 |
| sim | 107 | 100 | 2 | 5 | 0 |
| trialfacts | 9 | 9 | 0 | 0 | 0 |
| session | 165 | 152 | 9 | 4 | 0 |
| simrun and the two tuner files | 46 | 45 | 0 | 0 | 1 |
| audit | 13 | 6 | 0 | 1 | 6 |
| tool sim | 10 | 9 | 0 | 1 | 0 |
| **total** | **358** | **329** | **11** | **11** | **7** |

## tuningboot

### `internal/tuningboot/tuningboot_test.go` (8)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestRestartLimitTwoRetriesThenNormalBoot` | `togi_tuningboot::tests::two_retries_then_exhaustion_clears_saved_entry_and_stays_at_three`<br>`togi_tuningboot::tests::stored_counts_map_to_the_next_count`<br>`togi_tuningboot::tests::a_retry_never_clears_the_saved_entry`<br>`togi_tuningboot::tests::restart_limit_works_through_a_trait_object` | A |
| `TestRestartLimitRejectsInvalidCountWithoutRetry` | `togi_tuningboot::tests::invalid_counts_grant_no_retry_and_write_nothing` | A |
| `TestRestartLimitFailuresDoNotGrantRetry` | `togi_tuningboot::tests::a_failure_at_each_step_grants_no_retry_and_leaves_only_what_happened`<br>`togi_tuningboot::tests::a_random_failure_grants_no_retry_and_touches_nothing` | A |
| `TestReasonRoundTripAndIndependentCleanup` | `togi_tuningboot::tests::reason_round_trips_through_the_environment_and_cleans_up_independently` | A |
| `TestReasonSanitizesAndBoundsWrites` | `togi_tuningboot::tests::sanitize_replaces_cuts_and_trims`<br>`togi_tuningboot::tests::encoded_records_fit_and_round_trip_for_hostile_reasons`<br>`togi_tuningboot::tests::an_escaped_reason_is_cut_to_the_budget`<br>`togi_tuningboot::tests::quotes_and_backslashes_in_a_reason_are_escaped_and_decode_back`<br>`togi_tuningboot::tests::new_records_encode_within_bounds_and_decode`<br>`togi_tuningboot::tests::escape_heavy_records_encode_within_bounds_and_decode` | A |
| `TestReadReasonRejectsInvalidRecords` | `togi_tuningboot::tests::decoding_rejects_invalid_records`<br>`togi_tuningboot::tests::decoding_accepts_a_well_formed_record` | A |
| `TestWriteReasonRejectsInvalidMetadata` | `togi_tuningboot::tests::constructors_refuse_invalid_metadata`<br>`togi_tuningboot::tests::restart_count_next_saturates_at_three` | A |
| `TestReasonOperationFailuresPreserveState` | `togi_tuningboot::tests::failed_operations_preserve_state`<br>`togi_tuningboot::tests::error_display_names_the_operation_without_the_source_text` | A |

## sim

### `internal/sim/ccd_test.go` (3)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestCCDLoadedHazardAndDraws` | `togi_sim::suite::ccd::only_loaded_ccds_in_r7_contribute_by_their_mean_depth`<br>`togi_sim::suite::ccd::the_effect_and_the_slope_shape_each_ccd`<br>`togi_sim::suite::draws::ccd_failures_follow_the_integrated_probability` | A |
| `TestCCDValidation` | `togi_sim::suite::validation::ccd_parameters_are_finite_with_a_nonnegative_slope` | A |
| `TestCCDResidualDefersToExistingJoint` | `togi_sim::suite::ccd::an_active_joint_on_a_ccd_replaces_its_residual_even_before_the_joint_starts` | A |

### `internal/sim/clone_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestCloneSharesNothingWithItsSource` | `togi_sim::suite::hazard::a_model_owns_its_data_and_clones_share_it`<br>`togi_sim::suite::definition::a_model_keeps_what_it_was_built_from_by_value` | A |

### `internal/sim/config_test.go` (13)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestLoadMachine` | `togi_sim::suite::definition::a_full_machine_decodes_with_its_metadata`<br>`togi_sim::suite::definition::malformed_documents_are_refused_with_the_file_named` | A |
| `TestLoadMachineInvalidDefinitions` | `togi_sim::suite::definition::malformed_documents_are_refused_with_the_file_named`<br>`togi_sim::suite::validation::the_core_table_names_every_core_exactly_once`<br>`togi_sim::suite::validation::scripts_name_a_trial_once_and_a_core_of_the_machine` | A |
| `TestLoadMachineEmptyObjectUsesDefaults` | `togi_sim::suite::definition::an_empty_object_is_the_default_machine` | A |
| `TestLoadMachineRejectsInvalidCoreCountBeforeLimits` | `togi_sim::suite::validation::a_loaded_document_is_refused_for_its_topology_before_its_limits`<br>`togi_sim::suite::validation::topology_is_checked_before_the_limits` | A |
| `TestValidateCoresBound` | `togi_sim::suite::validation::the_core_count_is_even_from_two_to_sixteen` | A |
| `TestLoadMachineBIOSContext` | `togi_sim::suite::definition::the_default_bios_context_fills_what_a_file_leaves_out` | A |
| `TestNewRejectsInvalidScriptCore` | `togi_sim::suite::validation::scripts_name_a_trial_once_and_a_core_of_the_machine` | A |
| `TestNewKeepsItsOwnConfig` | `togi_sim::suite::definition::a_model_keeps_what_it_was_built_from_by_value` | A |
| `TestNewKeepsItsOwnConfigReplay` | — | O(a `Replay` moves into `RunSettings` by value; the caller cannot alias it) |
| `TestScriptTrialAfterNew` | `togi_sim::suite::trials::a_trial_can_be_scripted_after_the_machine_is_built` | A |
| `TestNewRejectsInvalidModelWeights` | `togi_sim::suite::validation::weights_are_finite_nonnegative_with_a_positive_total`<br>`togi_sim::suite::validation::reset_weights_are_finite_nonnegative_with_a_positive_total`<br>`togi_sim::suite::definition::malformed_documents_are_refused_with_the_file_named` | A |
| `TestNewRejectsInvalidHazardsAndSignals` | `togi_sim::suite::validation::flat_rates_are_finite_and_nonnegative`<br>`togi_sim::suite::validation::joints_need_members_on_real_cores_and_valid_numbers`<br>`togi_sim::suite::validation::weights_are_finite_nonnegative_with_a_positive_total`<br>`togi_sim::suite::validation::only_signals_a_simulated_failure_can_draw_are_supported`<br>`togi_sim::suite::definition::malformed_documents_are_refused_with_the_file_named` | A |
| `TestNewRejectsInvalidModelTopology` | `togi_sim::suite::validation::bios_and_ranking_have_one_value_per_core`<br>`togi_sim::suite::validation::limits_idle_and_workload_limits_stay_within_minus_fifty_and_one`<br>`togi_sim::suite::validation::joints_need_members_on_real_cores_and_valid_numbers` | A |

### `internal/sim/encode_test.go` (6)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestEncodeMachineRoundTripsEveryField` | `togi_sim::suite::encode::every_field_round_trips`<br>`togi_sim::suite::encode::every_valid_definition_round_trips`<br>`togi_sim::suite::encode::every_committed_machine_round_trips_through_the_encoder` | A |
| `TestEncodeMachineLeavesDefaultModelFieldsOut` | `togi_sim::suite::encode::fields_equal_to_their_default_are_left_out_and_the_rest_are_written`<br>`togi_sim::suite::encode::the_default_machine_is_an_empty_object` | A |
| `TestEncodeMachineRefusesRuntimeOnlyFields` | — | O(run-only settings are `RunSettings`, a separate type; a `MachineDefinition` cannot hold them) |
| `TestEncodeMachineRoundTripsControlCharacterStrings` | `togi_sim::suite::encode::awkward_strings_round_trip` | A |
| `TestEncodeMachineRoundTripsLargeWholeFloats` | `togi_sim::suite::encode::large_and_small_finite_floats_keep_their_value` | A |
| `TestEncodeMachineRejectsNonFiniteFloats` | `togi_sim::suite::encode::an_invalid_machine_is_refused_before_anything_is_written` | A |

### `internal/sim/hazard_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestHazard` | `togi_sim::suite::hazard::steady_hazard_follows_the_limits_of_the_applied_registers`<br>`togi_sim::suite::hazard::r6_and_r7_use_the_together_limit_even_when_alone`<br>`togi_sim::suite::hazard::a_limit_of_one_fails_at_offset_zero` | A |

### `internal/sim/nonmember_mce_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestJointCrashNonmemberMCE` | `togi_sim::suite::trials::a_misleading_machine_check_core_can_be_a_nonmember_that_is_not_loaded` | A |

### `internal/sim/overlay_test.go` (9)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestOverlayDecodesToParentPlusNamedFields` | `togi_sim::suite::overlay::an_overlay_is_its_parent_plus_the_named_fields` | A |
| `TestOverlayFollowsParentChanges` | `togi_sim::suite::overlay::an_overlay_follows_its_parent` | A |
| `TestOverlayChainLoadsLeafFirstAndLastOverrideWins` | `togi_sim::suite::overlay::a_chain_loads_leaf_first_and_the_last_override_wins` | A |
| `TestOverlayParentLookupIsRelativeToTheOverlay` | `togi_sim::suite::overlay::the_parent_is_looked_up_relative_to_the_overlay` | A |
| `TestOverlayRebasesInheritedFacts` | `togi_sim::suite::overlay::inherited_facts_resolve_to_the_same_extract_from_the_overlay`<br>`togi_sim::suite::overlay::inherited_facts_through_a_symlinked_directory_still_name_the_same_extract`<br>`togi_sim::file::tests::rebase_climbs_past_the_shared_directories_and_keeps_the_rest_verbatim` | A |
| `TestOverlayKeepsFactsAbsent` | `togi_sim::suite::overlay::an_overlay_of_a_parent_without_facts_has_none` | A |
| `TestLoadMachineWithFilesFullMachine` | `togi_sim::suite::overlay::a_full_machine_lists_only_itself_and_a_missing_one_is_an_error` | A |
| `TestOverlayRejectsCycles` | `togi_sim::suite::overlay::a_cycle_is_an_error_however_it_is_spelled` | A |
| `TestOverlayRejectsMalformedDocuments` | `togi_sim::suite::overlay::malformed_overlays_are_refused_with_the_file_named` | A |

### `internal/sim/predictor_test.go` (3)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestPredictorMatchesMachine` | `togi_sim::suite::hazard::sources_sum_to_the_steady_hazard_and_scale_to_the_probability`<br>`togi_sim::suite::hazard::the_model_is_the_predictor_with_no_limits_required`<br>`togi_sim::suite::draws::a_loaded_cores_flat_rate_matches_its_probability` | A |
| `TestPredictorOwnsItsConfig` | `togi_sim::suite::hazard::a_model_owns_its_data_and_clones_share_it` | A |
| `TestNewPredictorRejects` | `togi_sim::suite::validation::joints_need_members_on_real_cores_and_valid_numbers`<br>`togi_sim::suite::hazard::the_model_is_the_predictor_with_no_limits_required` | A |

### `internal/sim/probability_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestFailureProbability` | `togi_sim::suite::probability::probability_integrates_loaded_idle_and_delayed_joint_exposure`<br>`togi_sim::suite::probability::exposure_counts_boosted_seconds_once_more`<br>`togi_sim::suite::validation::model_scalars_are_checked` | A |

### `internal/sim/regime_signals_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestRegimeSignalsReplaceModelSignalsInTheirRegime` | `togi_sim::suite::trials::a_regime_mix_replaces_the_pooled_mix_in_its_regime` | A |
| `TestRegimeSignalsDrawUnattributedR7Failures` | `togi_sim::suite::trials::unattributed_r7_failures_draw_the_regime_mix_when_there_is_one` | A |

### `internal/sim/replay_test.go` (6)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestReplayDraws` | `togi_sim::suite::replay::a_match_is_drawn_among_the_facts_of_its_context_and_full_profile`<br>`togi_sim::suite::replay::the_choice_among_matches_is_uniform`<br>`togi_sim::suite::replay::the_choice_does_not_depend_on_the_index_hash_order` | A |
| `TestReplayFallback` | `togi_sim::suite::replay::any_difference_in_the_key_leaves_the_trial_to_the_fitted_model`<br>`togi_sim::suite::replay::an_offset_of_an_unloaded_core_is_part_of_the_key`<br>`togi_sim::suite::replay::a_replay_miss_runs_the_fitted_failure_model_exactly_as_if_there_were_no_replay`<br>`togi_sim::suite::trials::a_trial_without_a_matching_fact_falls_back_to_the_fitted_model` | A |
| `TestReplayCrashTime` | `togi_sim::suite::trials::a_replayed_crash_or_uncorrected_machine_check_happens_at_its_recorded_exposure` | A |
| `TestReplayRequiresDeclaredContext` | `togi_sim::suite::replay::facts_with_invalid_evidence_are_refused_with_their_index` | A |
| `TestReplayCorrectedMCETime` | `togi_sim::suite::trials::a_replayed_corrected_machine_check_ends_the_trial_at_its_recorded_exposure` | A |
| `TestReplayRejectsInvalidEvidence` | `togi_sim::suite::replay::facts_with_invalid_evidence_are_refused_with_their_index` | A |

### `internal/sim/resume_test.go` (6)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestResumeAfterIncompatibleArchive` | — | O(an archive of another schema with unknown kinds; Rust reads only its own journals) |
| `TestResumeContextPrecedence` | `togi_sim::suite::runtime::the_simulator_continues_after_its_history`<br>`togi_journal::reader::tests::recordings_read_the_archives_oldest_first_then_the_live_journal`<br>`togi_journal::reader::tests::recordings_order_archives_by_session_not_by_name` | A |
| `TestResumeEmptyAndCorruptHistory` | `togi_sim::suite::seams::a_machine_starts_at_the_epoch_or_one_reboot_after_its_history`<br>`togi_journal::reader::tests::an_undecodable_archive_after_a_readable_one_is_an_error_naming_its_session`<br>`togi_journal::reader::tests::recordings_fail_on_an_unreadable_journal_and_ignore_a_torn_tail` | A |
| `TestResumeIgnoresTornIncompatibleTail` | `togi_journal::reader::tests::recordings_fail_on_an_unreadable_journal_and_ignore_a_torn_tail` | X |
| `TestResumeReportsUnlistableArchive` | `togi_journal::journal::tests::an_archive_that_is_not_a_directory_fails_the_lookup` | X |
| `TestResumeReadsArchivesOfPatternLikeStateDir` | — | O(Go globbed the state-directory path; Rust lists the archive directory, so a pattern-like name is plain) |

### `internal/sim/samples_integration_linux_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestSamplePersistenceFailuresRemainErrors` | `togi_sim::suite::samples::a_sample_file_that_refuses_writes_is_an_error_with_the_trial_result`<br>`togi_sim::suite::samples::a_sample_store_that_cannot_be_written_is_an_error_with_the_trial_result` | A |

### `internal/sim/samples_test.go` (4)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestCrashWorkerSamplesSurviveRecovery` | `togi_sim::suite::samples::a_crashed_trials_samples_survive_recovery_in_files` | A |
| `TestSimulatedSamples` | `togi_sim::suite::samples::samples_tick_each_second_strictly_before_the_end_and_a_stalled_culprit_stops_early`<br>`togi_sim::suite::samples::ending_a_trial_compresses_its_samples_without_changing_them` | A |
| `TestInMemorySampleRetentionDoesNotGrowWithDuration` | `togi_sim::suite::samples::a_long_trial_keeps_no_series_and_yields_lazily` | A |
| `TestSampleSetupFailuresRemainErrors` | `togi_sim::suite::samples::a_sample_store_that_cannot_be_written_is_an_error_with_the_trial_result` | A |

### `internal/sim/seams_test.go` (11)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestFaultsAndRanking` | `togi_sim::suite::seams::ranking_is_unavailable_unless_the_machine_declares_one`<br>`togi_sim::suite::seams::write_faults_start_counting_when_armed`<br>`togi_sim::suite::seams::a_missing_backend_fails_every_start_of_its_workloads`<br>`togi_sim::suite::seams::an_old_kernel_reports_no_reset_reason_whatever_the_reset` | A |
| `TestHostValidationAndWatchdogFaults` | `togi_sim::suite::seams::only_identity_checks_block_smu_validation_and_the_watchdog_fails_separately` | A |
| `TestInvalidSMUCoreDoesNotWrite` | `togi_sim::suite::seams::an_smu_access_to_a_missing_core_changes_nothing` | A |
| `TestCanceledStartDoesNotConsumeFault` | `togi_sim::suite::seams::a_cancelled_start_does_not_consume_a_setup_fault_or_move_the_clock` | A |
| `TestStopMatchesCanceledWaitCleanup` | `togi_sim::suite::seams::stopping_a_trial_matches_what_a_cancelled_wait_would_clean_up` | A |
| `TestCrashedSeamsRejectOperations` | `togi_sim::suite::seams::a_crashed_machine_rejects_its_operations_and_does_not_move_its_clock` | A |
| `TestTopologyAndConfiguredClock` | `togi_sim::suite::seams::the_topology_splits_the_cores_into_two_ccds_with_their_sibling_threads`<br>`togi_sim::suite::seams::a_machine_starts_at_the_epoch_or_one_reboot_after_its_history`<br>`togi_sim::suite::seams::the_clock_seam_follows_sleeps_wall_jumps_and_reboots_from_a_fresh_or_resumed_start` | A |
| `TestKernelCursorAcrossEmptyBoundariesAndReboot` | `togi_sim::suite::seams::a_kernel_cursor_resumes_across_empty_reads_and_stays_in_its_boot` | A |
| `TestKernelMissingBootsAndCursors` | `togi_sim::suite::seams::unknown_boots_and_cursors_are_named_errors` | A |
| `TestOldKernelDoesNotReportResetKind` | `togi_sim::suite::seams::an_old_kernel_reports_no_reset_reason_whatever_the_reset` | A |
| `TestResetReasonKinds` | `togi_sim::suite::seams::each_reset_leaves_the_line_a_recent_kernel_logs`<br>`togi_sim::suite::seams::a_boot_without_a_reset_reports_no_kind_and_a_hold_of_the_power_button_is_its_own` | A |

### `internal/sim/shared_voltage_test.go` (17)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestSharedVoltageTopRequester` | `togi_sim::suite::shared_voltage::the_rail_follows_the_top_loaded_requester_and_idle_cores_never_set_it`<br>`togi_sim::suite::shared_voltage::the_rail_is_the_highest_loaded_request_even_when_every_request_is_negative` | A |
| `TestSharedVoltageClockShape` | `togi_sim::suite::shared_voltage::clocks_follow_package_power_idle_gain_and_cross_ccd_balance` | A |
| `TestSharedVoltageCore11NeedsSupport` | `togi_sim::suite::shared_voltage::core_eleven_needs_a_supporting_core_to_keep_its_rail`<br>`togi_sim::suite::shared_voltage::a_rail_failure_of_core_eleven_takes_the_signal_of_its_own_mix` | A |
| `TestSharedVoltageDrawsAndScores` | `togi_sim::suite::shared_voltage::the_rail_replaces_loaded_core_hazards_joints_and_ccd_for_multi_core_r7_only`<br>`togi_sim::suite::shared_voltage::unloaded_core_hazards_survive_the_rail_and_sources_keep_their_order`<br>`togi_sim::suite::hazard::shared_voltage_changes_nothing_for_trials_that_do_not_load_two_cores_in_r7`<br>`togi_sim::suite::draws::the_shared_rail_fails_as_its_probability_says_and_names_no_machine_check` | A |
| `TestSharedVoltageSamplesAndDeterminism` | `togi_sim::suite::samples::a_shared_voltage_machine_samples_its_lanes_the_same_in_memory_and_in_files`<br>`togi_sim::suite::shared_voltage::r7_requests_exist_for_multi_core_r7_and_telemetry_for_any_trial_on_a_rail`<br>`togi_sim::suite::draws::the_same_seed_draws_the_same_failures_and_telemetry_changes_none` | A |
| `TestSharedVoltageValidation` | `togi_sim::suite::shared_voltage::the_rail_needs_sixteen_cores_the_three_r7_workloads_and_sixteen_core_entries`<br>`togi_sim::suite::shared_voltage::rail_numbers_are_finite_and_in_range`<br>`togi_sim::suite::shared_voltage::rail_fields_are_checked_in_document_order` | A |
| `TestSharedVoltageSamplesStayLazy` | `togi_sim::suite::samples::a_long_trial_keeps_no_series_and_yields_lazily` | A |
| `TestSharedVoltageThresholdClock` | `togi_sim::suite::shared_voltage::equal_request_and_requirement_slopes_cancel_a_common_clock_shift` | A |
| `TestSharedVoltageDefaultThresholdClock` | `togi_sim::suite::shared_voltage::an_omitted_requirement_coefficient_keeps_the_clock_independent_threshold` | A |
| `TestR7FailureShare` | `togi_sim::suite::shared_voltage::failure_share_is_a_loaded_cores_rail_rate_over_the_total`<br>`togi_sim::suite::shared_voltage::the_share_of_two_rates_whose_sum_overflows_is_still_their_ratio` | A |
| `TestSharedVoltageThresholdClockValidation` | `togi_sim::suite::shared_voltage::requirement_coefficients_are_finite_and_nonnegative_without_an_upper_bound` | A |
| `TestSharedVoltageThresholdClockDecodeAndOwnership` | `togi_sim::suite::shared_voltage::background_and_requirement_coefficients_decode_from_a_file`<br>`togi_sim::suite::definition::a_model_keeps_what_it_was_built_from_by_value` | A |
| `TestSharedVoltageBackgroundHazard` | `togi_sim::suite::shared_voltage::the_background_rate_ignores_profile_workload_and_load_and_takes_the_onset_boost`<br>`togi_sim::suite::shared_voltage::the_background_is_part_of_the_total_and_never_of_a_numerator` | A |
| `TestSharedVoltageBackgroundDraws` | `togi_sim::suite::draws::background_failures_are_unattributed_crashes_and_match_their_probability`<br>`togi_sim::suite::probability::a_failure_time_is_the_inverse_of_exposure` | A |
| `TestSharedVoltageBackgroundFailureShare` | `togi_sim::suite::shared_voltage::the_background_is_part_of_the_total_and_never_of_a_numerator` | A |
| `TestSharedVoltageBackgroundValidation` | `togi_sim::suite::shared_voltage::the_background_rate_is_finite_and_nonnegative` | A |
| `TestSharedVoltageBackgroundReplayBypass` | `togi_sim::suite::draws::a_replayed_pass_overrides_the_background` | A |

### `internal/sim/sim_test.go` (11)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestR6ProgressRespectsTrialDuration` | `togi_sim::suite::runtime::r6_progress_counts_the_bursts_of_the_whole_trial`<br>`togi_sim::suite::runtime::an_r6_failure_before_the_transition_reports_only_its_bursts` | A |
| `TestSameSeedSameObservations` | `togi_sim::suite::runtime::the_same_seed_makes_the_same_observations_and_another_seed_does_not`<br>`togi_sim::suite::runtime::the_same_seed_and_history_make_the_same_observations` | A |
| `TestEachSignal` | `togi_sim::suite::runtime::each_signal_has_its_result_shape` | A |
| `TestNearLimitRate` | `togi_sim::suite::runtime::the_near_limit_rate_fails_a_core_at_its_limit_only_when_it_is_set`<br>`togi_sim::suite::hazard::near_limit_rate_applies_at_the_limit_and_nothing_below_it` | A |
| `TestTogetherOnlyLimit` | `togi_sim::suite::runtime::a_nonzero_outsider_switches_a_trial_to_the_together_limit`<br>`togi_sim::suite::hazard::r6_and_r7_use_the_together_limit_even_when_alone` | A |
| `TestBankTypes` | `togi_sim::suite::runtime::machine_checks_name_a_bank_of_the_kind_the_model_chooses` | A |
| `TestFaults` | `togi_sim::suite::runtime::a_readback_can_be_corrupted_once_after_the_next_write`<br>`togi_sim::suite::runtime::a_failed_write_leaves_the_register_alone_and_costs_one_write`<br>`togi_sim::suite::runtime::setup_failures_fail_the_next_starts_only`<br>`togi_sim::suite::runtime::a_worker_escapes_its_cpus_in_one_trial`<br>`togi_sim::suite::runtime::a_failed_check_shows_in_the_preflight_until_cleared`<br>`togi_sim::suite::runtime::the_bios_context_can_change_under_a_running_machine`<br>`togi_sim::suite::runtime::a_crash_before_apply_takes_the_first_write_of_the_boot`<br>`togi_sim::suite::runtime::a_trial_alone_with_an_outsider_at_a_nonzero_offset_is_a_violation` | A |
| `TestReboot` | `togi_sim::suite::runtime::a_reboot_restores_the_bios_offsets_and_takes_ninety_seconds` | A |
| `TestContextDone` | `togi_sim::suite::runtime::a_cancelled_wait_does_not_advance_the_clock`<br>`togi_sim::suite::runtime::a_cancelled_wait_consumes_the_trial` | A |
| `TestNewRejects` | `togi_sim::suite::validation::the_core_count_is_even_from_two_to_sixteen`<br>`togi_sim::suite::validation::bios_and_ranking_have_one_value_per_core`<br>`togi_sim::suite::validation::the_core_table_names_every_core_exactly_once`<br>`togi_sim::suite::validation::limits_idle_and_workload_limits_stay_within_minus_fifty_and_one` | A |
| `TestNewSignalWeights` | `togi_sim::suite::runtime::signal_weights_pick_in_proportion_and_skip_zero_weights`<br>`togi_sim::suite::validation::weights_are_finite_nonnegative_with_a_positive_total` | A |

### `internal/sim/smu_interrupt_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestSMUInterruptionSurroundsActualEffect` | `togi_sim::suite::hooks::a_hook_failing_before_an_access_stops_it_and_one_failing_after_cannot_undo_it`<br>`togi_sim::suite::hooks::a_write_transaction_is_seen_before_and_after_with_its_core_and_offset`<br>`togi_sim::suite::hooks::an_access_that_does_not_happen_is_not_reported_to_the_hook` | A |

### `internal/sim/trial_test.go` (10)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestTrialRNGStableConditionDomains` | `togi_sim::suite::trials::a_retry_reproduces_the_class_outcome`<br>`togi_sim::random::tests::domains_seeds_and_keys_give_independent_streams` | O(pinned Go's string-keyed draw streams per condition; streams are not Go's) |
| `TestModelHazards` | `togi_sim::suite::trials::an_onset_boost_moves_the_failure_along_the_same_cumulative_hazard`<br>`togi_sim::suite::trials::a_joint_fails_only_in_its_regimes_and_after_its_delay`<br>`togi_sim::suite::trials::a_joint_failure_is_attributed_to_the_lowest_loaded_member_whatever_the_order`<br>`togi_sim::suite::trials::a_joint_without_a_signal_crashes_whatever_the_model_signal`<br>`togi_sim::suite::trials::a_crash_of_an_unloaded_joint_or_idle_core_leaves_no_machine_check`<br>`togi_sim::suite::trials::a_workload_limit_beats_the_alone_limit_and_an_outsider_switches_to_together` | A |
| `TestFailureDrawAfterOnset` | `togi_sim::suite::trials::a_failure_after_the_onset_window_keeps_its_exposure`<br>`togi_sim::suite::probability::a_failure_time_is_the_inverse_of_exposure` | A |
| `TestUnloadedDrawFollowsLoadedSet` | `togi_sim::suite::trials::an_unloaded_core_draws_by_the_loaded_set_not_its_order` | A |
| `TestJointCrashMachineEvidence` | `togi_sim::suite::trials::a_joint_names_a_machine_check_core_only_when_it_is_told_to`<br>`togi_sim::suite::validation::joints_need_members_on_real_cores_and_valid_numbers` | A |
| `TestScriptsAndClocks` | `togi_sim::suite::trials::scripts_drive_outcomes_and_the_wall_and_monotonic_clocks_apart` | A |
| `TestScriptThenCrashReportsOnlyBackendSignals` | `togi_sim::suite::trials::a_script_that_then_crashes_reports_only_backend_signals`<br>`togi_sim::suite::trials::a_script_that_crashes_after_a_signal_returns_the_exposure_it_ran` | A |
| `TestScriptCrashResetAndBackendReport` | `togi_sim::suite::trials::a_scripted_backend_failure_reports_and_a_scripted_crash_picks_its_reset` | A |
| `TestWeightedCrashReset` | `togi_sim::suite::trials::a_crash_draws_its_reset_from_the_model` | A |
| `TestCrashResetWithoutWeights` | `togi_sim::suite::validation::reset_weights_are_finite_nonnegative_with_a_positive_total` | A |

## trialfacts

### `tools/trialfacts/class_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestSpecDescribesClass` | `togi_tool_trialfacts::trialfacts::record::a_class_names_what_ran`<br>`togi_tool_trialfacts::trialfacts::replay::replay_facts_keep_what_the_simulator_matches_and_replays` | A |
| `TestProfileClassKeyIgnoresProvenanceAndOutcome` | `togi_tool_trialfacts::trialfacts::key::the_key_ignores_provenance_condition_and_outcome`<br>`togi_tool_trialfacts::trialfacts::key::the_loaded_cores_are_a_set_and_the_key_hashes`<br>`togi_tool_trialfacts::trialfacts::key::an_idle_failure_is_not_a_trial_of_the_same_cores` | A |

### `tools/trialfacts/extract_io_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestExtractReportsSourceAndOutputFailures` | `togi_tool_trialfacts::trialfacts::extract::a_bad_source_is_an_error_and_no_facts`<br>`togi_tool_trialfacts::trialfacts::extract::a_session_whose_schema_is_not_readable_history_is_refused_by_name`<br>`togi_tool_trialfacts::trialfacts::file::a_write_failure_at_any_step_is_reported`<br>`togi_tool_trialfacts::trialfacts::file::a_destination_that_cannot_be_replaced_is_an_error_and_leaves_nothing_behind`<br>`togi_tool_trialfacts::trialfacts::file::a_fact_that_would_not_read_back_is_refused_and_leaves_the_destination_unchanged` | A |
| `TestReadRejectsInvalidExtracts` | `togi_tool_trialfacts::trialfacts::file::an_unreadable_extract_is_an_error_and_never_a_partial_result`<br>`togi_tool_trialfacts::trialfacts::file::an_extract_is_one_valid_record_per_line`<br>`togi_tool_trialfacts::trialfacts::file::an_extract_without_records_is_refused`<br>`togi_tool_trialfacts::trialfacts::file::a_record_that_contradicts_itself_or_the_vocabulary_is_refused` | A |

### `tools/trialfacts/extract_regression_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestExtractRejectsEmptyEvidence` | `togi_tool_trialfacts::trialfacts::extract::no_evidence_is_refused`<br>`togi_tool_trialfacts::trialfacts::file::an_empty_extract_changes_nothing` | A |

### `tools/trialfacts/extract_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestExtractAllSessionsPrivacyAndDeterminism` | `togi_tool_trialfacts::trialfacts::extract::every_session_extracts_oldest_first_and_only_the_whitelist_survives`<br>`togi_tool_trialfacts::trialfacts::extract::facts_follow_session_order_not_archive_order`<br>`togi_tool_trialfacts::trialfacts::extract::a_state_directory_on_disk_extracts_like_its_memory_image`<br>`togi_tool_trialfacts::trialfacts::file::an_extract_round_trips_through_its_file_byte_for_byte` | A |
| `TestExtractRecordOnlyPartialFacts` | `togi_tool_trialfacts::trialfacts::extract::partial_r7_facts_keep_their_class_and_measurements`<br>`togi_tool_trialfacts::trialfacts::extract::a_failure_outside_any_trial_is_an_idle_fact_of_the_session_cores` | A |

### `tools/trialfacts/replay_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestExtractsReplaySelectsTrialEvidence` | `togi_tool_trialfacts::trialfacts::replay::only_decisive_trials_with_a_context_answer_trials`<br>`togi_tool_trialfacts::trialfacts::replay::replay_facts_keep_what_the_simulator_matches_and_replays`<br>`togi_tool_trialfacts::trialfacts::replay::a_machine_without_an_extract_or_context_or_file_is_refused`<br>`togi_tool_trialfacts::trialfacts::replay::extracts_resolve_relative_to_the_machine_file` | A |
| `TestExtractsReadEachExtractOnce` | `togi_tool_trialfacts::trialfacts::replay::each_extract_is_read_once_and_shared` | A |

## session

### `internal/session/adjudicator_test.go` (5)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestAdjudicatorPrecedence` | `togi_session::evidence::tests::the_stronger_evidence_decides_in_every_pairing` | A |
| `TestBackendFailuresAndDuration` | `togi_session::evidence::tests::a_backend_failure_survives_every_weaker_diagnostic_and_keeps_its_core`<br>`togi_session::evidence::tests::a_trial_passes_only_when_it_ran_its_full_duration` | A |
| `TestRunnerEvidenceSurvivesErrors` | `togi_session::run_trial::tests::evidence_survives_a_runner_error_and_the_error_stays_in_the_reason` | A |
| `TestInterruptedBackendFailureWithoutReset` | `togi_session::recover::tests::a_trial_a_stopped_run_left_open_in_this_boot_ends_as_the_signal_or_the_stop_it_proves`<br>`togi_session::recovery::tests::a_backend_signal_is_kept_on_the_open_trial` | A |
| `TestRunnerMissingCurrentBoot` | `togi_session::run_trial::tests::a_missing_current_boot_leaves_what_the_trial_itself_showed` | A |

### `internal/session/boot_interrupt_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestBootHandoffResumesEveryCleanupBoundary` | `togi_session::whole::dead_end::a_tuning_boot_handoff_interrupted_at_any_cleanup_boundary_resumes_without_repeating_or_skipping_it` | A |
| `TestFailedBootCleanupNeverAuthorizesRebootOnResume` | `togi_session::whole::dead_end::a_failed_boot_cleanup_never_authorizes_a_reboot_when_the_handoff_resumes` | A |

### `internal/session/boot_progress_test.go` (10)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestTuningBootResetsCountOnlyAfterFirstDurableAppend` | `togi_session::boot::tests::the_restart_count_resets_once_and_only_after_the_first_durable_append` | A |
| `TestTuningBootFailedAppendDoesNotResetCount` | `togi_session::boot::tests::a_failed_first_append_leaves_the_count_and_the_reason_alone` | A |
| `TestTuningBootReasonReadFailureDoesNotAppend` | `togi_session::boot::tests::a_reason_that_cannot_be_read_stops_before_anything_is_appended`<br>`togi_session::session::tests::a_tuning_boot_reason_that_cannot_be_read_returns_the_journal_untouched` | A |
| `TestTuningBootReasonSurvivesEveryImportBoundary` | `togi_session::boot::tests::the_pending_reason_is_journaled_then_cleared`<br>`togi_session::boot::tests::a_reason_whose_event_failed_stays_pending_and_is_imported_by_the_next_run`<br>`togi_session::boot::tests::a_reason_durable_before_its_clearing_failed_is_not_journaled_twice`<br>`togi_session::boot::tests::a_reason_an_archived_journal_holds_is_cleared_without_a_new_event` | A |
| `TestTuningBootGRUBProgressFailuresAreNotJournalFailures` | `togi_session::boot::tests::bootloader_failures_are_ordinary_errors_and_the_journal_keeps_working` | A |
| `TestDeadEndWritesBoundedReasonAndReportsWriteFailure` | `togi_session::boot::tests::leaving_for_a_dead_end_saves_a_bounded_reason_then_clears_the_saved_entry`<br>`togi_session::boot::tests::a_reason_that_cannot_be_saved_never_changes_whether_the_entry_was_cleared` | A |
| `TestTuningBootReasonSurvivesResumedDeadEnd` | `togi_session::boot::tests::a_resumed_dead_end_acknowledges_the_pending_tuning_boot_reason_once_and_leaves_its_own`<br>`togi_session::boot::tests::a_resumed_dead_end_whose_pending_reason_cannot_be_acknowledged_stops_before_the_grub_handoff`<br>`togi_session::boot::tests::a_resumed_dead_end_whose_saved_entry_append_fails_keeps_the_acknowledgement_and_its_own_reason` | A |
| `TestBootLeaveReasonPreservesShutdownCompletion` | `togi_session::recovery::tests::a_warning_or_leave_reason_never_replaces_a_shutdown`<br>`togi_session::recovery::tests::a_boot_without_a_shutdown_crashed_unless_it_is_current_or_a_crash_was_recorded` | A |
| `TestRestartLimitSequenceDemo` | `togi_tuningboot::tests::two_retries_then_exhaustion_clears_saved_entry_and_stays_at_three`<br>`togi_session::boot::tests::the_pending_reason_is_journaled_then_cleared` | X |
| `TestResumedDeadEndReasonErrorsStopBeforeGRUBHandoff` | `togi_session::boot::tests::a_resumed_dead_end_whose_pending_reason_cannot_be_acknowledged_stops_before_the_grub_handoff` | A |

### `internal/session/carry_backend_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestCarryRecordsOnlyPassesOfTheLoadedBackend` | `togi_carry::tests::resolution::resolve_drops_only_changed_backend_passes_keeps_failures`<br>`togi_session::begin::tests::every_eligible_fact_is_carried_once_in_order_before_one_commitment_and_the_first_phase` | X |

### `internal/session/carry_test.go` (3)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestCarryFactsResumeWithoutDuplicates` | `togi_session::begin::tests::every_eligible_fact_is_carried_once_in_order_before_one_commitment_and_the_first_phase`<br>`togi_session::begin::tests::a_begin_interrupted_before_or_after_any_append_up_to_the_commitment_resumes_to_one_complete_carry` | A |
| `TestCarryFactsRequireSameBIOS` | `togi_carry::tests::seeds::bios_change_archives_metadata_carry_available`<br>`togi_carry::tests::facts_walk::resolve_requires_context_and_retries` | X |
| `TestCarryFactsWaitForValidatedContext` | `togi_session::begin::tests::a_carry_is_committed_after_the_context_and_baseline_and_before_any_phase_once`<br>`togi_session::begin::tests::a_begin_interrupted_before_or_after_any_append_up_to_the_commitment_resumes_to_one_complete_carry` | A |

### `internal/session/checked_profile_test.go` (8)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestApplyWritesShallowBeforeDeepInCoreOrder` | `togi_session::write::tests::a_profile_zeroes_an_unknown_machine_then_writes_shallower_before_deeper` | A |
| `TestApplyRefusesCombinationBeforeSMUWrite` | `togi_session::write::tests::a_refused_write_appends_no_intent_and_reaches_no_hardware`<br>`togi_session::write::tests::a_write_completing_a_folded_combination_is_refused_after_the_safe_steps` | A |
| `TestApplyRefusesCombinationAndPartialWrite` | `togi_session::write::tests::a_combination_is_refused_at_the_step_that_reaches_it_with_the_earlier_steps_kept`<br>`togi_session::write::tests::a_failed_command_is_recorded_and_is_an_smu_dead_end_with_the_verified_steps_kept` | A |
| `TestRestoreNeverReachesCombinationWithNonzeroBaseline` | `togi_session::whole::same_boot::a_restoration_after_a_combination_hunt_never_reaches_the_combination`<br>`togi_session::restore::tests::the_restoration_stops_at_what_the_tuner_still_holds`<br>`togi_session::write::tests::a_write_completing_a_folded_combination_is_refused_after_the_safe_steps` | A |
| `TestApplyRefusesFailurePoint` | `togi_session::write::tests::a_write_reaching_a_folded_failure_point_is_refused_with_no_intent_appended`<br>`togi_session::write::tests::a_failure_point_is_refused_before_any_write_of_that_core` | A |
| `TestFoldTracksRegistersBySparseCoreID` | `togi_session::recovery::tests::registers_are_the_baseline_lowered_by_intents_and_certain_after_a_readback_by_core_id`<br>`togi_session::recovery::tests::a_sparse_machine_is_keyed_by_core_id_not_by_position` | A |
| `TestIdleEvidenceWindowStartsAtApplicationsFirstWrite` | `togi_session::recovery::tests::idle_evidence_starts_at_the_applications_first_write` | A |
| `TestFoldConsumesAttributedIdleFailure` | `togi_session::recovery::tests::stray_crashes_and_idle_crashes_stand_until_something_answers_them`<br>`togi_session::recover::tests::an_idle_crash_is_recorded_once_and_answered_with_one_failure` | A |

### `internal/session/checking_test.go` (4)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestIdleCrashInChecking` | `togi_session::recover::tests::an_idle_crash_is_recorded_once_and_answered_with_one_failure`<br>`togi_session::recovery::tests::a_nonzero_write_outside_a_trial_is_an_application_together_or_parked_once_a_group_arms_it`<br>`togi_session::recovery::tests::idle_evidence_starts_at_the_applications_first_write` | A |
| `TestResumeContinuesBoots` | `togi_session::whole::misc::a_resumed_checking_session_continues_its_numbering_on_new_boots_in_time_order` | A |
| `TestCrashEvidenceIsClaimedOnce` | `togi_session::recovery::tests::a_crash_claims_its_machine_checks_once`<br>`togi_session::evidence::tests::a_crash_owns_its_boots_machine_checks_and_the_uncorrected_ones_its_successor_retained` | A |
| `TestThermalTripIdleCrashClassification` | `togi_session::recover::tests::an_idle_crash_follows_the_reset_after_it_and_only_a_failure_is_answered`<br>`togi_session::recover::tests::an_uncorrected_machine_check_the_next_boot_kept_is_evidence_and_a_corrected_one_is_not`<br>`togi_session::recovery::tests::a_thermal_trip_stands_as_evidence_until_its_dead_end_and_kernel_retries_end_with_a_crash` | A |

### `internal/session/commands_test.go` (5)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestResetCore` | `togi_session::commands::reset_core_appends_the_command_and_nothing_else`<br>`togi_session::commands::a_reset_core_is_consumed_by_the_resumed_session_which_retunes_it_to_its_limit`<br>`togi_session::commands::tests::resetting_a_core_appends_only_the_command_and_no_shutdown` | A |
| `TestResetAll` | `togi_session::commands::reset_all_archives_the_session_and_the_directory_starts_empty`<br>`togi_session::commands::tests::resetting_everything_records_the_command_and_archives_the_session`<br>`togi_journal::dir::tests::a_session_id_is_the_utc_second_until_it_is_taken` | A |
| `TestResetAllRefusesAnExistingArchive` | `togi_session::commands::reset_all_that_cannot_archive_fails_and_releases_the_lock`<br>`togi_journal::journal::tests::archive::an_archive_that_already_exists_is_refused_before_anything_is_recorded` | X |
| `TestResetRefusalsLeaveJournalUnchanged` | `togi_session::commands::tests::resetting_in_an_empty_journal_is_no_session`<br>`togi_session::commands::reset_all_of_an_empty_journal_is_no_session_and_releases_the_lock`<br>`togi_session::commands::tests::resetting_a_core_with_another_build_is_refused_before_the_journal_is_folded`<br>`togi_session::commands::tests::resetting_a_core_the_session_lacks_is_refused_and_appends_nothing`<br>`togi_session::commands::reset_core_refuses_a_core_the_session_lacks_and_leaves_the_journal_alone` | A |
| `TestResetCoreAppendFailurePreservesDurablePrefix` | `togi_session::commands::a_failed_reset_core_append_keeps_the_durable_prefix_and_only_a_synced_command_survives` | A |

### `internal/session/compat_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestRunRefusesDifferentRulesetWithoutAppending` | `togi_session::session::tests::a_journal_of_another_build_is_refused_before_any_append`<br>`togi_session::session::tests::a_refused_journal_whose_saved_entry_cannot_be_cleared_keeps_both_errors` | A |

### `internal/session/config_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestConfigSnapshotPreservesCompleteWireShapeAndRoundTrips` | `togi_journal::tests::wire::every_kind_round_trips_through_the_wire` | X |

### `internal/session/crash_every_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestCrashAtEveryTrialEvent` | `togi_session::whole::crash::a_crash_at_every_trial_event_leaves_a_resumable_journal` | A |

### `internal/session/deadend_test.go` (8)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestDeadEndClearsSavedEntry` | `togi_session::whole::dead_end::a_dead_end_interrupted_before_the_saved_entry_clear_resumes_and_clears_it_once`<br>`togi_session::boot::tests::leaving_for_a_dead_end_saves_a_bounded_reason_then_clears_the_saved_entry` | A |
| `TestResumeInterruptedDeadEnd` | `togi_session::whole::dead_end::a_dead_end_interrupted_before_the_saved_entry_clear_resumes_and_clears_it_once` | A |
| `TestResumeAfterSavedEntryRecorded` | `togi_session::whole::dead_end::a_dead_end_resumed_after_its_saved_entry_was_recorded_only_shuts_down` | A |
| `TestResumeInterruptedDeadEndOutsideTuningBoot` | `togi_session::whole::dead_end::a_dead_end_resumed_outside_the_tuning_boot_only_shuts_down_and_the_next_run_tunes` | A |
| `TestFullyRecordedDeadEndReevaluatesPreflight` | `togi_session::whole::dead_end::a_fully_recorded_dead_end_does_not_stop_the_next_run_from_checking_preflight_again` | A |
| `TestDeadEndWithoutBootloaderExits` | `togi_session::session::tests::stray_crashes_end_in_a_boot_loop_that_exits_without_a_bootloader` | A |
| `TestUnconfirmedCleanupDeadEnd` | `togi_session::run_trial::tests::a_containment_error_from_the_wait_withholds_the_proof_a_successful_stop_gives`<br>`togi_session::run_trial::tests::a_start_that_returns_no_proof_dead_ends_on_containment_and_the_machine_is_left_alone`<br>`togi_session::run_trial::tests::an_unconfirmed_containment_withholds_the_proof_and_ends_the_trial_as_such`<br>`togi_session::recovery::tests::containment_evidence_names_the_escaped_cpus_or_carries_the_error_until_its_dead_end`<br>`togi_session::recover::tests::a_trial_whose_containment_is_unconfirmed_keeps_its_files_as_they_are` | A |
| `TestDeferredContainmentCleanupDoesNotEmergencyRestore` | `togi_session::close::tests::without_the_proof_that_every_workload_stopped_nothing_is_restored_or_recorded`<br>`togi_session::close::tests::a_machine_out_of_reach_is_left_alone_and_records_no_shutdown`<br>`togi_session::emergency::tests::nothing_touches_the_smu_without_a_sweep_a_validation_and_a_confirmed_teardown` | A |

### `internal/session/defect_recovery_test.go` (3)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestDefectAnswerResumesMissingCoreResets` | `togi_session::defects::tests::an_interrupted_multi_core_answer_resumes_to_the_effects_of_an_uninterrupted_one`<br>`togi_session::defects::tests::a_yes_answer_whose_resets_were_lost_is_finished_without_asking_again` | A |
| `TestLegacyDefectAnswerPreservesLaterCoreProgress` | — | O(the legacy defect-answer shape; the native journal has no legacy answers) |
| `TestDefectAnswerDoesNotReuseConsumedFindingResets` | — | O(resets citing the finding are the legacy answer order; native resets cite the answer) |

### `internal/session/defects_test.go` (5)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestDefectAnswersAndDisarm` | `togi_session::defects::tests::a_matching_decision_is_recorded_as_a_finding_citing_it`<br>`togi_session::defects::tests::a_yes_answer_queues_a_reset_of_every_affected_core_citing_it`<br>`togi_session::defects::tests::a_no_answer_resets_nothing`<br>`togi_session::defects::tests::an_aggressive_finding_without_a_terminal_is_a_defect_dead_end`<br>`togi_session::commands::a_reset_core_is_consumed_by_the_resumed_session_which_retunes_it_to_its_limit` | A |
| `TestRealPowerOffDefectFoundOnceOnResume` | `togi_session::whole::misc::a_defect_in_decisions_before_its_fix_is_found_once_however_often_the_session_resumes` | A |
| `TestDefectPromptFailureLeavesAnswerUncommitted` | `togi_session::defects::tests::a_question_that_cannot_be_asked_is_an_error_and_records_no_answer` | A |
| `TestDefectPromptCanceledStopsWithoutAnswering` | `togi_session::defects::tests::a_cancelled_question_stops_the_run_with_the_finding_unanswered`<br>`togi_session::startup::tests::a_defect_prompt_cancelled_at_startup_stops_the_run_before_any_trial` | A |
| `TestLegacyDefectAnswerRejectsUnrelatedWarning` | — | O(legacy answer recovery from projection warnings; the native journal has no legacy answers) |

### `internal/session/drain_stop_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestStopDrainsDurableFailureBoundaries` | `togi_session::close::tests::a_stop_at_a_durable_boundary_drains_what_it_owes_and_starts_no_new_work`<br>`togi_session::close::tests::drained_commitments_are_recorded_before_the_restoration`<br>`togi_session::close::tests::a_drained_dead_end_replaces_the_stop_and_its_commitments_come_first`<br>`togi_session::run::tests::signal_stops_cleanly_and_drains_the_completed_trial_failure` | A |

### `internal/session/emergency_test.go` (8)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestJournalFailureZerosMachineAndStopsAppending` | `togi_session::close::tests::a_journal_failure_zeroes_the_machine_through_the_gate_and_appends_nothing`<br>`togi_session::emergency::tests::a_gated_emergency_zeroes_every_core_reads_each_back_and_reports`<br>`togi_session::run_trial::tests::a_failed_start_record_of_a_joined_workload_zeroes_the_machine_as_the_emergency`<br>`togi_session::run_trial::tests::a_journal_that_fails_anywhere_in_a_trial_leaves_no_workload_running` | A |
| `TestEarlyJournalFailureDoesNotAccessUnvalidatedSMU` | `togi_session::emergency::tests::nothing_touches_the_smu_without_a_sweep_a_validation_and_a_confirmed_teardown`<br>`togi_session::close::tests::the_emergency_is_withheld_without_a_validated_smu_or_after_a_fault`<br>`togi_session::whole::crash::every_early_append_failure_respects_the_sweep_gate` | A |
| `TestJournalFailureReportsFailedEmergencyZeroAndReadbacks` | `togi_session::emergency::tests::a_failed_zeroing_is_reported_with_what_every_core_still_reads` | A |
| `TestStateRewriteFailureOnResumeRestoresSafeOffsets` | `togi_session::publication::tests::state_rebuild_write_failure_warns_and_continues`<br>`togi_session::publication::tests::state_write_failure_warns_continues_and_rebuilds` | A |
| `TestEveryEarlyJournalAppendFailureRespectsSweepGate` | `togi_session::whole::crash::every_early_append_failure_respects_the_sweep_gate` | A |
| `TestJournalFailureWithUnconfirmedTeardownWithholdsAllWrites` | `togi_session::run_trial::tests::a_containment_error_from_the_wait_withholds_the_proof_a_successful_stop_gives`<br>`togi_session::run_trial::tests::a_start_that_returns_no_proof_dead_ends_on_containment_and_the_machine_is_left_alone`<br>`togi_session::emergency::tests::nothing_touches_the_smu_without_a_sweep_a_validation_and_a_confirmed_teardown` | A |
| `TestBaselineReadFailureDeadEndsBeforeAnyWrite` | `togi_session::begin::tests::a_baseline_failing_to_read_is_smu_evidence_and_leaves_no_baseline`<br>`togi_session::write::tests::a_failed_read_records_the_error_and_is_smu_evidence` | A |
| `TestRestoreSMUFailureChangesOnlyCleanStopOutcome` | `togi_session::close::tests::a_restoration_that_fails_at_the_smu_turns_a_clean_stop_into_an_smu_dead_end`<br>`togi_session::close::tests::a_restoration_that_fails_at_the_smu_keeps_a_dead_end_the_run_already_reached` | A |

### `internal/session/hunt_restore_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestRestoreAfterCombinationHuntCommitmentNeverReachesCombination` | `togi_session::whole::same_boot::a_restoration_after_a_combination_hunt_never_reaches_the_combination`<br>`togi_session::write::tests::a_write_completing_a_folded_combination_is_refused_after_the_safe_steps`<br>`togi_session::restore::tests::the_restoration_stops_at_what_the_tuner_still_holds` | A |

### `internal/session/identity_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestFrozenClockSessionsArchiveWithoutCollision` | `togi_journal::dir::tests::a_session_id_is_the_utc_second_until_it_is_taken`<br>`togi_journal::dir::tests::suffixes_order_numerically_so_ten_follows_nine`<br>`togi_session::session::tests::an_identity_that_cannot_be_allocated_leaves_the_journal_empty` | X |

### `internal/session/kernel_test.go` (10)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestKernelBoundariesExcludeProfileWritesAndRetryWaits` | `togi_session::run_trial::tests::profile_writes_and_retry_waits_are_outside_every_trial_window` | A |
| `TestKernelBoundaryLossIsStickyBelowStrongerEvidence` | `togi_session::run_trial::tests::a_lost_start_boundary_keeps_its_error_below_stronger_evidence`<br>`togi_session::recovery::tests::a_kernel_read_error_is_sticky_on_its_open_trial_and_cursors_are_per_boot` | A |
| `TestKernelTeardownMCEIsTrialEvidence` | `togi_session::run_trial::tests::a_machine_check_the_reset_provokes_is_evidence_of_the_trial` | A |
| `TestKernelCursorResumeCoversSameBootAndReboot` | `togi_session::session::tests::a_reopened_session_resumes_its_own_boots_cursor_and_files_the_tail_between_trials`<br>`togi_session::recovery::tests::a_kernel_read_error_is_sticky_on_its_open_trial_and_cursors_are_per_boot` | A |
| `TestRebootRecordsBetweenTrialMCEWithoutDecisionEvidence` | `togi_session::recover::tests::the_crashed_boots_own_machine_checks_are_the_trials_only_inside_its_window`<br>`togi_session::session::tests::a_reopened_session_resumes_its_own_boots_cursor_and_files_the_tail_between_trials` | A |
| `TestInterruptedSetupReplaysReadbackWindow` | `togi_session::whole::same_boot::an_interrupted_setup_replays_only_the_matching_intent_write_readback_into_the_window` | A |
| `TestDeadEndShutdownCapturesRestorationTail` | `togi_session::close::tests::the_shutdown_follows_the_machine_check_of_the_restoration_tail` | A |
| `TestPartialCursorEvidenceOutranksFailedReanchor` | `togi_session::run_trial::tests::partial_machine_check_evidence_outranks_a_failed_or_successful_reanchor`<br>`togi_session::run_trial::tests::a_partial_kernel_read_with_a_machine_check_outranks_the_read_error`<br>`togi_session::session::tests::the_machine_checks_of_both_partial_reads_survive_a_failed_reanchor` | A |
| `TestUncorrectedTrialMCERejectsFullDurationPass` | `togi_session::run_trial::tests::an_uncorrected_machine_check_rejects_a_full_duration_pass`<br>`togi_session::evidence::tests::an_uncorrected_machine_check_rejects_a_full_duration_pass` | A |
| `TestTrialKernelCrashNeverInventsPass` | `togi_session::run_trial::tests::a_crash_at_a_kernel_boundary_never_invents_a_pass`<br>`togi_session::run_trial::tests::a_crash_on_the_fresh_read_of_a_reanchor_never_invents_a_pass`<br>`togi_session::session::tests::a_crash_on_the_read_after_a_lost_cursor_ends_the_step_with_no_evidence_recorded` | A |

### `internal/session/mce_window_test.go` (4)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestRecoveryDoesNotClaimRecordedPreWindowMCE` | `togi_session::recover::tests::a_machine_check_the_journal_already_holds_is_never_recorded_or_claimed_again` | A |
| `TestKernelMCEWindowBoundaries` | `togi_session::evidence::tests::a_kernel_machine_check_belongs_to_a_trial_inside_its_window_both_ends_inclusive`<br>`togi_session::recovery::tests::a_machine_check_is_the_same_by_boot_time_cpu_and_bank_not_by_its_text` | A |
| `TestMCEReplayKeepsBootLocalAndLegacyEvidence` | `togi_session::recovery::tests::which_machine_checks_belong_to_the_trial_and_which_to_its_crash`<br>`togi_session::evidence::tests::a_crash_owns_its_boots_machine_checks_and_the_uncorrected_ones_its_successor_retained`<br>`togi_session::evidence::tests::a_machine_check_without_a_time_counts_and_one_with_a_time_counts_from_the_window_start` | A |
| `TestRecoveryRecordsPreWindowMCEWithoutCause` | `togi_session::recover::tests::the_crashed_boots_own_machine_checks_are_the_trials_only_inside_its_window` | A |

### `internal/session/owner_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestRunOwnerEveryExit` | `togi_session::whole::dead_end::every_stop_and_error_exit_joins_the_trial_restores_the_hardware_publishes_the_state_and_returns_the_journal`<br>`togi_session::close::tests::a_cleanup_error_follows_the_error_that_ended_the_run` | A |
| `TestRunOwnerSkipsRestoreUntilWorkloadJoined` | `togi_session::close::tests::without_the_proof_that_every_workload_stopped_nothing_is_restored_or_recorded`<br>`togi_session::restore::tests::without_the_proof_that_nothing_runs_nothing_is_written` | A |

### `internal/session/parked_application_test.go` (3)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestParkedApplicationCrash` | `togi_session::recover::tests::a_crash_under_an_armed_hunt_group_is_a_parked_application`<br>`togi_session::recovery::tests::a_nonzero_write_outside_a_trial_is_an_application_together_or_parked_once_a_group_arms_it` | A |
| `TestParkedApplicationCrashAfterSameBootRestore` | `togi_session::whole::same_boot::a_crash_in_the_reapplication_after_a_same_boot_restore_is_a_parked_crash_that_ends_the_hunt_on_its_core`<br>`togi_session::whole::same_boot::a_hunt_groups_parked_condition_ends_with_the_hunt_so_the_next_together_application_crashes_as_together` | A |
| `TestHuntGroupArmsParkedUntilReplaced` | `togi_session::recovery::tests::a_hunt_group_stays_armed_through_a_restoration_until_something_replaces_it`<br>`togi_session::recovery::tests::a_together_application_a_profile_change_or_the_hunts_end_disarms_the_group`<br>`togi_session::recovery::tests::a_nonzero_write_outside_a_trial_is_an_application_together_or_parked_once_a_group_arms_it` | A |

### `internal/session/pstore_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestCrashRecoveryPstore` | `togi_session::recover::tests::a_saved_pstore_record_is_attached_an_absent_one_is_fine_and_an_unreadable_one_warns_once` | A |

### `internal/session/ranking_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestRankingFallbackAndTies` | `togi_session::execute::tests::a_ranking_without_a_value_for_every_core_falls_back_to_core_ids`<br>`togi_session::execute::tests::a_ranking_every_core_shares_falls_back_to_core_ids`<br>`togi_session::execute::tests::a_ranking_the_host_gives_orders_cores_best_first_and_ties_by_core_id` | A |

### `internal/session/recovery_rules_test.go` (14)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestTrialResetClassification` | `togi_session::recover::tests::the_reset_after_a_crash_decides_what_an_open_trial_ended_as`<br>`togi_session::recovery::tests::a_thermal_trip_stands_as_evidence_until_its_dead_end_and_kernel_retries_end_with_a_crash` | A |
| `TestTrialResetReasonAcrossNonTogiBoot` | `togi_session::recover::tests::a_later_boot_cannot_replace_the_reset_reason_of_the_boot_that_followed_the_crash` | A |
| `TestIdleResetReasonAcrossNonTogiBoot` | `togi_session::recover::tests::a_later_boot_cannot_replace_the_reset_reason_of_the_boot_that_followed_the_crash`<br>`togi_session::recover::tests::an_idle_crash_follows_the_reset_after_it_and_only_a_failure_is_answered` | A |
| `TestSignalBeforePowerLossSurvivesCrash` | `togi_session::recover::tests::a_backend_signal_before_the_power_loss_survives_and_keeps_its_core`<br>`togi_session::recovery::tests::a_backend_signal_is_kept_on_the_open_trial` | A |
| `TestPowerButtonBetweenTrialsIsInconclusive` | `togi_session::recover::tests::an_idle_crash_follows_the_reset_after_it_and_only_a_failure_is_answered`<br>`togi_simrun::sim::crash::a_power_button_between_trials_is_inconclusive` | A |
| `TestTrialProgressAndDurationIgnoreWallJump` | `togi_session::run_trial::tests::a_cancelled_wait_stays_interrupted_and_does_not_count_toward_the_backend` | A |
| `TestCorrectedMCESelectionSurvivesWallJump` | `togi_simrun::sim::crash::a_wall_clock_jump_does_not_lose_a_trials_corrected_mce`<br>`togi_sim::suite::trials::scripts_drive_outcomes_and_the_wall_and_monotonic_clocks_apart`<br>`togi_session::evidence::tests::a_kernel_machine_check_belongs_to_a_trial_inside_its_window_both_ends_inclusive` | X |
| `TestInterruptedTrialDurationUsesMonotonicTimeAfterWallJump` | `togi_session::recovery::tests::an_open_trial_ran_from_its_start_to_its_last_record_by_the_monotonic_clock_whatever_the_wall_did`<br>`togi_session::recover::tests::an_interrupted_trial_ran_until_its_last_own_sample_and_another_trials_sample_adds_nothing` | A |
| `TestCrashRecoveryLastSample` | `togi_session::recover::tests::a_crashed_trials_end_carries_the_last_complete_sample_and_ignores_a_torn_one`<br>`togi_session::evidence::tests::a_summary_fills_an_end_with_the_stall_of_a_failure_and_the_last_sample_of_a_crash` | A |
| `TestReplayCrashRecoveryPreservesFact` | `togi_sim::suite::trials::a_replayed_crash_or_uncorrected_machine_check_happens_at_its_recorded_exposure`<br>`togi_sim::suite::trials::a_replayed_crash_resets_by_watchdog_whatever_the_models_reset_mix`<br>`togi_sim::suite::replay::every_failure_signal_the_extract_can_hold_is_accepted_and_a_zero_exposure_is_valid`<br>`togi_session::recover::tests::the_reset_after_a_crash_decides_what_an_open_trial_ended_as`<br>`togi_session::recovery::tests::an_open_trial_ran_from_its_start_to_its_last_record_by_the_monotonic_clock_whatever_the_wall_did` | X |
| `TestReplayMachineCheckKeepsBankAttribution` | `togi_sim::suite::trials::a_replayed_uncorrected_machine_check_names_a_bank_by_the_models_probability`<br>`togi_tuner::r7_attribution_tests::r7_live_named_and_mce_attribution`<br>`togi_tuner::fold_tests::mces_outside_the_open_trials_core_local_window_are_not_remembered` | X |
| `TestTrialRequestedVoltageAllRegimes` | `togi_session::evidence::tests::the_requested_voltage_is_the_loaded_cores_highest_request_by_median_and_minimum`<br>`togi_session::evidence::tests::a_summary_fills_an_end_with_the_stall_of_a_failure_and_the_last_sample_of_a_crash`<br>`togi_session::evidence::tests::request_telemetry_needs_twenty_samples_after_the_warmup_and_every_loaded_core_on_a_known_ccd` | A |
| `TestCrashRecoveryRequestedVoltage` | `togi_session::recover::tests::a_recovered_trials_end_carries_the_request_telemetry_of_its_loaded_cores` | A |
| `TestPartialCrashRecoversIntoBackoff` | `togi_session::whole::crash::a_partial_crash_recovers_into_the_named_or_loaded_core_backoff_through_run` | A |

### `internal/session/requests_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestTrialRequestTelemetry` | `togi_session::run_trial::tests::request_telemetry_needs_twenty_samples_after_the_warmup`<br>`togi_session::evidence::tests::request_telemetry_needs_twenty_samples_after_the_warmup_and_every_loaded_core_on_a_known_ccd` | A |
| `TestCrashRecoveryRequestTelemetry` | `togi_session::recover::tests::a_recovered_trials_end_carries_the_request_telemetry_of_its_loaded_cores`<br>`togi_session::recover::tests::compression_precedes_the_end_so_a_crash_between_them_leaves_the_files_to_compress_again` | A |

### `internal/session/retries_test.go` (13)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestBackendRetriesAtOneFiveThirtyMinutesAndResumeWait` | `togi_session::run_trial::tests::a_trial_waits_at_one_five_and_thirty_minutes_after_inconclusive_trials_and_resumes_a_wait`<br>`togi_session::evidence::tests::a_backend_waits_one_five_then_thirty_minutes_after_the_threshold` | A |
| `TestRecoveryKernelLogRetriesAndDeadEnd` | `togi_session::recover::tests::the_kernel_log_is_retried_after_one_five_and_thirty_minutes_then_read_or_given_up_on`<br>`togi_session::startup::tests::a_kernel_log_unreadable_through_every_recovery_retry_is_a_no_evidence_dead_end_citing_them`<br>`togi_session::recovery::tests::kernel_retries_end_with_a_no_evidence_dead_end_so_a_later_run_retries_afresh`<br>`togi_session::evidence::tests::the_kernel_log_is_retried_on_the_same_schedule` | A |
| `TestRecoveryKernelLogRetryStopsCleanlyOnSignal` | `togi_session::whole::misc::a_signal_during_the_kernel_log_retry_wait_stops_the_run_and_leaves_the_crashed_trial_open` | A |
| `TestRecoverySkipsAnOlderBootTheSystemJournalDropped` | `togi_session::recover::tests::the_kernel_log_of_this_boot_is_retried_though_an_older_boot_the_system_journal_dropped_is_not` | A |
| `TestRecoveryWithoutSuccessorDoesNotBorrowCurrentResetReason` | `togi_session::recover::tests::a_crash_without_an_identified_successor_borrows_no_reset_reason_from_this_boot` | A |
| `TestMissingBackendDeadEndsWithoutRetries` | `togi_session::run_trial::tests::a_missing_backend_dead_ends_at_once_with_no_retry`<br>`togi_session::recovery::tests::a_missing_backend_is_evidence_until_the_no_evidence_dead_end` | A |
| `TestRecoveryRetryResumeUsesSameAttempt` | `togi_session::recover::tests::a_run_cut_short_in_a_retry_wait_waits_the_same_attempt_again_without_a_new_record`<br>`togi_session::recovery::tests::a_retry_is_recorded_once_per_attempt_and_again_after_a_trial_followed_it` | A |
| `TestBackendRetriesAfterInconclusiveTrials` | `togi_session::run_trial::tests::four_backend_setup_failures_wait_then_dead_end_without_evidence`<br>`togi_session::recovery::tests::a_backend_inconclusive_through_every_retry_is_exhausted_until_a_no_evidence_dead_end`<br>`togi_session::evidence::tests::no_evidence_comes_three_inconclusive_trials_after_the_threshold` | A |
| `TestRunnerWaitErrorsCountTowardBackendDeadEnd` | `togi_session::run_trial::tests::a_runner_failure_ends_an_inconclusive_trial_that_counts_toward_the_backend` | A |
| `TestRunnerWaitCancellationRemainsInterrupted` | `togi_session::run_trial::tests::a_cancelled_wait_stays_interrupted_and_does_not_count_toward_the_backend` | A |
| `TestBackendRetryCancellationStopsBeforeAnotherTrial` | `togi_session::run_trial::tests::a_cancelled_backend_wait_stops_the_run_before_another_trial` | A |
| `TestRetryAppendFailurePreventsSleepAndTrial` | `togi_session::run_trial::tests::a_backend_retry_that_cannot_be_journaled_prevents_the_wait_and_the_trial`<br>`togi_session::recover::tests::a_kernel_retry_that_cannot_be_recorded_fails_before_any_wait_or_further_read` | A |
| `TestPendingKernelRetryCancellationKeepsObservationPending` | `togi_session::recover::tests::a_cancelled_wait_appends_nothing_and_a_pending_retry_is_not_read_again` | A |

### `internal/session/same_boot_test.go` (9)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestSameBootRestoresBeforeDeadEndShutdown` | `togi_session::whole::same_boot::a_same_boot_resume_restores_the_safe_target_before_it_finishes_the_pending_dead_end`<br>`togi_session::whole::same_boot::a_same_boot_resume_finishes_a_dead_end_it_drains_without_deepening_any_core` | A |
| `TestSameBootKillAtEveryRestoreEvent` | `togi_session::whole::same_boot::a_kill_after_every_durable_restoration_event_of_the_same_boot_converges` | A |
| `TestRebootSkipsSameBootReconciliation` | `togi_session::whole::same_boot::a_different_boot_never_reconciles_what_the_same_boot_left` | A |
| `TestSameBootResumeDrainsPendingFailureBeforeRestore` | `togi_session::whole::same_boot::a_same_boot_resume_drains_the_pending_failure_before_it_restores` | A |
| `TestSameBootResumeRejectsUnvalidatedSMU` | `togi_session::startup::tests::a_same_boot_resume_with_an_unsupported_smu_is_a_preflight_dead_end_that_leaves_the_offsets` | A |
| `TestCanceledSameBootResumeStillRequiresArmedWatchdog` | `togi_session::startup::tests::a_resumed_boot_does_not_give_up_on_the_watchdog_before_it_is_reconciled`<br>`togi_session::startup::tests::a_watchdog_that_never_arms_is_a_preflight_dead_end_after_thirty_seconds` | A |
| `TestCanceledRecoveryReconcilesInheritedOffsetsBeforeSignalShutdown` | `togi_session::startup::tests::a_run_cancelled_while_recovery_waits_reconciles_the_inherited_offsets_before_its_signal_shutdown`<br>`togi_session::startup::tests::a_stop_asked_for_while_the_earlier_boots_were_read_waits_for_the_reconciliation` | A |
| `TestPendingBootLoopSurvivesFailedSameBootPreflight` | `togi_session::whole::same_boot::a_pending_boot_loop_survives_a_same_boot_preflight_that_fails_and_finishes_once_it_passes` | A |
| `TestSameBootPreflightDefersNewDeadEndAction` | `togi_session::whole::same_boot::a_dead_end_a_same_boot_preflight_finds_waits_for_the_reconciliation_to_act` | A |

### `internal/session/samples_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestSampleEvidenceStalledWorker` | `togi_session::evidence::tests::only_a_unique_earliest_persistent_stall_names_a_core` | A |
| `TestRequestedVoltage` | `togi_session::evidence::tests::the_requested_voltage_is_the_loaded_cores_highest_request_by_median_and_minimum` | A |

### `internal/session/session_test.go` (11)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestKillAtEveryEvent` | `togi_session::whole::crash::killing_the_run_at_every_durable_event_resumes_to_the_same_final_state` | A |
| `TestCandidateSoloLimitsStartChecking` | `togi_session::whole::misc::configured_candidate_limits_start_checking_with_frozen_r1_r2_workloads`<br>`togi_session::begin::tests::a_configured_start_and_a_configured_candidate_override_the_baseline` | A |
| `TestCrashDuringRecovery` | `togi_session::whole::crash::a_crash_during_recovery_fails_the_original_trial_once_and_the_recovery_boot_is_stray` | A |
| `TestStrayCrashes` | `togi_session::session::tests::stray_crashes_end_in_a_boot_loop_that_exits_without_a_bootloader`<br>`togi_session::session::tests::smu_evidence_outranks_a_boot_loop`<br>`togi_session::whole::crash::fewer_stray_crashes_than_the_threshold_do_not_stop_the_run` | A |
| `TestDeadEnds` | `togi_session::write::tests::a_failed_command_is_recorded_and_is_an_smu_dead_end_with_the_verified_steps_kept`<br>`togi_session::write::tests::a_readback_that_disagrees_is_durable_then_an_smu_dead_end`<br>`togi_session::run_trial::tests::four_backend_setup_failures_wait_then_dead_end_without_evidence`<br>`togi_session::recovery::tests::containment_evidence_names_the_escaped_cpus_or_carries_the_error_until_its_dead_end`<br>`togi_session::startup::tests::failed_checks_are_a_preflight_dead_end_citing_them_and_skip_the_sweep`<br>`togi_session::startup::tests::a_changed_bios_context_is_a_preflight_dead_end`<br>`togi_session::run::tests::a_whole_session_dispatches_a_failure_at_zero_dead_end`<br>`togi_session::whole::same_boot::a_pending_zero_dead_end_finishes_on_the_same_boot_and_every_later_run_stops_at_it_cleanly`<br>`togi_session::whole::dead_end::a_dead_end_interrupted_before_the_saved_entry_clear_resumes_and_clears_it_once` | A |
| `TestCrashThenPreflightFailure` | `togi_session::whole::dead_end::a_crash_then_a_failing_preflight_records_the_crash_before_the_dead_end_and_never_twice` | A |
| `TestSignalStopsCleanly` | `togi_session::run::tests::signal_stops_cleanly_and_drains_the_completed_trial_failure`<br>`togi_session::whole::dead_end::a_signal_stop_is_a_clean_stop_so_the_next_boot_is_no_crash_and_tuning_resumes` | A |
| `TestInterruptedTrialRecordsTimeRan` | `togi_session::run_trial::tests::a_cancelled_wait_stays_interrupted_and_does_not_count_toward_the_backend` | A |
| `TestStopRestoresBaseline` | `togi_session::close::tests::a_stop_restores_the_baseline_and_ends_with_the_shutdown`<br>`togi_session::close::tests::an_smu_dead_end_leaves_the_machine_alone_and_still_records_its_shutdown`<br>`togi_session::restore::tests::the_machine_goes_back_to_the_baseline_and_the_restoration_cites_it`<br>`togi_session::restore::tests::a_failure_point_at_zero_restores_every_core_to_zero` | A |
| `TestCrashDuringRestoreKeepsTheAppliedCondition` | `togi_session::close::tests::a_crash_at_the_restoration_intent_is_classified_by_the_condition_applied_before_it`<br>`togi_session::close::tests::a_crash_during_the_restoration_keeps_the_applied_condition_and_records_no_shutdown` | A |
| `TestRunnerErrorKeepsMachineCheck` | `togi_session::run_trial::tests::a_backend_that_exits_during_setup_keeps_the_machine_check_it_logged`<br>`togi_session::run_trial::tests::evidence_survives_a_runner_error_and_the_error_stays_in_the_reason` | A |

### `internal/session/smu_interrupt_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestSameBootInterruptsEverySMURestoreWindow` | `togi_session::whole::same_boot::every_smu_access_window_of_the_same_boot_restoration_converges` | A |
| `TestEmergencyZeroingInterruptsEverySMUWindow` | `togi_session::emergency::tests::an_interruption_of_any_emergency_access_keeps_the_original_error_and_appends_nothing`<br>`togi_session::whole::same_boot::every_smu_access_window_of_the_emergency_zeroing_resumes_to_the_uninterrupted_decisions` | A |

### `internal/session/startup_test.go` (6)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestUnknownConfiguredCoreRefusesBeforeStartingSession` | `togi_session::session::tests::a_configured_core_the_machine_lacks_is_refused` | A |
| `TestCarriedStartsRespectFailurePointsAndMachineTopology` | `togi_session::begin::tests::a_carried_start_stays_one_count_shallower_than_the_failure_point`<br>`togi_session::begin::tests::a_carry_is_committed_after_the_context_and_baseline_and_before_any_phase_once`<br>`togi_carry::tests::resolution::emission_filters_to_machine_cores_and_missing_copies`<br>`togi_carry::tests::seeds::bios_change_archives_metadata_carry_available` | A |
| `TestResumedBaselineClampsInitialSearch` | `togi_session::begin::tests::a_resumed_baseline_beyond_the_range_is_clamped_where_the_search_starts` | A |
| `TestHostReadFailureDoesNotStartTuning` | `togi_session::session::tests::a_host_that_cannot_be_read_does_not_start_tuning` | A |
| `TestSessionIDFailureLeavesEmptyJournal` | `togi_session::session::tests::an_identity_that_cannot_be_allocated_leaves_the_journal_empty` | A |
| `TestIncompatibleBootClearFailureRetainsBothErrors` | `togi_session::session::tests::a_refused_journal_whose_saved_entry_cannot_be_cleared_keeps_both_errors` | A |

### `internal/session/state_warning_test.go` (3)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestStateWriteFailureWarnsContinuesAndRebuilds` | `togi_session::publication::tests::state_write_failure_warns_continues_and_rebuilds` | A |
| `TestProjectionWarningAppendFailureIsFatal` | `togi_session::publication::tests::projection_warning_append_failure_is_fatal` | A |
| `TestStateRebuildWriteFailureWarnsAndContinues` | `togi_session::publication::tests::state_rebuild_write_failure_warns_and_continues` | A |

### `internal/session/sweep_test.go` (5)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestSweepBeforeWritesStartAndResume` | `togi_session::whole::misc::every_startup_runs_preflight_then_the_sweep_before_its_first_smu_write`<br>`togi_session::startup::tests::the_sweep_hands_over_the_proof_that_nothing_runs` | A |
| `TestFailedPreflightSkipsSweep` | `togi_session::startup::tests::a_run_whose_preflight_fails_never_sweeps_writes_or_starts_a_trial`<br>`togi_session::startup::tests::failed_checks_are_a_preflight_dead_end_citing_them_and_skip_the_sweep` | A |
| `TestFailedSweepJournalFailureDoesNotWrite` | `togi_session::startup::tests::an_unconfirmed_sweep_latches_the_containment_before_its_check_is_recorded`<br>`togi_session::startup::tests::an_unconfirmed_sweep_never_ends_in_a_shutdown_or_touches_the_smu`<br>`togi_session::close::tests::the_emergency_is_withheld_without_a_validated_smu_or_after_a_fault` | A |
| `TestSameBootOpenTrialFinalizesAfterSweep` | `togi_session::startup::tests::a_same_boot_open_trial_is_ended_only_after_a_successful_sweep`<br>`togi_session::startup::tests::the_same_boot_trial_waits_for_the_boot_to_be_swept`<br>`togi_session::recover::tests::a_trial_open_in_this_boot_waits_for_the_sweep_while_earlier_boots_are_still_recovered` | A |
| `TestRecoveredFinalizationRetriesAfterInterruptedEnd` | `togi_session::recover::tests::compression_precedes_the_end_so_a_crash_between_them_leaves_the_files_to_compress_again` | A |

### `internal/session/warm_test.go` (8)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestWarmVerifyAcceptsTheStateItFolded` | `togi_session::resume::tests::a_capsule_verifies_the_state_it_folded`<br>`togi_session::resume::tests::a_verified_capsules_document_round_trips_through_json` | A |
| `TestWarmVerifyNamesWhatDiffersFromAReplay` | `togi_session::resume::tests::verify_names_the_part_that_differs`<br>`togi_session::resume::tests::the_document_check_rejects_a_document_other_than_the_cold_folds` | A |
| `TestWarmVerifyRejectsAStateThatMissesEvents` | `togi_session::resume::tests::verify_rejects_a_capsule_that_misses_events` | A |
| `TestWarmIsAdoptedOnlyWhenItCoversTheJournal` | `togi_session::resume::tests::a_capsule_is_adopted_only_when_it_covers_exactly_the_journal` | A |
| `TestCrashLeavesDeferredStateUnwrittenInWarm` | `togi_session::resume::tests::a_crash_leaves_the_state_unwritten_and_the_capsule_holding_what_the_boot_folded`<br>`togi_session::resume::tests::a_crashed_runs_capsule_publishes_what_a_live_run_had_and_the_next_boot_rebuilds_nothing` | A |
| `TestPendingStateIsTakenOnceAndWrittenWhenNotAdopted` | `togi_session::resume::tests::the_state_a_crashed_run_left_unwritten_is_published_by_the_next_boot_when_no_capsule_is_adopted` | A |
| `TestWarmStoreDropsStateTheRunnerSetOutsideTheFold` | — | O(a capsule holds only state folded from events, by type; Go runner fields outside the fold do not exist) |
| `TestDeferredStateIsWrittenBeforeAJournalOnlyAppendAndWhenTheRunEnds` | `togi_session::publication::tests::deferred_state_is_written_before_the_journal_leaves_the_run`<br>`togi_session::publication::tests::on_finish_publishes_the_state_once_after_the_final_event`<br>`togi_session::publication::tests::live_publishes_after_every_append_and_the_last_state_covers_the_final_event` | A |

### `internal/session/watchdog_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestWatchdogPreflight` | `togi_session::startup::tests::the_watchdog_wait_polls_each_second_until_armed_or_thirty_seconds_and_stops_on_a_signal_or_clock_error`<br>`togi_session::startup::tests::a_watchdog_that_never_arms_is_a_preflight_dead_end_after_thirty_seconds`<br>`togi_session::startup::tests::an_armed_watchdog_is_recorded_without_waiting`<br>`togi_session::startup::tests::a_signal_while_waiting_for_the_watchdog_stops_the_run`<br>`togi_session::session::tests::a_missing_watchdog_is_warned_about_outside_the_tuning_boot` | A |

## simrun

`internal/simrun` → `togi-simrun` (#812). `helpers_test.go` holds helpers only. The Rust crate's `split::*`, `log::*` and `simulate::tests::a_boot_that_ends_without_a_crash_is_always_checked` tests have no Go counterpart: they pin the `machine.json` continuation and the log failures that #812 fixed.

### `internal/simrun/carry_test.go` (12)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestTransitionWithUnknownKinds` | `togi_simrun::sim::carry::a_journal_with_an_unknown_kind_is_refused_untouched_by_the_build_that_wrote_it`<br>`togi_simrun::sim::carry::an_older_journal_with_an_unknown_kind_is_archived_and_carries_nothing` | A — the older-schema and schema-2 cases are obsolete (Rust has one schema), and so is carrying known events past an unknown kind (Rust refuses unknown kinds, so such a journal carries nothing) |
| `TestARulesetTransitionSeedsTheNextSession` | `togi_simrun::sim::carry::a_ruleset_transition_seeds_the_next_session` | A |
| `TestAPreviousRulesetSessionTransitionsToASeededSession` | `togi_simrun::sim::carry::a_previous_ruleset_session_transitions_to_a_seeded_session` | A |
| `TestAPreviousRulesetTransitionStartsPhaseOneWithNothingConfirmed` | `togi_simrun::sim::carry::a_ruleset_transition_starts_phase_one_with_nothing_confirmed` | A |
| `TestARulesetTransitionAfterABIOSChangeCarriesOnlySoloLimits` | `togi_simrun::sim::carry::a_ruleset_transition_after_a_bios_change_carries_only_solo_limits` | A |
| `TestBIOSChangeArchivesAndChecksSoloLimits` | `togi_simrun::sim::carry::a_bios_change_archives_and_checks_solo_limits` | A |
| `TestAConfiguredCandidateSoloLimitStopsShortOfACarriedFailurePoint` | `togi_simrun::sim::carry::a_configured_candidate_solo_limit_stops_short_of_a_carried_failure_point` | A |
| `TestACarriedFailurePointAtZeroDeadEndsTheCore` | `togi_simrun::sim::carry::a_carried_failure_point_at_zero_dead_ends_the_core` | A |
| `TestBIOSArchiveInterruptedBeforeMoveResumesCarry` | `togi_simrun::sim::carry::a_bios_archive_interrupted_before_the_move_resumes_the_carry` | A |
| `TestRulesetTransitionCarriesCulpritAndDirectHuntFailurePoints` | `togi_simrun::sim::carry::a_ruleset_transition_carries_a_culprit_hunt_failure_point`<br>`togi_simrun::sim::carry::a_ruleset_transition_carries_a_direct_hunt_failure_point` | A |
| `TestCurrentRulesetChecksCarriedSoloLimitsButCompletesFullCyclesWithLivePasses` | `togi_simrun::sim::carry::a_transition_checks_carried_solo_limits_but_completes_full_cycles_with_live_passes` | A |
| `TestCurrentRulesetStartsHuntFromCarriedTogetherFailure` | `togi_simrun::sim::carry::a_transition_starts_a_hunt_from_a_carried_together_failure` | A |

### `internal/simrun/crash_test.go` (4)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestResetReasonPowerButtonAndThermal` | `togi_simrun::sim::crash::a_power_button_reset_during_a_trial_is_a_conclusive_failure`<br>`togi_simrun::sim::crash::a_thermal_trip_reset_during_a_trial_dead_ends_without_a_failure` | A |
| `TestThermalTripWhileTrialRunningDeadEnds` | `togi_simrun::sim::crash::a_thermal_trip_while_a_trial_runs_dead_ends` | A |
| `TestPowerButtonBetweenTrialsIsInconclusive` | `togi_simrun::sim::crash::a_power_button_between_trials_is_inconclusive` | A |
| `TestClockJumpDoesNotLoseTrialMCE` | `togi_simrun::sim::crash::a_wall_clock_jump_does_not_lose_a_trials_corrected_mce` | A |

### `internal/simrun/hunt_test.go` (8)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestHuntAllZeroParkedOffsets` | `togi_simrun::sim::hunt::a_hunt_without_a_clean_cycle_parks_every_offset_at_zero` | A |
| `TestHuntCulpritAfterCleanCycleParkedOffsets` | `togi_simrun::sim::hunt::a_hunt_after_a_clean_cycle_parks_the_offsets_of_that_cycle_and_finds_the_culprit` | A |
| `TestParkedOffsetBackendFailureRaisesParkedOffsets` | `togi_simrun::sim::hunt::a_failure_at_a_held_cores_parked_offset_raises_the_parked_offsets_of_the_next_hunt` | A |
| `TestHuntCombination` | `togi_simrun::sim::hunt::a_joint_crash_makes_a_combination_instead_of_single_core_failure_points` | A |
| `TestHuntJointMisleadingMCE` | `togi_simrun::sim::hunt::a_joint_crash_with_a_misleading_core_local_mce_blames_that_core_only` | A |
| `TestDelayedHuntEscalates` | `togi_simrun::sim::hunt::a_hunt_of_a_joint_that_fails_after_six_idle_minutes_escalates`<br>`togi_simrun::sim::hunt::a_hunt_of_a_joint_that_fails_at_the_short_duration_boundary_escalates` | A |
| `TestScriptedJointAndIdleLimits` | `togi_simrun::sim::hunt::a_scripted_sharp_joint_limit_is_found_in_ranking_order`<br>`togi_simrun::sim::hunt::a_scripted_idle_only_limit_outside_the_loaded_cores_is_found_by_r1` | A |
| `TestIdleOnlyHazardReachesCleanCycle` | `togi_simrun::sim::hunt::an_idle_only_hazard_reaches_a_clean_cycle_at_seed_1`<br>`togi_simrun::sim::hunt::an_idle_only_hazard_reaches_a_clean_cycle_at_seed_2` | A |

### `internal/simrun/memory_test.go` (6)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestInMemoryJournalMatchesFileBacked` | `togi_simrun::sim::persistence::shared_voltage_1000`<br>`togi_simrun::sim::persistence::default_machine_1`<br>`togi_simrun::sim::persistence::target_fit_0_1` | A |
| `TestWarmResumeMatchesColdReplay` | `togi_simrun::sim::warm_cold::flat_hazard_102`<br>`togi_simrun::sim::warm_cold::flat_hazard_7`<br>`togi_simrun::sim::warm_cold::misleading_mce_3`<br>`togi_simrun::sim::warm_cold::shared_voltage_1000` | A |
| `TestInMemoryProjectionFailureWarnsAfterStop` | `togi_simrun::sim::memory::a_failed_projection_warns_after_the_stop_and_does_not_fail_the_session` | A |
| `TestInMemoryCrashBeforeFirstProjectionReplaysDurableStart` | `togi_simrun::sim::memory::a_crash_right_after_session_start_is_replayed_once` | A |
| `TestInMemoryResumeReadsMatchingDiskProjection` | `togi_simrun::sim::memory::a_resume_reads_the_projection_the_earlier_run_published` | A |
| `TestInMemoryInvalidConfigLeavesNoProjection` | `togi_simrun::sim::memory::an_invalid_config_begins_no_session_and_publishes_no_projection` | A |

### `internal/simrun/r7_chain_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestR7SimulationRunsFullFirstOffsetFallbackChains` | `togi_simrun::sim::r7_chain::simulation_runs_full_first_offset_fallback_chains` | A |

### `internal/simrun/simrun_test.go` (10)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestSixteenCoresReachCleanCycle` | `togi_simrun::sim::sixteen::sixteen_cores_reach_a_clean_cycle` | A |
| `TestSimulatorRefusesAnotherJournalWriter` | `togi_simrun::sim::cap::a_held_lock_refuses_the_simulation_before_any_journal_or_offset_change` | A |
| `TestBootCapLeavesPartialSessionInJournal` | `togi_simrun::sim::cap::boot_cap_leaves_a_partial_session_in_the_journal` | A |
| `TestBootCapVerifiesWarmStateOnceAndReportsFailure` | `togi_simrun::simulate::tests::a_capped_simulation_checks_each_crashed_boot_at_most_once`<br>`togi_simrun::simulate::tests::a_capped_simulation_checks_the_boots_its_cadence_names_in_either_replay`<br>`togi_simrun::simulate::tests::a_failed_check_of_the_last_boot_fails_the_simulation_instead_of_capping_it`<br>`togi_simrun::simulate::tests::a_resume_state_that_differs_from_the_journal_fails_the_simulation` | A |
| `TestBootCapWithFailedFlushIsNotErrBootCap` | `togi_simrun::sim::cap::a_cap_whose_last_state_cannot_be_published_is_an_error_not_a_cap` | A |
| `TestNewSessionTrialGetsNoSamplesFromAnEarlierInvocation` | `togi_simrun::sim::samples::a_new_sessions_trial_gets_no_samples_from_an_earlier_invocation`<br>`togi_simrun::sim::samples::a_new_in_memory_sessions_trial_gets_no_samples_from_an_earlier_one` | A |
| `TestJournalUntilCancelsAfterFirstMatchingAppend` | `togi_simrun::sim::stop::an_observer_stops_the_run_after_the_first_matching_append` | A |
| `TestRecordedConfigResumesRecordedPassesAndDurations` | `togi_simrun::sim::config::a_resume_under_the_recorded_configuration_repeats_the_recorded_trials`<br>`togi_simrun::sim::config::recorded_passes_count_only_under_the_recorded_backends` | A |
| `TestRecordedConfigWithoutJournalIsFresh` | `togi_simrun::sim::config::a_journal_without_a_recorded_configuration_gives_the_fresh_one` | A |
| `TestFirstResultStopsAfterPhaseOnesPassedCycle` | `togi_simrun::sim::first_result::the_first_result_stops_after_phase_ones_passed_cycle` | A |

### `internal/simrun/smu_matrix_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestPowerLossDuringHuntResume` | `togi_simrun::sim::interrupt::power_loss_during_hunt_resume_0`<br>`togi_simrun::sim::interrupt::power_loss_during_hunt_resume_1`<br>`togi_simrun::sim::interrupt::power_loss_during_hunt_resume_2`<br>`togi_simrun::sim::interrupt::power_loss_during_hunt_resume_3` | A |
| `TestPowerLossDuringDeepeningResume` | `togi_simrun::sim::interrupt::power_loss_during_deepening_resume_0`<br>`togi_simrun::sim::interrupt::power_loss_during_deepening_resume_1`<br>`togi_simrun::sim::interrupt::power_loss_during_deepening_resume_2`<br>`togi_simrun::sim::interrupt::power_loss_during_deepening_resume_3` | A |

## tuner simulator tests (#741)

These two `internal/tuner` files need whole simulated sessions, so #812 ports them into `togi-simrun` (`tests/sim/forecast.rs`, `tests/sim/memo.rs`): `togi-tuner` cannot take `togi-simrun` as a dev-dependency.

### `internal/tuner/forecast_sim_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestForecastMatchesSimulatedTrials` | `togi_simrun::sim::forecast::seed_1_two_cores`<br>`togi_simrun::sim::forecast::seed_7_two_cores_crash_joint`<br>`togi_simrun::sim::forecast::seed_1_default_cores_every_eighth` | A |
| `BenchmarkDashboardProjectionForecast` | — | G(no Rust benchmark of projection and forecast over a simulated journal) |

### `internal/tuner/memo_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestMemosMatchRecomputation` | `togi_simrun::sim::memo::flat_hazard_7`<br>`togi_simrun::sim::memo::target_shared_voltage_2`<br>`togi_simrun::sim::memo::target_r7_request_gap_1`<br>`togi_simrun::sim::memo::misleading_mce_3`<br>`togi_simrun::sim::memo::late_onset_1` | A |

## audit

### `tools/audit/audit_test.go` (5)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestAuditInvariants` | `togi_tool_audit::audit::avoidance::failure_points_and_combinations_are_avoided_by_applied_profiles`<br>`togi_tool_audit::audit::avoidance::the_audit_keeps_its_own_failure_history`<br>`togi_tool_audit::audit::avoidance::a_reset_clears_the_marks_once_search_restarts`<br>`togi_tool_audit::audit::avoidance::a_multi_core_r7_failure_naming_a_core_at_zero_counts_only_once_a_dead_end_cites_it`<br>`togi_tool_audit::audit::writes::a_write_follows_its_intent_and_is_read_back`<br>`togi_tool_audit::audit::writes::an_intent_authorizes_one_write_in_its_own_boot`<br>`togi_tool_audit::audit::writes::a_reboot_may_interrupt_a_write_before_its_readback`<br>`togi_tool_audit::audit::writes::a_readback_matches_the_write_it_cites`<br>`togi_tool_audit::audit::references::a_cause_names_an_existing_earlier_event`<br>`togi_tool_audit::audit::conclusion::a_finished_run_ends_in_a_conclusion`<br>`togi_tool_audit::audit::rebuild::a_rebuild_disagreeing_with_a_current_snapshot_is_a_violation`<br>`togi_tool_audit::audit::ranges::an_offset_the_tuner_chooses_cannot_leave_the_range_in_a_journal`<br>`togi_tool_audit::audit::ranges::the_hardware_reports_offsets_outside_the_range_and_the_journal_keeps_them` | A |
| `TestProjectedState` | `togi_tool_audit::audit::snapshot::a_current_state_document_equals_a_fresh_fold_of_its_journal`<br>`togi_tool_audit::audit::snapshot::a_current_document_that_disagrees_with_the_journal_is_a_violation`<br>`togi_tool_audit::audit::snapshot::a_document_from_another_sequence_is_stale_and_not_compared` | A |
| `TestDirectoryJournals` | `togi_tool_audit::audit::directory::archives_may_end_anywhere_and_only_the_live_run_must_conclude`<br>`togi_tool_audit::audit::directory::a_violation_is_attributed_to_its_own_journal` | A |
| `TestOlderRulesetSnapshot` | — | O(one native schema; a journal of another ruleset is archived before the run, so no older-ruleset snapshot reaches the audit) |
| `TestRecordLookupIgnoresPathSpelling` | — | G(bench run records looked up by `--root` are not ported; deferred with the bench to #642) |

### `tools/audit/edge_test.go` (3)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestAvoidanceHistory` | `togi_tool_audit::audit::avoidance::the_audit_keeps_its_own_failure_history`<br>`togi_tool_audit::audit::avoidance::failure_points_and_combinations_are_avoided_by_applied_profiles`<br>`togi_tool_audit::audit::avoidance::a_multi_core_r7_failure_naming_a_core_at_zero_counts_only_once_a_dead_end_cites_it`<br>`togi_tool_audit::audit::conclusion::a_finished_run_ends_in_a_conclusion` | A |
| `TestMissingCause` | `togi_tool_audit::audit::references::a_cause_names_an_existing_earlier_event` | A |
| `TestProjectionShape` | `togi_tool_audit::audit::snapshot::a_document_not_shaped_as_this_build_writes_it_is_a_violation` | A |

### `tools/audit/sweep_test.go` (5)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestTimingChecks` | — | G(wall-time checks over bench run records are not ported; deferred with the bench to #642) |
| `TestSweepSuite` | — | G(the generated bench sweep is not ported; deferred with the bench to #642) |
| `TestSweepSuiteWire` | — | G(the sweep file `tools/bench` reads is not ported; deferred with the bench to #642) |
| `TestLoadSweepSuiteRejects` | — | G(the sweep loader is not ported; `togi_tool_sim::suite::rules::invalid_suites_are_refused` pins the same decode rules for the sim suite, not for the sweep file; deferred with the bench to #642) |
| `TestUsage` | — | G(the audit command's sweep flags are not ported; `togi-tool-audit` is a library, and the sweep is deferred with the bench to #642) |

## tool sim

### `tools/sim/main_test.go` (10)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestSimRefusesInvalidInputs` | `togi_tool_sim::cli::inputs::invalid_arguments_are_usage_errors_that_touch_nothing` | A |
| `TestSimBootCapExitsCensored` | `togi_tool_sim::cli::session::the_boot_cap_ends_an_unfinished_session_with_status_three` | A |
| `TestSimInvalidMachineLeavesStateUntouched` | `togi_tool_sim::cli::inputs::an_invalid_machine_leaves_the_state_directory_untouched` | A |
| `TestSimReplayRequiresMachineBeforeStateChanges` | `togi_tool_sim::cli::inputs::replaying_facts_needs_a_machine_before_any_state_changes` | A |
| `TestSimInvalidMachineCreatesNoTemporaryState` | `togi_tool_sim::cli::inputs::invalid_input_creates_no_temporary_state` | A |
| `TestSimDefaultStateDirIsAnnouncedTemporaryDirectory` | `togi_tool_sim::cli::session::without_a_state_directory_a_temporary_one_is_kept_and_announced_first` | A |
| `TestSimSamplesAreOptIn` | `togi_tool_sim::cli::session::samples_reach_the_state_directory_only_when_asked_for` | A |
| `TestSimResumesUnderTheRecordedConfiguration` | `togi_tool_sim::cli::session::a_resume_runs_under_the_configuration_the_journal_recorded` | A |
| `TestSimTransitionsAnOlderSchemaUnderTheRecordedBackends` | — | O(one native schema, so there is no older-schema journal to transition; recorded backends are pinned by the resume test) |
| `TestBufferedLogReachesAFileOnlyWhenFlushedAndATerminalAtOnce` | `togi_tool_sim::bin::log::tests::a_buffered_log_reaches_its_stream_only_when_finished`<br>`togi_tool_sim::bin::log::tests::an_announcement_is_written_at_once` | A |
