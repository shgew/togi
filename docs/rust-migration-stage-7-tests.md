# Stage 7 Go-test mapping

Every `func Test*`, `Benchmark*`, `Fuzz*` and `Example*` in the Go packages that stage 7 (#642) ported is mapped to the Rust tests that pin the same behavior: `tools/modelcheck` → `togi-tool-modelcheck`, `tools/forecast` → `togi-tool-forecast`, `tools/facts` → `togi-tool-facts`, `tools/fit` → `togi-tool-fit`, `tools/stats` → `togi-tool-stats`, `tools/bench` → `togi-tool-bench` and `cmd/togi` → `togi`. `tools/audit` → `togi-tool-audit` is listed too: stage 4 ported its journal audit and stage 7 its sweep. There are no Benchmark, Fuzz or Example functions. Go is the statement of intent, not an oracle: a row is **O** where the Go test pinned a Go format or behavior the native contract replaced.

The pull requests that ported each package are #841 (modelcheck), #844 (forecast), #832 (facts), #852 (fit), #851 (stats), #857 (bench runs, metrics, comparison and cache), #862 (bench gate reports), #866 (the bench's simulator check), #872 (`--same`, `--smoke` and `--forecast` of the bench), #878 (the bench's slim baseline projection, `--baseline-out`), #861 (`togi` argument parsing, exits, version and completion), #869 (`reset`, `restart-limit`, `doctor`), #870 (the `tools/audit` sweep), #879 (`status`, `events` and `watch`, closing issue #826) and #880 (`run`, the dashboard owner and panic handling, closing issue #829). Each row follows the Go-to-Rust mapping of the pull request that ported its command; a `cmd/togi` test that no mapping names is assigned by the command it drives. Every row is settled: none is pending.

Rust names are `crate::module::fn`; an integration test binary adds its name after the crate (`togi_tool_bench::gate::…`, `togi::read::status::…`, `togi::run::lifecycle::…`), and a unit test of the library has no binary segment (`togi::table::tests::…`). Each name resolves to a test that `cargo nextest list --workspace` reports on `main` at d1a4dd63; a parameterised `#[test_case]` function counts by its name. One exception: `togi::run::pty::a_session_panic_over_the_real_linux_console_dashboard_restores_the_cursor_and_the_palette` was added by the closure review after d1a4dd63 and is not in that listing.

Status:

- **A**: a Rust test pins the behavior. Where the port changed a detail deliberately (clap's wording of a usage error, one `command.reset` without a `shutdown`, an unknown event kind refused in read-only words), the row maps the tests that pin the native behavior of the same intent.
- **O(reason)**: obsolete under the native contract; the reason says what the Go test pinned and what replaced it, and the Rust tests listed, if any, pin the replacement.
- **S4**: ported in stage 4; the row and its status are in `docs/rust-migration-stage-4-tests.md`, and its tests are repeated here.

## Summary

| Package | Go tests | A | O | S4 |
|---|---:|---:|---:|---:|
| modelcheck | 11 | 11 | 0 | 0 |
| forecast | 12 | 12 | 0 | 0 |
| facts | 3 | 3 | 0 | 0 |
| fit | 37 | 35 | 2 | 0 |
| stats | 27 | 26 | 1 | 0 |
| bench | 84 | 83 | 1 | 0 |
| audit | 13 | 6 | 0 | 7 |
| togi | 118 | 113 | 5 | 0 |
| **total** | **305** | **289** | **9** | **7** |

## modelcheck

### `tools/modelcheck/check_regression_test.go` (6)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestModelCheckResetWeights` | `togi_tool_modelcheck::modelcheck::a_machine_with_an_indecisive_reset_is_refused_and_a_zero_weight_changes_nothing` | A |
| `TestModelCheckEligibility` | `togi_tool_modelcheck::modelcheck::the_machines_declared_context_limits_which_facts_are_checked`<br>`togi_tool_modelcheck::modelcheck::trials_of_other_depths_make_other_groups_each_needing_ten`<br>`togi_tool_modelcheck::modelcheck::a_trial_the_extract_holds_for_another_context_is_not_validated` | A |
| `TestCheckRefusesUnreadableExtract` | `togi_tool_modelcheck::modelcheck::an_unreadable_extract_is_refused_with_its_path`<br>`togi_tool_modelcheck::modelcheck::a_machine_without_an_extract_is_refused` | A |
| `TestCheckRecordsRefusesInvalidMachine` | `togi_tool_modelcheck::modelcheck::an_invalid_machine_is_refused_before_its_extract_is_read` | A |
| `TestCheckerRefusesMalformedTrials` | `togi_tool_modelcheck::modelcheck::a_trial_that_does_not_fit_the_machine_is_refused_by_its_place_in_the_extract` | A |
| `TestCheckerCountsIdleWithoutInventingExposure` | `togi_tool_modelcheck::modelcheck::idle_failures_are_counted_without_inventing_exposure` | A |

### `tools/modelcheck/check_test.go` (5)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestBinomialInterval` | `togi_tool_modelcheck::interval::tests::known_intervals_hold_at_the_boundaries_of_p`<br>`togi_tool_modelcheck::interval::tests::the_interval_is_ordered_inside_n_and_holds_the_mean`<br>`togi_tool_modelcheck::interval::tests::a_count_is_inside_the_interval_exactly_when_p_is_inside_its_bounds`<br>`togi_tool_modelcheck::interval::tests::log_choose_matches_exact_binomial_coefficients`<br>`togi_tool_modelcheck::interval::tests::bounds_of_a_count_at_either_end_are_open_on_that_side` | A |
| `TestModelGrouping` | `togi_tool_modelcheck::modelcheck::a_group_is_keyed_by_the_shallowest_loaded_offset_and_idle_cores_do_not_count`<br>`togi_tool_modelcheck::modelcheck::idle_failures_are_counted_without_inventing_exposure` | A |
| `TestFixtureModelCheck` | `togi_tool_modelcheck::modelcheck::a_fixture_machine_is_judged_against_its_own_extract`<br>`togi_tool_modelcheck::modelcheck::the_report_names_each_flagged_group_and_nothing_for_no_checks` | A |
| `TestCheckerConstraintsPreserveBinomialCheck` | `togi_tool_modelcheck::modelcheck::a_checker_accepts_exactly_what_its_report_calls_ok`<br>`togi_tool_modelcheck::interval::tests::a_count_is_inside_the_interval_exactly_when_p_is_inside_its_bounds` | A |
| `TestReportOmitsAbsentChecks` | `togi_tool_modelcheck::modelcheck::the_report_names_each_flagged_group_and_nothing_for_no_checks` | A |

## forecast

### `tools/forecast/forecast_test.go` (12)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestAfter` | `togi_tool_forecast::tests::after::nothing_after_the_anchor_is_a_censored_run_at_the_session_profile`<br>`togi_tool_forecast::tests::after::events_before_the_anchor_are_excluded`<br>`togi_tool_forecast::tests::after::a_hunt_answered_only_by_carried_facts_runs_no_trial`<br>`togi_tool_forecast::tests::after::a_run_concludes_at_a_cycles_shutdown`<br>`togi_tool_forecast::tests::after::nothing_after_a_cycles_shutdown_counts`<br>`togi_tool_forecast::tests::after::a_session_after_the_terminal_point_need_not_be_replayable`<br>`togi_tool_forecast::tests::after::a_dead_end_ends_the_run_before_a_later_conclusion`<br>`togi_tool_forecast::tests::after::a_new_session_without_a_terminal_point_supplies_the_profile`<br>`togi_tool_forecast::tests::after::a_cycle_start_after_a_clean_cycle_is_the_terminal_point_and_is_not_counted`<br>`togi_tool_forecast::tests::after::the_same_hunt_number_in_another_session_is_another_hunt` | A |
| `TestAfterRequiresAReplayableNewestSession` | `togi_tool_forecast::tests::after::an_unreplayable_newest_session_cannot_project_a_profile`<br>`togi_tool_forecast::tests::after::an_unreplayable_session_holding_a_cycle_start_is_not_concluded_by_it` | A |
| `TestAfterScoresAnEndlessRunAsACyclesOneRun` | `togi_tool_forecast::simulated::an_endless_run_scores_as_the_cycles_one_run_though_it_crashes_after_concluding`<br>`togi_tool_forecast::simulated::a_run_that_goes_on_past_its_conclusion_scores_as_the_cycles_one_run_with_the_same_history` | A |
| `TestReadDirRefusesAnotherRuleset` | `togi_tool_forecast::tests::files::a_journal_of_another_ruleset_reads_as_one_session_that_cannot_be_replayed`<br>`togi_tool_forecast::tests::score::a_score_refuses_sessions_with_another_ruleset_at_its_terminal_session`<br>`togi_tool_forecast::tests::after::a_newer_schema_with_this_builds_ruleset_is_refused_for_its_schema` | A |
| `TestSummarize` | `togi_tool_forecast::tests::summary::an_odd_count_has_the_middle_run_as_its_median`<br>`togi_tool_forecast::tests::summary::an_even_count_averages_the_middle_two`<br>`togi_tool_forecast::tests::summary::censored_and_dead_end_runs_count_everywhere_but_hours`<br>`togi_tool_forecast::tests::summary::without_a_concluded_run_hours_have_no_range` | A |
| `TestPercentilesUseNearestRank` | `togi_tool_forecast::tests::summary::percentiles_use_the_nearest_rank` | A |
| `TestScore` | `togi_tool_forecast::tests::score::a_concluded_run_with_the_forecasts_build_is_placed_against_the_ensemble`<br>`togi_tool_forecast::tests::score::a_censored_run_bounds_hours_crashes_and_hunts`<br>`togi_tool_forecast::tests::score::another_build_is_flagged`<br>`togi_tool_forecast::tests::score::a_concluded_value_below_the_10th_percentile_is_placed_below`<br>`togi_tool_forecast::tests::score::a_censored_bound_below_the_10th_percentile_is_open_not_below` | A |
| `TestScoreBuildsCoverTheScoredSpan` | `togi_tool_forecast::tests::score::builds_cover_the_scored_span` | A |
| `TestScoreWithoutConcludedRuns` | `togi_tool_forecast::tests::score::without_concluded_runs_hours_are_unforecast` | A |
| `TestScoreRefusesAnotherCoreCount` | `togi_tool_forecast::tests::score::a_run_over_other_cores_than_the_forecast_is_refused`<br>`togi_tool_forecast::tests::score::a_run_over_other_cores_than_the_forecast_is_refused_though_their_count_matches` | A |
| `TestScoreFlagsRulesetAndDirtyForecast` | `togi_tool_forecast::tests::score::a_dirty_forecast_and_another_ruleset_or_evidence_epoch_are_flagged` | A |
| `TestScoreRefusesMissingOrChangedAnchor` | `togi_tool_forecast::tests::score::a_session_the_anchor_does_not_name_or_that_changed_is_refused`<br>`togi_tool_forecast::tests::score::the_error_messages_name_the_session_and_event` | A |

## facts

### `tools/facts/generate_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestGenerateReportsDestinationFailuresAndRemovesTemporaryFiles` | `togi_tool_facts::facts::generate::a_missing_output_directory_fails_without_creating_anything`<br>`togi_tool_facts::facts::generate::a_directory_in_place_of_the_output_is_not_replaced_and_leaves_no_temporary_file` | A |

### `tools/facts/main_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestGeneratePreservesOutputOnSourceError` | `togi_tool_facts::facts::generate::a_source_that_yields_no_facts_leaves_the_output_untouched`<br>`togi_tool_facts::facts::cli::a_failed_extraction_exits_one_names_the_tool_on_stderr_and_keeps_the_output` | A |
| `TestGenerateReplacesOutputAfterSuccess` | `togi_tool_facts::facts::generate::success_replaces_the_output_with_the_extracted_facts` | A |

## fit

### `tools/fit/anchor_test.go` (4)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestSharedVoltageAnchorDispatchAndConflictingFlags` | `togi_tool_fit::fit::cli::flags_that_do_not_go_together_are_usage_errors`<br>`togi_tool_fit::fit::cli::an_unreadable_extract_exits_1_with_the_error_on_stderr`<br>`togi_tool_fit::fit::cli::the_help_lists_the_modes` | A |
| `TestSharedVoltageAnchorCheckedDeterministicAndReplayable` | `togi_tool_fit::tests::anchor::an_anchor_is_checked_deterministic_and_replayable_from_its_own_provenance` | A |
| `TestSharedVoltageAnchorFlaggedFitNeverWrites` | `togi_tool_fit::tests::anchor::a_flagged_anchor_prints_its_scores_and_never_writes` | A |
| `TestSharedVoltageAnchorValidatesBeforeFitting` | `togi_tool_fit::tests::anchor::facts_the_anchor_cannot_replay_are_refused_before_any_fit` | A |

### `tools/fit/fit_idle_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestFitIdlePreservesTotalLikelihood` | `togi_tool_fit::tests::search::the_idle_limit_keeps_the_total_likelihood_from_worsening` | A |
| `TestFitIdleRequiresNegativeUnloadedExposure` | `togi_tool_fit::tests::search::an_idle_limit_needs_a_negative_offset_on_an_unloaded_core_to_be_fitted` | A |

### `tools/fit/fit_test.go` (13)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestFitRecoversMachine` | `togi_tool_fit::tests::search::a_fit_recovers_the_limits_hazard_shape_and_ccd_hazard_of_the_machine_that_drew_the_trials` | A |
| `TestBootstrapDeterminism` | `togi_tool_fit::tests::evidence::the_same_seed_resamples_the_same_trials_and_another_seed_others`<br>`togi_tool_fit::fit::determinism::the_same_input_and_seed_give_byte_identical_output_on_every_pool_size` | A |
| `TestMixedContextRejected` | `togi_tool_fit::tests::evidence::evidence_of_two_bios_contexts_is_refused_whichever_comes_first` | A |
| `TestFitRecoversFlatAndIdle` | `togi_tool_fit::tests::search::a_fit_recovers_a_flat_rate_and_an_idle_limit` | A |
| `TestConstrainedFitUsesResampledLikelihood` | `togi_tool_fit::tests::search::a_constrained_refit_maximizes_the_resampled_likelihood_inside_the_original_evidence` | A |
| `TestConstrainedFitRefitsCCDWithoutChangingR7Structure` | `togi_tool_fit::tests::search::a_constrained_refit_refits_the_ccd_hazard_and_keeps_the_r7_structure` | A |
| `TestLikelihoodRejectsImpossibleOutcomes` | `togi_tool_fit::tests::search::an_outcome_the_model_calls_impossible_costs_infinity_and_a_certain_one_costs_nothing` | A |
| `TestDecisiveRefusesInvalidEvidence` | `togi_tool_fit::tests::evidence::a_fact_that_describes_no_machine_exposure_is_refused` | A |
| `TestDecisiveFiltersNonTrialsAndPreservesContext` | `togi_tool_fit::tests::evidence::idle_failures_are_left_out_and_the_context_is_kept` | A |
| `TestJointSearchSeparatesCleanBoundary` | `togi_tool_fit::tests::search::the_joint_search_separates_a_clean_boundary_from_a_failing_one` | A |
| `TestJointCandidateLimitPreservesModel` | `togi_tool_fit::tests::search::a_ninth_joint_on_a_ccd_is_never_added_but_an_eighth_is` | A |
| `TestWorkloadOverridesRequireSupportAndImproveLikelihood` | `togi_tool_fit::tests::search::a_workload_override_needs_ten_trials_and_a_better_likelihood` | A |
| `TestCoupledShiftIncludesWorkloadAndPreservesUnsupportedLimits` | `togi_tool_fit::tests::search::the_coupled_shift_moves_workload_limits_and_leaves_unsupported_ones` | A |

### `tools/fit/forward_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestForwardCheck` | `togi_tool_fit::tests::forward::a_held_out_session_is_scored_by_a_fit_of_the_sessions_before_it_only`<br>`togi_tool_fit::tests::forward::sessions_with_a_numeric_suffix_are_ordered_by_the_number`<br>`togi_tool_fit::tests::forward::a_sealed_session_is_neither_fitted_nor_scored`<br>`togi_tool_fit::tests::forward::the_log_loss_clamps_the_probability_and_the_constant_predictor_is_scored_alongside` | A |
| `TestReportForwardRegimeLogLoss` | `togi_tool_fit::tests::forward::the_regime_rows_show_mean_log_loss_in_regime_order` | A |

### `tools/fit/main_test.go` (7)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestEncodeMachineRetainsFitEvidenceAndParameters` | `togi_tool_fit::tests::generate::an_encoded_machine_retains_the_fit_evidence_and_parameters` | A |
| `TestCommittedMachineNotesKeepOnlyProvenance` | — | O(it pinned the note prefixes of the committed Go-written machine files re-encoded with Go's encoder; those files are not regenerated, and the Rust notes are pinned by `an_encoded_machine_retains_the_fit_evidence_and_parameters` and the anchor tests) |
| `TestGenerateWritesCheckedReproducibleEnsemble` | `togi_tool_fit::tests::generate::an_ensemble_is_written_checked_against_the_original_evidence_and_reproducible` | A |
| `TestGenerateFlaggedMembersMatchSerial` | `togi_tool_fit::tests::generate::the_ensemble_report_and_files_are_the_same_on_one_thread_and_on_many` | A |
| `TestGenerateRefusesMissingOrNondecisiveEvidence` | `togi_tool_fit::tests::generate::evidence_with_no_decisive_trial_is_refused_before_anything_is_written` | A |
| `TestGenerateRefusesConflictingDestinations` | `togi_tool_fit::tests::generate::a_destination_that_conflicts_is_refused_and_left_as_it_was` | A |
| `TestRunForwardOnly` | `togi_tool_fit::fit::cli::forward_only_prints_the_check_and_writes_no_machine_files`<br>`togi_tool_fit::fit::cli::sealing_every_held_out_session_is_an_error`<br>`togi_tool_fit::fit::cli::flags_that_do_not_go_together_are_usage_errors` | A |

### `tools/fit/parallel_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestFitParallelKeepsIndexOrder` | `togi_tool_fit::fit::determinism::the_same_input_and_seed_give_byte_identical_output_on_every_pool_size`<br>`togi_tool_fit::fit::determinism::the_shared_voltage_anchor_is_byte_identical_on_every_pool_size` | A |
| `TestFitParallelBoundsWorkersAndReturnsFirstFailure` | — | O(`fitParallel` is gone: rayon bounds the workers, every fit runs to completion and the lowest-index error is reported, so no fit step on valid evidence can fail to test it) |

### `tools/fit/signals_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestReportSignalsNamesFittedCounts` | `togi_tool_fit::tests::signals::the_signal_report_names_the_failure_counts_pooled_and_per_regime` | A |
| `TestFitSignalsWithoutSignaledFailuresResetsToDefaultWeights` | `togi_tool_fit::tests::signals::without_any_failure_the_mix_is_the_default_whatever_the_machine_held` | A |

### `tools/fit/voltage_test.go` (5)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestSharedVoltageRequestRegression` | `togi_tool_fit::tests::voltage::the_request_regression_recovers_the_count_and_clock_slopes_and_the_clock_medians` | A |
| `TestSharedVoltageNamedFailureLikelihood` | `togi_tool_fit::tests::voltage::a_named_core_failure_pins_the_hazard_to_the_named_core` | A |
| `TestSharedVoltageFitDeterminismAndIsolation` | `togi_tool_fit::tests::voltage::the_shared_voltage_fit_is_deterministic_bounded_and_leaves_its_input_alone` | A |
| `TestSharedVoltageBackgroundExplainsStockFailure` | `togi_tool_fit::tests::voltage::a_failure_at_stock_offsets_is_explained_by_a_bounded_background_rate` | A |
| `TestConstrainedSharedRatePreservesVoltageGeometry` | `togi_tool_fit::tests::voltage::the_constrained_rate_search_keeps_the_voltage_geometry_and_refuses_incompatible_groups` | A |

## stats

### `tools/stats/forecast_test.go` (4)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestScoreResumedSimulatedJournal` | `togi_tool_stats::tests::score::a_concluded_run_is_scored_against_the_ensemble`<br>`togi_tool_stats::tests::score::a_run_that_has_not_concluded_is_scored_with_lower_bounds`<br>`togi_tool_stats::tests::score::a_forecast_without_a_concluded_run_has_no_hours_to_score_against`<br>`togi_tool_stats::tests::score::a_state_directory_is_scored_from_the_anchor_through_the_forecast_file`<br>`togi_tool_stats::stats::cli::a_forecast_scores_the_state_directory_instead_of_reporting` | A |
| `TestRenderScoreRecordedScores` | `togi_tool_stats::tests::score::a_concluded_run_is_scored_against_the_ensemble`<br>`togi_tool_stats::tests::score::a_run_that_has_not_concluded_is_scored_with_lower_bounds`<br>`togi_tool_stats::tests::score::a_forecast_without_a_concluded_run_has_no_hours_to_score_against`<br>`togi_tool_stats::tests::score::flags_name_what_makes_the_forecast_less_comparable` | A |
| `TestScoreRefusesChangedAnchor` | `togi_tool_stats::tests::score::a_state_directory_whose_journal_through_the_anchor_changed_is_refused` | A |
| `TestForecastRejectsJournalAndSince` | `togi_tool_stats::stats::cli::rejected_arguments_exit_two_without_a_report` | A |

### `tools/stats/known_failure_skip_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestKnownFailureSkipDoesNotRecountIdleFailure` | `togi_tool_stats::tests::evidence::a_known_failure_skip_does_not_recount_an_idle_failure_or_invalidate_a_later_pass` | A |

### `tools/stats/main_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestStatsArgumentErrors` | `togi_tool_stats::stats::cli::rejected_arguments_exit_two_without_a_report` | A |
| `TestStatsReadsOnlyJournal` | `togi_tool_stats::stats::cli::the_report_reads_only_the_journal_and_leaves_it_as_it_was` | A |

### `tools/stats/r7_decisions_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestReportR7BackoffsAndChains` | `togi_tool_stats::tests::scenario::r7_backoffs_and_chains_are_listed_with_their_causes_and_sources` | A |

### `tools/stats/regression_test.go` (9)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestRecoveryGapUsesLastEvidenceAfterWallJump` | `togi_tool_stats::tests::recovery::the_recovery_gap_runs_from_the_last_evidence_whatever_the_wall_clock_did` | A |
| `TestCrashMetricsPreserveStrongerSignal` | `togi_tool_stats::tests::recovery::a_crash_keeps_its_stronger_signal_in_every_metric` | A |
| `TestRerunsGroupRecordedObligation` | `togi_tool_stats::tests::hunts::reruns_group_by_the_obligation_that_recorded_them` | A |
| `TestCheckingStepsListRerunsApart` | `togi_tool_stats::tests::hunts::checking_steps_list_reruns_apart_from_the_cycles` | A |
| `TestHuntCommitmentRequiresRecordedCause` | `togi_tool_stats::tests::hunts::a_hunt_commits_only_to_a_backoff_that_cites_its_end_or_combination`<br>`togi_tool_stats::tests::hunts::a_decision_other_than_a_backoff_is_no_commitment` | A |
| `TestHuntParkedOffsets` | `togi_tool_stats::tests::hunts::a_hunt_names_how_its_offsets_were_parked`<br>`togi_tool_stats::tests::hunts::an_open_hunt_runs_to_the_journals_last_event` | A |
| `TestIdleCrashInvalidatesAllCoreR6PriorPasses` | `togi_tool_stats::tests::evidence::an_idle_crash_invalidates_the_passes_of_an_all_core_r6_target_it_is_no_deeper_than` | A |
| `TestSingleCarriedFailureDecisions` | `togi_tool_stats::tests::evidence::a_decision_resting_on_one_carried_failure_is_counted` | A |
| `TestSingleCarriedFailureThroughHunt` | `togi_tool_stats::tests::evidence::a_chain_through_hunt_events_is_followed_to_its_failures` | A |

### `tools/stats/report_test.go` (4)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestReportReturnsEarlierSectionWriteError` | `togi_tool_stats::stats::cli::a_report_that_cannot_be_written_exits_one_and_names_the_write` | A |
| `TestReviewBoundaryReport` | `togi_tool_stats::tests::scenario::the_review_boundary_scenario_keeps_the_evidence_a_cutoff_hides`<br>`togi_tool_stats::tests::scenario::the_review_boundary_report_before_and_after_a_cutoff` | A |
| `TestReviewEvenRecoveryMedian` | `togi_tool_stats::tests::recovery::an_even_number_of_gaps_has_the_mean_of_the_middle_two_as_its_median`<br>`togi_tool_stats::tests::recovery::a_gap_summary_is_ordered_and_counts_every_gap` | A |
| `TestCrashTimingBoundaries` | `togi_tool_stats::tests::recovery::crashes_are_binned_by_the_trial_seconds_before_their_last_evidence` | A |

### `tools/stats/requests_test.go` (3)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestR7RequestReport` | `togi_tool_stats::tests::requests::r7_trials_are_counted_by_top_request_bin_and_by_top_requester` | A |
| `TestR7RequestsRecoveredFromSamples` | — | O(a Rust `trial.end` carries the telemetry its session summarized from the samples, and the tool reads no samples) |
| `TestR7RequestsIncludeCarriedFacts` | `togi_tool_stats::tests::requests::carried_facts_count_once_per_source_and_only_with_both_request_fields` | A |

### `tools/stats/stats_test.go` (3)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestSimulatedCombinationReport` | `togi_tool_stats::tests::scenario::a_combination_failure_is_reported_in_every_section`<br>`togi_tool_stats::stats::simulated::a_simulated_sessions_report_agrees_with_its_events` | A |
| `TestRunGrouping` | `togi_tool_stats::tests::runs::a_crash_reboot_continues_the_run_until_a_shutdown`<br>`togi_tool_stats::tests::runs::a_shutdown_splits_the_boots_into_runs`<br>`togi_tool_stats::tests::runs::a_restart_within_a_boot_starts_a_run_that_stays_open` | A |
| `TestPriorPasses` | `togi_tool_stats::tests::evidence::prior_passes_count_what_is_as_deep_and_not_ruled_out_by_a_failure` | A |

## bench

### `tools/bench/anchor_overlays_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestAnchorOverlaysDifferFromAnchorOnlyByTheirMechanism` | `togi_tool_bench::suite::the_anchor_overlays_differ_from_the_anchor_only_by_their_mechanism` | A |

### `tools/bench/baseline_compat_test.go` (3)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestReadResultsIgnoresRetiredFields` | `togi_tool_bench::records::a_record_is_an_object_with_exactly_the_fields_the_format_names`<br>`togi_tool_bench::sanity::go_scores_ignore_the_fields_the_rust_bench_does_not_compare` | O(inverted on purpose: the Rust record refuses an unknown key such as Go's `partial_seconds`, pinned by the first test; only the reader of the committed Go scores tolerates them, pinned by the second) |
| `TestComparisonIgnoresProjectedFields` | `togi_tool_bench::compare::a_baseline_file_compares_exactly_as_the_full_records_do`<br>`togi_tool_bench::gate::a_baseline_file_is_judged_exactly_as_the_full_records_are`<br>`togi_tool_bench::run::the_baseline_file_holds_the_runs_records_without_wall_times_and_compares_as_they_do` | A |
| `TestCommittedBaselineIsSlim` | `togi_tool_bench::records::the_baseline_writer_leaves_out_the_model_check_and_the_wall_time_and_nothing_else`<br>`togi_tool_bench::records::a_baseline_file_round_trips_through_the_reader`<br>`togi_tool_bench::compare::a_baseline_file_compares_exactly_as_the_full_records_do` | A |

`TestCommittedBaselineIsSlim` pins the committed Go file `tools/bench/baseline.jsonl`, which #878 leaves untouched and which stays a Go test until the Go bench is deleted (stage 9, #644). The Rust tests listed pin the slim projection that `--baseline-out` writes; the committed file has not migrated to a Rust recording.

### `tools/bench/bench_test.go` (6)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestMetrics` | `togi_tool_bench::measure::a_hand_built_journal_measures_every_field_exactly`<br>`togi_tool_bench::measure::a_journal_cut_before_any_passed_cycle_has_no_first_passed_cycle`<br>`togi_tool_bench::measure::a_concluded_session_counts_its_cycles_trials_and_depth`<br>`togi_tool_bench::measure::a_journal_with_no_events_measures_the_zero_profile`<br>`togi_tool_bench::measure::the_other_regimes_hazards_are_in_the_record`<br>`togi_tool_bench::measure::measuring_twice_gives_the_same_numbers` | A |
| `TestPairing` | `togi_tool_bench::compare::runs_pair_by_scenario_and_seed_in_scenario_then_seed_order`<br>`togi_tool_bench::compare::a_baseline_run_that_changed_split_pairs_and_is_not_counted_unmatched`<br>`togi_tool_bench::compare::unmatched_runs_in_the_candidates_splits_are_warned_about` | A |
| `TestVerdict` | `togi_tool_bench::compare::the_verdict_follows_the_violations_and_the_time_interval`<br>`togi_tool_bench::compare::a_rejected_candidate_says_which_violations`<br>`togi_tool_bench::compare::one_deeper_pair_among_six_is_a_pair_violation_but_not_a_mean_one`<br>`togi_tool_bench::compare::a_slower_target_is_a_violation_and_no_pairs_is_neutral`<br>`togi_tool_bench::compare::a_hazard_rise_of_exactly_the_threshold_is_not_a_violation` | A |
| `TestScenarioWeightedBootstrap` | `togi_tool_bench::compare::the_time_ratio_weights_scenarios_equally_and_the_bootstrap_is_fixed` | A |
| `TestRunStatus` | `togi_tool_bench::records::a_run_status_follows_the_exit_status_and_what_the_session_left`<br>`togi_tool_bench::run::a_status_follows_the_exit_status_and_the_log` | A |
| `TestBootCapRunIsCensoredCost` | `togi_tool_bench::measure::a_boot_capped_session_is_censored_with_its_crashes_and_time_so_far`<br>`togi_tool_bench::run::a_status_follows_the_exit_status_and_the_log` | A |

### `tools/bench/cache_test.go` (9)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestCacheKey` | `togi_tool_bench::cache::a_runs_inputs_bind_the_suite_the_run_and_every_file_of_its_machine`<br>`togi_tool_bench::cache::flipping_replay_changes_the_inputs`<br>`togi_tool_bench::cache::equal_files_in_another_directory_give_the_same_inputs`<br>`togi_tool_bench::cache::the_tree_key_binds_both_binaries_and_the_boot_cap`<br>`togi_tool_bench::cache::the_bench_hashes_its_own_executable`<br>`togi_tool_bench::cache::a_declared_facts_extract_is_an_input`<br>`togi_tool_bench::same::tests::the_digest_key_changes_with_the_simulator_and_the_boot_cap`<br>`togi_tool_bench::same::tests::a_session_s_inputs_change_with_its_identity_and_replay`<br>`togi_tool_bench::same::tests::a_session_s_inputs_change_with_any_file_of_its_machine` | A |
| `TestRunInputsHashEveryMachineFile` | `togi_tool_bench::cache::every_file_an_overlay_extends_is_an_input`<br>`togi_tool_bench::run::a_changed_machine_file_misses_the_cache_and_another_run_hits` | A |
| `TestSessionCacheMissesWhenOnlyOverlayGrandparentChanges` | `togi_tool_bench::cache::an_equal_overlay_chain_in_another_tree_hits_and_only_its_grandparent_changing_misses` | A |
| `TestSessionCache` | `togi_tool_bench::cache::a_record_comes_back_only_for_the_inputs_it_was_made_from`<br>`togi_tool_bench::cache::an_entry_for_another_run_or_a_damaged_one_is_a_miss`<br>`togi_tool_bench::run::a_second_run_over_the_same_inputs_comes_from_the_cache`<br>`togi_tool_bench::run::a_changed_suite_file_misses_the_cache`<br>`togi_tool_bench::run::another_boot_cap_misses_the_cache`<br>`togi_tool_bench::run::another_simulator_binary_misses_the_cache`<br>`togi_tool_bench::run::a_failed_cache_write_is_a_warning_and_the_records_are_kept`<br>`togi_tool_bench::run::keeping_the_runs_skips_the_cache_and_leaves_their_directories`<br>`togi_tool_bench::same::tests::a_digest_record_holds_only_for_the_inputs_that_made_it`<br>`togi_tool_bench::same::tests::a_corrupt_digest_record_misses` | A |
| `TestOpenSessionCachePrunesLeastRecentlyUsed` | `togi_tool_bench::cache::opening_a_cache_keeps_the_seven_most_recently_used_others`<br>`togi_tool_bench::cache::reopening_a_cache_marks_it_recently_used` | A |
| `TestCostTable` | `togi_tool_bench::cache::costs_are_kept_per_scope_with_the_latest_run_as_the_fallback`<br>`togi_tool_bench::cache::costs_round_trip_through_their_file_and_a_damaged_file_is_empty`<br>`togi_tool_bench::cache::a_cost_that_is_not_a_positive_finite_time_is_not_kept`<br>`togi_tool_bench::cache::sessions_start_longest_first_and_the_unknown_ones_before_them` | A |
| `TestCostTableSurvivesScopeWithoutWall` | `togi_tool_bench::cache::costs_survive_a_scope_that_has_none_and_keep_the_eight_latest_scopes` | A |
| `TestTouchDirRecreatesDirRemovedByPruning` | `togi_tool_bench::cache::tests::a_directory_pruned_before_it_is_marked_is_created_again`<br>`togi_tool_bench::cache::tests::a_directory_pruned_on_every_attempt_fails_after_the_last` | A |
| `TestRunPool` | `togi_tool_bench::pool::tests::every_index_runs_once_in_the_given_order_by_one_worker`<br>`togi_tool_bench::pool::tests::many_workers_run_each_index_exactly_once`<br>`togi_tool_bench::pool::tests::the_first_error_stops_new_starts_and_errors_come_back_by_index`<br>`togi_tool_bench::pool::tests::errors_are_sorted_by_index_whatever_the_order_they_occurred_in`<br>`togi_tool_bench::run::the_first_failure_stops_the_running_simulators_and_is_what_the_run_reports` | A |

### `tools/bench/forecast_test.go` (12)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestForecastFromSimulatedCopy` | `togi_tool_bench::forecast::a_forecast_counts_only_what_follows_the_anchor_and_leaves_the_copy_alone` | A |
| `TestForecastReportRendersRecordedForecasts` | `togi_tool_bench::forecast::the_report_names_the_anchor_the_runs_by_status_and_each_metric_s_range` | A |
| `TestForecastFailsOnFailedRun` | `togi_tool_bench::forecast::a_run_that_fails_or_does_not_conclude_fails_the_forecast` | A |
| `TestForecastRejectsDeadEndBeforeAnchor` | `togi_tool_bench::forecast::a_dead_end_the_copy_already_holds_is_not_the_runs_own`<br>`togi_tool_bench::forecast::exit_one_is_a_dead_end_only_when_this_run_reached_one` | A |
| `TestCopyStateCopiesSessionBearingArchive` | `togi_tool_bench::forecast::only_the_files_a_resumed_session_reads_are_copied` | A |
| `TestCopyStateRejectsIrregularArchiveEntry` | `togi_tool_bench::forecast::an_archive_entry_that_is_not_a_regular_file_fails_naming_it` | A |
| `TestFailedForecastKeepsOnlyFailedRuns` | `togi_tool_bench::forecast::a_failed_forecast_keeps_only_the_failed_runs_and_says_where`<br>`togi_tool_bench::forecast::a_forecast_that_fails_after_every_run_succeeded_keeps_nothing`<br>`togi_tool_bench::forecast::an_interrupt_stops_starting_runs_and_fails_as_interrupted`<br>`togi_tool_bench::forecast::a_forecast_that_fails_to_write_its_record_leaves_no_run_directory_behind`<br>`togi_tool_bench::forecast::an_interrupted_forecast_leaves_no_run_directory_behind`<br>`togi_tool_bench::forecast::the_command_says_where_the_runs_of_a_failed_forecast_are_kept` | A |
| `TestRelativeToResolvesMixedPaths` | `togi_tool_bench::forecast::a_machine_the_suite_names_by_absolute_path_is_named_relative_to_the_suite`<br>`togi_tool_bench::forecast::a_relative_suite_naming_its_machine_by_absolute_path_names_it_relative_to_the_suite` | A |
| `TestForecastReportNamesMachinesRelativeToSuite` | `togi_tool_bench::forecast::a_machine_the_suite_names_by_absolute_path_is_named_relative_to_the_suite` | A |
| `TestEnsembleFilesListEveryMachineFile` | `togi_tool_bench::forecast::the_record_lists_each_machine_file_the_ensemble_reads_once_by_suite_relative_path` | A |
| `TestLoadRunsSharesMachineFiles` | `togi_tool_bench::suite::a_machine_shared_by_runs_is_read_once` | A |
| `TestForecastRejectsConflictingFlags` | `togi_tool_bench::forecast::bench_flags_that_judge_a_run_are_usage_errors_with_forecast`<br>`togi_tool_bench::forecast::the_command_prints_the_forecast_and_writes_the_record` | A |

### `tools/bench/gate_archive_test.go` (3)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestArchivedRuleset11Gate` | `togi_tool_bench::gate::the_archived_go_gate_is_kept_unchanged_and_never_re_scored`<br>`togi_tool_bench::gate_cli::a_gate_of_the_go_bench_stops_the_run_before_any_session` | A |
| `TestCommittedSuiteRegistersNoGate` | `togi_tool_bench::gate::the_committed_suite_registers_no_gate` | A |
| `TestCommittedBaselineIsRuleset11` | `togi_tool_bench::gate::the_committed_go_baseline_covers_every_run_of_the_suite` | A |

### `tools/bench/gate_cli_integration_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestGateCLIIntegration` | `togi_tool_bench::gate_cli::a_registered_gate_judges_the_run_against_the_baseline_and_sets_the_exit`<br>`togi_tool_bench::gate_cli::a_suite_with_a_gate_and_no_baseline_prints_no_verdict_and_exits_0`<br>`togi_tool_bench::gate_cli::a_gate_of_the_go_bench_stops_the_run_before_any_session` | A |

### `tools/bench/gate_test.go` (11)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestGateQuantileNoise` | `togi_tool_bench::gate::a_quantile_fails_only_when_its_interval_lies_above_zero` | A |
| `TestGateQuantileIsDifferenceOfQuantiles` | `togi_tool_bench::gate::the_change_is_the_difference_of_quantiles_not_their_pairwise_quantile` | A |
| `TestGatePooledMedianStrictlyLower` | `togi_tool_bench::gate::the_pooled_median_must_fall_strictly` | A |
| `TestGateConclusion` | `togi_tool_bench::gate::a_lost_conclusion_fails_and_a_baseline_without_one_cannot_lose_it` | A |
| `TestGateTimeBound` | `togi_tool_bench::gate::the_time_ratio_may_equal_but_not_exceed_its_bound`<br>`togi_tool_bench::gate::the_time_ratio_is_the_geometric_mean_over_pairs` | A |
| `TestGateRefusals` | `togi_tool_bench::gate::the_gate_refuses_what_it_cannot_judge_and_scores_nothing`<br>`togi_tool_bench::gate::a_missing_run_on_both_sides_is_named_as_both` | A |
| `TestGateExitsByVerdict` | `togi_tool_bench::gate::the_verdict_passes_only_when_every_criterion_does`<br>`togi_tool_bench::gate_cli::a_registered_gate_judges_the_run_against_the_baseline_and_sets_the_exit` | A |
| `TestGateBaselineIdentity` | `togi_tool_bench::gate::a_baseline_may_mix_the_commits_the_gate_names_and_no_others` | A |
| `TestGateBootstrapReproducible` | `togi_tool_bench::gate::the_bootstrap_interval_is_reproducible_and_pinned`<br>`togi_tool_bench::gate::the_bootstrap_interval_does_not_depend_on_the_number_of_threads`<br>`togi_tool_bench::gate::a_single_pair_has_a_point_interval_and_no_pairs_a_zero_one` | A |
| `TestGateReportGolden` | `togi_tool_bench::gate::the_reports_of_a_passing_failing_and_refused_gate`<br>`togi_tool_bench::gate::the_report_prints_the_gates_note_after_its_header`<br>`togi_tool_bench::gate::the_header_names_the_split_the_gate_is_judged_on` | A |
| `TestLoadGate` | `togi_tool_bench::gate::an_invalid_gate_is_refused_with_its_reason`<br>`togi_tool_bench::gate::a_gate_that_leaves_a_scenario_without_seeds_is_refused`<br>`togi_tool_bench::gate::a_valid_gate_resolves_the_seeds_of_its_split`<br>`togi_tool_bench::gate::a_suite_without_a_gate_registers_none`<br>`togi_tool_bench::gate::a_gate_decodes_from_an_object_only` | A |

### `tools/bench/main_test.go` (3)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestLoadRunsRejectsInvalidSuite` | `togi_tool_bench::suite::an_invalid_suite_is_refused_with_the_reason`<br>`togi_tool_bench::suite::a_missing_suite_file_is_a_read_error` | A |
| `TestReadResults` | `togi_tool_bench::records::records_round_trip_through_a_file_one_per_line`<br>`togi_tool_bench::records::a_malformed_line_names_its_number`<br>`togi_tool_bench::records::a_missing_file_is_a_read_error`<br>`togi_tool_bench::records::an_empty_file_and_blank_lines_hold_no_records`<br>`togi_tool_bench::records::blank_lines_between_records_are_skipped`<br>`togi_tool_bench::records::a_run_listed_twice_is_refused_even_across_splits`<br>`togi_tool_bench::records::a_record_is_refused_in_the_positional_array_form`<br>`togi_tool_bench::records::absent_optional_measurements_are_omitted_or_null_as_the_format_says`<br>`togi_tool_bench::records::the_first_passed_cycle_is_required_but_may_be_null` | A |
| `TestLoadRunsMarksSmokeSessions` | `togi_tool_bench::suite::smoke_seeds_mark_their_runs` | A |

### `tools/bench/replay_test.go` (3)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestRealAnswerMetrics` | `togi_tool_bench::measure::only_started_decisive_trials_matching_a_real_fact_are_real_answers`<br>`togi_tool_bench::measure::trials_a_real_fact_answers_are_counted_against_the_trials_that_ended` | A |
| `TestRealAnswerShares` | `togi_tool_bench::compare::real_answer_shares_pool_over_the_scenario_and_the_pairs` | A |
| `TestTargetSuite` | `togi_tool_bench::suite::the_target_scenario_spreads_two_seeds_per_member_over_nine_replayed_fits`<br>`togi_tool_bench::suite::an_ensemble_picks_members_by_position_within_each_split`<br>`togi_tool_bench::run::a_replayed_scenario_names_its_machine_and_asks_for_the_facts` | A |

### `tools/bench/report_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestBenchReports` | `togi_tool_bench::compare::the_comparison_names_each_scenario_and_the_verdict`<br>`togi_tool_bench::compare::the_summary_lists_scenarios_in_record_order_then_the_total`<br>`togi_tool_bench::run::the_report_lists_the_summary_for_each_scenario_and_the_cache_line` | A |

### `tools/bench/same_test.go` (21)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestNormalizeBuild` | `togi_tool_bench::same::tests::normalizing_removes_only_the_build_stamp_of_a_session_start`<br>`togi_tool_bench::same::tests::normalizing_takes_the_description_out_of_both_config_messages`<br>`togi_tool_bench::same::tests::normalizing_leaves_every_other_event_byte_for_byte`<br>`togi_tool_bench::same::tests::a_stamped_event_that_does_not_decode_is_an_error` | A |
| `TestBuildComparisonPreservesDecisions` | `togi_tool_bench::same::tests::a_build_only_difference_is_not_a_difference`<br>`togi_tool_bench::same::tests::the_first_difference_is_the_first_line_that_differs`<br>`togi_tool_bench::same::a_build_stamp_alone_is_not_a_difference_and_a_decision_is` | A |
| `TestFirstJournalDifference` | `togi_tool_bench::same::tests::the_first_difference_is_the_first_line_that_differs` | A |
| `TestCompareJournals` | `togi_tool_bench::same::tests::directories_differ_in_the_journal_one_side_lacks_but_not_in_other_files`<br>`togi_tool_bench::same::tests::a_side_without_journals_or_events_cannot_be_compared` | A |
| `TestSameReport` | `togi_tool_bench::same::tests::the_report_names_each_differing_session_and_the_sessions_one_tree_lacks` | A |
| `TestSameExitCodesAndSplits` | `togi_tool_bench::same::tests::a_session_is_identified_by_its_split_and_judged_by_its_exit_status`<br>`togi_tool_bench::same::the_command_exits_zero_for_equal_trees_and_one_for_a_difference` | A |
| `TestSameKeepsOnlyDifferingRuns` | `togi_tool_bench::same::tests::only_differing_sessions_keep_their_runs_unless_asked_to_keep_all` | A |
| `TestSameRemovesMatchingRunsBeforeNextPair` | `togi_tool_bench::same::tests::a_matching_session_is_removed_before_the_next_one_starts` | A |
| `TestSameKeepsFailedPairs` | `togi_tool_bench::same::tests::a_failed_run_stops_the_comparison_and_leaves_its_own_diagnostics`<br>`togi_tool_bench::same::a_failed_session_exits_one_and_names_the_only_directory_it_left_in_the_temporary_directory` | A |
| `TestSameRejectsMissingJournals` | `togi_tool_bench::same::tests::sessions_without_journals_or_events_never_compare_equal`<br>`togi_tool_bench::same::a_simulator_that_leaves_no_journal_fails_the_comparison_and_keeps_the_runs` | A |
| `TestSameRejectsEmptyJournals` | `togi_tool_bench::same::tests::sessions_without_journals_or_events_never_compare_equal` | A |
| `TestSameRejectsExplicitBenchFlags` | `togi_tool_bench::same::bench_flags_that_judge_a_run_are_usage_errors_with_same` | A |
| `TestJournalDigest` | `togi_tool_bench::same::tests::the_digest_names_every_journal_and_ignores_the_build_and_other_files`<br>`togi_tool_bench::same::tests::no_journals_or_only_empty_ones_have_no_digest` | A |
| `TestSameCachesBothSides` | `togi_tool_bench::same::tests::both_sides_are_cached_and_a_fresh_run_ignores_the_cache`<br>`togi_tool_bench::same::a_second_comparison_of_the_same_binaries_runs_nothing_again` | A |
| `TestSameRerunsOnlyTheSideItNeedsToShowADifference` | `togi_tool_bench::same::tests::a_difference_reruns_only_the_sides_that_have_no_run_to_show` | A |
| `TestSameStopsAtTheFirstDifference` | `togi_tool_bench::same::tests::the_comparison_stops_at_the_first_difference_unless_told_to_go_on` | A |
| `TestSameInterruptRemovesRuns` | `togi_tool_bench::same::tests::an_interrupt_removes_every_run_and_reports_nothing`<br>`togi_tool_bench::same::a_termination_during_the_build_fails_the_command_instead_of_reporting_equality` | A |
| `TestSameKeepsJournalsOfCachedSidesOfDifferingSessions` | `togi_tool_bench::same::tests::a_differing_session_keeps_the_journals_of_its_cached_sides` | A |
| `TestSameFailFastReportsOneDifferenceWhenSessionsRace` | `togi_tool_bench::same::tests::sessions_that_race_to_a_difference_report_one_when_stopping_at_the_first` | A |
| `TestSameCancelledRerunLeavesNothingBehind` | `togi_tool_bench::same::tests::a_rerun_cancelled_for_an_interrupt_leaves_its_directory_nowhere` | A |
| `TestSameReportsCPUTimeWithoutFailing` | `togi_tool_bench::same::tests::the_time_of_the_compared_sessions_is_reported_without_failing` | A |

### `tools/bench/shared_voltage_test.go` (6)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestWorstR7HazardPartial` | `togi_tool_bench::metrics::tests::a_partial_load_more_hazardous_than_its_ccd_is_the_worst_including_idle_hazard`<br>`togi_tool_bench::metrics::tests::the_worst_comes_from_a_partial_of_the_second_ccd` | A |
| `TestR7PartialChainRecomputesRequestsAndTies` | `togi_tool_bench::metrics::tests::the_chain_asks_the_model_for_the_requests_of_every_shrunken_load`<br>`togi_tool_bench::metrics::tests::the_partial_chain_drops_one_tie_group_at_a_time_down_to_two_cores`<br>`togi_tool_bench::metrics::tests::a_tie_is_never_split_to_reach_a_two_core_load`<br>`togi_tool_bench::metrics::tests::all_cores_within_the_tie_leave_nothing` | A |
| `TestR7PartialDropsTopOfNegativeRequests` | `togi_tool_bench::metrics::tests::negative_requests_still_shed_their_top_group` | A |
| `TestWorstR7HazardCCDsAllCoreAndWorkloads` | `togi_tool_bench::metrics::tests::the_worst_comes_from_the_all_core_load_under_power_pressure`<br>`togi_tool_bench::metrics::tests::the_worst_comes_from_the_last_r7_workload_alone`<br>`togi_tool_bench::measure::the_worst_r7_hazard_covers_at_least_every_all_core_load` | A |
| `TestWorstR7OldBaselineCompatibility` | `togi_tool_bench::measure::a_machine_that_is_not_sixteen_cores_has_no_worst_r7_hazard`<br>`togi_tool_bench::measure::a_machine_without_a_shared_rail_has_no_worst_r7_hazard`<br>`togi_tool_bench::compare::a_missing_worst_r7_hazard_is_unavailable_not_zero` | A |
| `TestWorstR7Reports` | `togi_tool_bench::compare::shared_voltage_hazards_have_their_own_lines` | A |

### `tools/bench/suite_cleanup_test.go` (4)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestDeletedScenariosAreGone` | `togi_tool_bench::suite::the_committed_suite_has_the_runs_and_scenarios_it_documents`<br>`togi_tool_bench::suite::every_machine_of_the_committed_suite_is_valid_and_lists_its_files`<br>`togi_tool_bench::sanity::the_committed_go_baseline_is_the_whole_suite_without_deleted_scenarios` | A |
| `TestDefaultSuiteRunCounts` | `togi_tool_bench::suite::the_committed_suite_has_the_runs_and_scenarios_it_documents`<br>`togi_tool_bench::suite::runs_come_in_suite_order_with_dev_seeds_before_holdout_seeds` | A |
| `TestNonmemberIsSyntheticConclusionGuardOnly` | `togi_tool_bench::suite::the_committed_nonmember_scenario_is_a_replayed_synthetic_guard_with_one_smoke_seed` | A |
| `TestReportSuiteNotes` | `togi_tool_bench::suite::the_registered_gates_notes_are_the_suites_notes`<br>`togi_tool_bench::run::the_report_prints_the_suites_notes_before_the_summary_and_none_without_a_gate` | A |

## audit

### `tools/audit/audit_test.go` (5)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestAuditInvariants` | `togi_tool_audit::audit::avoidance::failure_points_and_combinations_are_avoided_by_applied_profiles`<br>`togi_tool_audit::audit::avoidance::the_audit_keeps_its_own_failure_history`<br>`togi_tool_audit::audit::avoidance::a_reset_clears_the_marks_once_search_restarts`<br>`togi_tool_audit::audit::avoidance::a_multi_core_r7_failure_naming_a_core_at_zero_counts_only_once_a_dead_end_cites_it`<br>`togi_tool_audit::audit::writes::a_write_follows_its_intent_and_is_read_back`<br>`togi_tool_audit::audit::writes::an_intent_authorizes_one_write_in_its_own_boot`<br>`togi_tool_audit::audit::writes::a_reboot_may_interrupt_a_write_before_its_readback`<br>`togi_tool_audit::audit::writes::a_readback_matches_the_write_it_cites`<br>`togi_tool_audit::audit::references::a_cause_names_an_existing_earlier_event`<br>`togi_tool_audit::audit::conclusion::a_finished_run_ends_in_a_conclusion`<br>`togi_tool_audit::audit::rebuild::a_rebuild_disagreeing_with_a_current_snapshot_is_a_violation`<br>`togi_tool_audit::audit::ranges::an_offset_the_tuner_chooses_cannot_leave_the_range_in_a_journal`<br>`togi_tool_audit::audit::ranges::the_hardware_reports_offsets_outside_the_range_and_the_journal_keeps_them` | S4 |
| `TestProjectedState` | `togi_tool_audit::audit::snapshot::a_current_state_document_equals_a_fresh_fold_of_its_journal`<br>`togi_tool_audit::audit::snapshot::a_current_document_that_disagrees_with_the_journal_is_a_violation`<br>`togi_tool_audit::audit::snapshot::a_document_from_another_sequence_is_stale_and_not_compared` | S4 |
| `TestDirectoryJournals` | `togi_tool_audit::audit::directory::archives_may_end_anywhere_and_only_the_live_run_must_conclude`<br>`togi_tool_audit::audit::directory::a_violation_is_attributed_to_its_own_journal` | S4 |
| `TestOlderRulesetSnapshot` | — | S4 |
| `TestRecordLookupIgnoresPathSpelling` | `togi_tool_audit::audit::sweep::a_directory_is_audited_with_the_run_it_belongs_to_whatever_its_spelling`<br>`togi_tool_audit::audit::sweep::a_directory_is_audited_with_its_run_whatever_dots_its_path_has`<br>`togi_tool_audit::command::a_run_directory_is_found_whatever_dots_its_root_is_spelled_with`<br>`togi_tool_audit::command::a_retained_sweep_is_audited_again_from_its_records` | A |

### `tools/audit/edge_test.go` (3)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestAvoidanceHistory` | `togi_tool_audit::audit::avoidance::the_audit_keeps_its_own_failure_history`<br>`togi_tool_audit::audit::avoidance::failure_points_and_combinations_are_avoided_by_applied_profiles`<br>`togi_tool_audit::audit::avoidance::a_multi_core_r7_failure_naming_a_core_at_zero_counts_only_once_a_dead_end_cites_it`<br>`togi_tool_audit::audit::conclusion::a_finished_run_ends_in_a_conclusion` | S4 |
| `TestMissingCause` | `togi_tool_audit::audit::references::a_cause_names_an_existing_earlier_event` | S4 |
| `TestProjectionShape` | `togi_tool_audit::audit::snapshot::a_document_not_shaped_as_this_build_writes_it_is_a_violation` | S4 |

### `tools/audit/sweep_test.go` (5)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestTimingChecks` | `togi_tool_audit::audit::sweep::timing_flags_a_run_over_ten_times_its_scenario_median`<br>`togi_tool_audit::audit::sweep::timing_names_the_run_directory_and_its_median` | A |
| `TestSweepSuite` | `togi_tool_audit::audit::sweep::the_sweep_suite_is_dev_only_over_seeds_one_to_n_with_absolute_machines`<br>`togi_tool_audit::command::a_sweep_runs_the_bench_keeps_its_runs_and_audits_them` | A |
| `TestSweepSuiteWire` | `togi_tool_audit::audit::sweep::the_sweep_suite_is_dev_only_over_seeds_one_to_n_with_absolute_machines`<br>`togi_tool_audit::command::a_sweep_runs_the_bench_keeps_its_runs_and_audits_them` | A |
| `TestLoadSweepSuiteRejects` | `togi_tool_audit::audit::sweep::a_suite_that_cannot_be_swept_is_refused`<br>`togi_tool_audit::command::a_missing_suite_file_fails_before_running_the_bench` | A |
| `TestUsage` | `togi_tool_audit::command::help_goes_to_stdout_and_a_bad_flag_exits_two` | A |

## togi

### `cmd/togi/carried_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestCarriedEventsLeaveStatusUnchanged` | `togi::read::status::carried_facts_add_self_sufficiency_evidence_but_no_live_regime_exposure`<br>`togi::read::events::kind_filters_select_the_carried_facts_they_name` | A |

### `cmd/togi/cli_text_test.go` (9)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestHelpExamplesShareOneDescriptionColumn` | `togi::cli::tests::examples_start_every_description_in_one_column`<br>`togi::cli::tests::every_help_examples_block_shares_one_description_column` | A |
| `TestCyclesExampleWordedOnce` | `togi::cli::the_cycles_example_is_worded_the_same_in_the_top_level_and_run_help` | A |
| `TestFlagErrorsSpellFlagsWithTwoDashes` | `togi::cli::tests::usage_errors_exit_2_with_stderr_only` | O(it pinned the rewording of Go's `flag` messages; clap words its own, pinned by the usage-error test listed) |
| `TestFlagErrorsPreserveQuotedValues` | — | O(the same rewording, whose parser could mistake a submitted value for its own text; clap's errors carry the value as data) |
| `TestConfigOnCommandsWithoutConfigIsAUsageError` | `togi::cli::tests::config_is_rejected_with_the_scope_error_wherever_it_is_written`<br>`togi::cli::tests::the_scope_error_shows_the_usage_of_the_command_it_names`<br>`togi::args::tests::config_is_rejected_where_it_is_not_attached`<br>`togi::cli::usage_errors_exit_2_on_stderr_with_nothing_on_stdout`<br>`togi::read::commands::config_is_a_usage_error_before_or_after_the_command_name` | A |
| `TestStatusFitsEightyColumns` | `togi::read::status::status_fits_eighty_columns` | A |
| `TestWrapLines` | `togi::table::tests::text_wraps_at_the_width_and_continues_under_the_rest_prefix`<br>`togi::table::tests::wide_text_wraps_by_terminal_cells`<br>`togi::table::tests::a_word_wider_than_the_room_overflows_instead_of_splitting` | A |
| `TestStatusHuntLabelsAccumulatedPassEvidence` | `togi::read::status::scenarios_render_the_status`<br>`togi::read::status::pass_evidence_above_need_is_shown_uncapped` | A |
| `TestStatusWideTablesWrapWithoutDroppingValues` | `togi::table::tests::a_row_wider_than_the_width_wraps_without_dropping_a_value`<br>`togi::table::tests::a_wide_cell_pads_to_its_cells_not_its_characters`<br>`togi::table::tests::wide_decision_notes_wrap_under_their_row_within_the_width` | A |

### `cmd/togi/compat_test.go` (14)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestReadCommandsHandleIncompatibleJournal` | `togi::read::events::a_missing_journal_is_an_error_naming_the_file`<br>`togi::read::events::a_file_that_is_not_a_journal_is_an_error`<br>`togi::read::events::a_newer_schema_is_refused_with_advice`<br>`togi::read::events::an_older_schema_is_refused_too`<br>`togi::read::events::a_different_ruleset_warns_and_still_renders`<br>`togi::read::status::a_missing_journal_is_an_error_naming_the_file`<br>`togi::read::status::a_journal_without_a_session_is_an_error` | A |
| `TestReadCommandsStyleSchemaRefusalInJournal` | `togi::read::commands::a_schema_refusal_is_red_bold_priority_3_when_stderr_is_the_system_journal` | A |
| `TestResetAllArchivesUnrecognizedSchema` | `togi::reset::reset_all_of_an_unrecognized_schema_archives_it_unread` | A |
| `TestResetCoreRefusesDifferentRuleset` | `togi::reset::reset_core_refuses_every_stamp_mismatch_with_exit_16_and_appends_nothing` | A |
| `TestResetCoreExitCodeForEveryStampMismatch` | `togi::reset::reset_core_refuses_every_stamp_mismatch_with_exit_16_and_appends_nothing` | A |
| `TestRunChecksCompatibilityBeforeConfig` | `togi::run::lifecycle::a_journal_with_a_newer_evidence_epoch_is_refused_before_the_config_is_read`<br>`togi::run::lifecycle::an_unknown_event_kind_in_a_current_journal_is_refused_before_the_config_naming_the_kind`<br>`togi::cli::run_refuses_a_journal_from_a_newer_evidence_epoch_before_the_config_and_appends_nothing` | A |
| `TestRunStillRejectsInvalidConfigForCompatibleJournal` | `togi::run::lifecycle::an_invalid_config_is_a_usage_error_for_a_compatible_journal`<br>`togi::cli::run_with_an_invalid_config_is_a_usage_error` | A |
| `TestIncompatibleArchiveRefusesCollisionAndResumesPendingMove` | `togi::reset::an_incompatible_archive_refuses_a_collision_and_resumes_a_pending_move` | A |
| `TestResetAllCompletesRenamedIncompatibleArchive` | `togi::reset::reset_all_completes_an_interrupted_incompatible_archive` | A |
| `TestResetAllArchivesDifferentRulesetNormally` | `togi::reset::reset_all_of_another_ruleset_archives_it_normally` | A |
| `TestResetAllCompletesRecordedOldSchemaArchive` | `togi::reset::a_journal_that_already_records_its_archive_is_only_moved` | A |
| `TestUnknownKindsBeforeRunConfigAndReset` | `togi::reset::a_current_journal_with_an_unknown_kind_exits_16_and_is_left_alone`<br>`togi::reset::an_unknown_kind_keeps_a_pending_carry_and_refuses_an_older_ruleset_too`<br>`togi::run::lifecycle::an_unknown_event_kind_in_a_current_journal_is_refused_before_the_config_naming_the_kind`<br>`togi::run::lifecycle::an_unknown_event_kind_in_the_tuning_boot_leaves_its_own_reason_and_clears_the_saved_entry`<br>`togi::run::lifecycle::an_unknown_kind_in_an_older_journal_does_not_refuse_the_run_before_the_config_is_read` | A |
| `TestCommandsWithFutureKind` | `togi::reset::a_current_journal_with_an_unknown_kind_exits_16_and_is_left_alone`<br>`togi::doctor::an_unknown_event_kind_in_a_current_journal_exits_16_naming_it`<br>`togi::doctor::the_unknown_kind_refusal_is_red_bold_on_a_terminal_like_the_stamp_refusal`<br>`togi::run::lifecycle::an_unknown_event_kind_in_a_current_journal_is_refused_before_the_config_naming_the_kind`<br>`togi::read::status::an_event_of_an_unknown_kind_is_refused_in_read_only_words_by_status`<br>`togi::read::events::an_event_of_an_unknown_kind_is_refused_in_read_only_words_by_events`<br>`togi::read::watch::an_event_of_an_unknown_kind_is_refused_in_read_only_words_by_watch` | A |
| `TestReadCommandsRefuseHistoricalSchemaFixture` | `togi::read::events::an_older_schema_is_refused_too`<br>`togi::read::events::a_newer_schema_is_refused_with_advice` | A |

### `cmd/togi/completion_integration_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestNushellCompletionLoads` | `togi::cli::nushell_loads_the_script_without_warnings` | A |

### `cmd/togi/completion_test.go` (3)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestCompletionScripts` | `togi::cli::bash_completion`<br>`togi::cli::zsh_completion`<br>`togi::cli::fish_completion`<br>`togi::cli::nushell_completion` | A |
| `TestCompletionCoversCommands` | `togi::completion::tests::every_script_completes_the_command_and_its_shared_flag`<br>`togi::cli::tests::the_definition_is_consistent`<br>`togi::completion::tests::the_shells_are_the_four_the_spec_lists`<br>`togi::completion::tests::only_the_commands_that_read_the_configuration_offer_it` | A |
| `TestCompletionExitCodes` | `togi::cli::tests::usage_errors_exit_2_with_stderr_only`<br>`togi::cli::tests::a_bad_shell_lists_the_supported_ones`<br>`togi::cli::a_missing_shell_error_names_the_argument`<br>`togi::cli::tests::state_dir_before_or_after_the_command_is_accepted`<br>`togi::cli::completion_help` | A |

### `cmd/togi/config_flags_test.go` (4)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestCommandHelpConfigScope` | `togi::args::tests::config_is_accepted_before_and_after_the_command_name_it_is_attached_to`<br>`togi::cli::tests::a_leading_config_moves_after_the_command_name`<br>`togi::cli::tests::other_arguments_are_left_as_they_are`<br>`togi::args::tests::config_defaults_and_reports_whether_it_was_given`<br>`togi::args::tests::the_config_help_names_the_default_path`<br>`togi::cli::only_doctor_and_reset_list_config`<br>`togi::completion::tests::only_the_commands_that_read_the_configuration_offer_it` | A |
| `TestReadOnlyCommandsRejectConfig` | `togi::cli::tests::config_is_rejected_with_the_scope_error_wherever_it_is_written`<br>`togi::cli::tests::the_scope_error_shows_the_usage_of_the_command_it_names`<br>`togi::args::tests::config_is_rejected_where_it_is_not_attached`<br>`togi::cli::usage_errors_exit_2_on_stderr_with_nothing_on_stdout`<br>`togi::read::commands::config_is_a_usage_error_before_or_after_the_command_name`<br>`togi::restart_limit::the_state_directory_flag_is_accepted_after_the_command_and_config_never` | A |
| `TestWriteCommandsAcceptConfigPlacements` | `togi::args::tests::config_is_accepted_before_and_after_the_command_name_it_is_attached_to`<br>`togi::cli::tests::a_leading_config_moves_after_the_command_name`<br>`togi::cli::tests::other_arguments_are_left_as_they_are`<br>`togi::args::tests::config_defaults_and_reports_whether_it_was_given`<br>`togi::args::tests::the_config_help_names_the_default_path`<br>`togi::completion::tests::only_the_commands_that_read_the_configuration_offer_it`<br>`togi::doctor::config_is_accepted_before_and_after_the_command`<br>`togi::reset::config_is_accepted_before_and_after_the_command`<br>`togi::run::lifecycle::the_config_flag_may_precede_the_command_name`<br>`togi::cli::run_reads_its_config_wherever_the_flag_is_written` | A |
| `TestLeadingConfigRejectionPrecedesCommandArguments` | `togi::cli::tests::config_is_rejected_with_the_scope_error_wherever_it_is_written`<br>`togi::cli::tests::the_scope_error_shows_the_usage_of_the_command_it_names`<br>`togi::args::tests::config_is_rejected_where_it_is_not_attached`<br>`togi::cli::usage_errors_exit_2_on_stderr_with_nothing_on_stdout`<br>`togi::read::commands::the_config_error_comes_before_the_arguments_of_the_command` | A |

### `cmd/togi/dashboard_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestDashboardEventVisibility` | `togi::dashboard::tests::event_lines_are_dropped_only_while_the_dashboard_shows`<br>`togi::run::lifecycle::the_dashboard_hides_event_lines_and_the_clean_stop_prints_the_restoration_and_shutdown` | A |
| `TestDashboardPanicFallsBackToEventLines` | `togi::dashboard::tests::a_panic_while_drawing_falls_back_to_event_lines_at_once_not_when_the_dashboard_is_hidden`<br>`togi::run::panics::a_panic_in_the_dashboard_falls_back_to_event_lines_while_the_run_goes_on`<br>`togi::run::panics::a_dashboard_that_gives_way_between_the_restoration_and_the_shutdown_shows_each_closing_line_once`<br>`togi::run::panics::a_dashboard_that_gives_way_after_the_last_event_shows_both_closing_lines_once_in_order` | A |

### `cmd/togi/deadline_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestRebootTimeout` | `togi_hardware::reboot::tests::asks_systemd_for_a_plain_reboot_within_thirty_seconds` | A |

### `cmd/togi/doctor_test.go` (4)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestDoctorOutput` | `togi::doctor::a_ready_machine_prints_the_table_and_ready`<br>`togi::doctor::a_failed_check_names_itself_and_exits_15`<br>`togi::doctor::a_changed_context_is_a_warning_not_a_failure`<br>`togi::doctor::without_root_it_takes_no_lock_and_says_what_it_skipped` | A |
| `TestDoctorExitCodes` | `togi::doctor::a_failed_check_names_itself_and_exits_15`<br>`togi::doctor::a_journal_run_would_refuse_exits_16_before_any_lock_or_check`<br>`togi::doctor::an_unreadable_journal_exits_1`<br>`togi::doctor::a_configuration_error_exits_2`<br>`togi::doctor::a_busy_host_lock_exits_3_only_for_root`<br>`togi::doctor::a_host_lock_that_cannot_be_opened_exits_1`<br>`togi::doctor::an_unprivileged_failed_check_exits_15_and_another_platform_exits_1`<br>`togi::doctor::a_failed_diagnosis_exits_1`<br>`togi::doctor::a_bios_context_that_cannot_be_read_follows_the_table_with_exit_1`<br>`togi::doctor::another_platform_stops_before_the_journal_the_lock_and_every_check`<br>`togi::cli::doctor_refuses_a_newer_journal_with_exit_16_before_any_check`<br>`togi::cli::doctor_exits_1_for_an_unreadable_journal_and_2_for_a_bad_config` | A |
| `TestDoctorPassesRecordedContext` | `togi::doctor::the_recorded_context_reaches_the_checks_only_for_a_journal_run_would_continue`<br>`togi::doctor::a_current_journal_before_its_session_context_passes_no_context` | A |
| `TestDoctorNotReadyStyle` | `togi::doctor::the_failed_style_follows_the_terminal_and_the_environment` | A |

### `cmd/togi/epoch_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestRunRefusesNewerEvidenceEpochBeforeConfig` | `togi::run::lifecycle::a_journal_with_a_newer_evidence_epoch_is_refused_before_the_config_is_read`<br>`togi::cli::run_refuses_a_journal_from_a_newer_evidence_epoch_before_the_config_and_appends_nothing` | A |

### `cmd/togi/events_test.go` (7)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestEvents` | `togi::read::events::filters_narrow_the_readable_lines`<br>`togi::read::events::a_time_window_is_inclusive_since_and_exclusive_until`<br>`togi::read::events::json_prints_the_journal_lines_unchanged` | A |
| `TestEventsJSONUncoloredInSystemJournal` | `togi::read::events::human_lines_are_styled_for_the_system_journal_and_json_is_not` | A |
| `TestEventsErrors` | `togi::read::events::flag_value_errors_are_usage_errors`<br>`togi::read::events::a_valid_filter_with_no_match_prints_nothing_and_exits_zero` | A |
| `TestEventsKindValidation` | `togi::read::events::kind_errors_are_usage_errors_before_the_journal_is_read` | A |
| `TestEventsKindQueries` | `togi::read::events::filters_narrow_the_readable_lines`<br>`togi::read::events::a_valid_filter_with_no_match_prints_nothing_and_exits_zero` | A |
| `TestEventsUnfilteredUnknownKind` | `togi::read::events::an_event_of_an_unknown_kind_is_refused_in_read_only_words_by_events` | A |
| `TestReadCommandsReportTornTail` | `togi::read::events::a_torn_tail_is_reported_after_the_events` | A |

### `cmd/togi/hostlock_test.go` (9)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestResetRefusesHostLock` | `togi::reset::a_busy_host_lock_exits_3_naming_it_before_anything_changes` | A |
| `TestResetBelowProtectedSharedAncestorHonorsHostLock` | `togi_hostlock::policy::tests::directory_authorization_matrix` | O(the lock path is `System::host_lock`, not a flag, so `togi` has no ancestor case of its own; the ancestor rules are pinned by `togi-hostlock`, listed) |
| `TestRunRefusesHostLockBeforeHardwareAndCarry` | `togi::run::lifecycle::a_busy_host_lock_stops_the_run_before_hardware_and_any_event`<br>`togi::run::lifecycle::a_busy_journal_lock_is_exit_three`<br>`togi::run::lifecycle::the_host_lock_is_taken_before_the_boot_id_is_read_and_the_hardware_opened`<br>`togi::run::lifecycle::the_host_lock_is_taken_before_the_journals_lock_as_reset_does` | A |
| `TestRunRefusesUnsafeHostLockBeforeHardwareAndCarry` | `togi::run::lifecycle::a_host_lock_that_is_not_safe_is_refused_before_hardware` | A |
| `TestStartupRefusalDoesNotClearGRUBWithoutHostLock` | `togi::run::lifecycle::a_tuning_boot_refusal_does_not_touch_the_boot_entry_without_the_host_lock`<br>`togi::run::lifecycle::a_tuning_boot_refusal_does_not_touch_the_boot_entry_when_the_host_lock_is_not_safe` | A |
| `TestRunRefusesIdentityBeforeBIOSOrSMUAccess` | `togi::run::lifecycle::the_bios_context_is_not_read_from_a_machine_whose_identity_is_not_validated`<br>`togi::run::lifecycle::the_host_lock_is_taken_before_the_boot_id_is_read_and_the_hardware_opened` | A |
| `TestRunReportsConstructionError` | `togi::run::lifecycle::a_hardware_that_cannot_be_opened_is_reported_as_an_error` | A |
| `TestRunOpensJournalBeforeShowingDashboard` | `togi::run::lifecycle::the_dashboard_starts_after_the_journal_is_open`<br>`togi::run::lifecycle::a_journal_that_cannot_be_opened_never_shows_the_dashboard`<br>`togi::run::lifecycle::a_journal_that_is_not_a_journal_never_shows_the_dashboard` | A |
| `TestReadOnlyCommandsDoNotTakeHostLock` | `togi::read::commands::the_read_only_commands_succeed_while_another_process_holds_the_host_lock_and_never_take_it` | A |

### `cmd/togi/main_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestTopLevelRefusals` | `togi::cli::tests::no_command_prints_the_wordmark_and_usage_to_stderr_and_exits_2`<br>`togi::cli::tests::state_dir_without_a_command_prints_the_wordmark_and_usage_to_stderr_and_exits_2`<br>`togi::cli::tests::usage_errors_exit_2_with_stderr_only`<br>`togi::cli::no_command_prints_the_wordmark_and_usage_on_stderr`<br>`togi::cli::state_dir_without_a_command_prints_the_wordmark_and_usage_on_stderr`<br>`togi::cli::usage_errors_exit_2_on_stderr_with_nothing_on_stdout`<br>`togi::cli::a_stdout_that_cannot_be_written_exits_1_with_the_error_on_stderr` | A |

### `cmd/togi/reset_boundary_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestResetAllPathsCannotReviveFactsSoloLimitsOrFailurePoints` | `togi::reset::reset_all_archives_the_session_and_marks_the_boundary`<br>`togi::reset::reset_all_of_an_unrecognized_schema_archives_it_unread`<br>`togi::reset::a_pending_carry_is_dropped_without_a_journal`<br>`togi_journal::dir::tests::pending_markers_clear_but_the_reset_boundary_is_permanent`<br>`togi_journal::dir::tests::a_fault_anywhere_in_dropping_a_carry_never_loses_the_boundary_or_the_carry`<br>`togi_journal::dir::tests::an_id_always_follows_the_reset_boundary_whatever_the_clock_says` | A |

### `cmd/togi/reset_test.go` (11)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestCommandsRefuseWhileLocked` | `togi::reset::a_busy_host_lock_exits_3_naming_it_before_anything_changes`<br>`togi::reset::a_held_state_directory_lock_exits_3_and_changes_nothing`<br>`togi::doctor::a_busy_host_lock_exits_3_only_for_root`<br>`togi::read::commands::the_read_only_commands_succeed_while_another_process_holds_the_host_lock_and_never_take_it` | A |
| `TestResetAllCandidateSoloLimits` | `togi::reset::reset_all_warns_for_each_solo_limit_at_or_past_its_failure_point`<br>`togi::reset::start_offsets_are_not_solo_limits`<br>`togi::reset::a_solo_limit_on_a_core_without_a_failure_point_does_not_warn`<br>`togi::reset::a_core_that_failed_at_zero_warns_with_the_remove_it_remedy` | A |
| `TestResetLogsTheEventsItRecords` | `togi::reset::reset_core_queues_one_command_in_the_current_boot`<br>`togi::reset::reset_all_archives_the_session_and_marks_the_boundary`<br>`togi::reset::a_torn_tail_is_repaired_and_logged_before_the_reset`<br>`togi::reset::a_torn_tail_under_all_logs_the_repair_the_reset_and_the_archive`<br>`togi::reset::a_failed_archive_still_logs_the_events_it_recorded` | A |
| `TestResetAllSkipsUnavailableConfig` | `togi::reset::an_invalid_or_missing_config_never_stops_the_archive`<br>`togi::reset::a_missing_default_config_is_silent`<br>`togi::reset::an_invalid_default_config_is_silent_and_the_archive_succeeds`<br>`togi::reset::the_default_config_comes_from_the_host`<br>`togi::args::tests::a_missing_file_is_the_default_configuration_unless_it_was_asked_for`<br>`togi::args::tests::a_default_that_cannot_be_read_for_another_reason_than_absence_is_an_error`<br>`togi::args::tests::load_reads_the_default_the_caller_names_and_the_given_file_over_it` | A |
| `TestResetAllDropsAPendingCarry` | `togi::reset::a_pending_carry_is_dropped_without_a_journal`<br>`togi::reset::a_pending_carry_is_dropped_with_a_live_journal_and_an_empty_one` | A |
| `TestResetAllFindsPendingMarkersInPatternLikeStateDir` | `togi::reset::pending_markers_are_found_in_a_state_directory_named_like_a_pattern`<br>`togi::reset::a_pending_carry_is_dropped_with_a_live_journal_and_an_empty_one` | A |
| `TestResetAllWarnsAcrossRulesets` | `togi::reset::solo_limits_are_checked_across_rulesets_and_skipped_across_schemas` | A |
| `TestResetAllCannotInterruptLockedTransition` | `togi::reset::a_held_writer_lock_leaves_a_pending_carry_alone` | A |
| `TestMissingSessionRefusals` | `togi::reset::no_journal_or_no_session_exits_1_without_creating_state` | A |
| `TestResetCoreCommandOutcome` | `togi::reset::reset_core_queues_one_command_in_the_current_boot`<br>`togi::reset::reset_core_of_a_core_the_session_lacks_exits_2_and_changes_nothing`<br>`togi::reset::a_core_in_range_but_not_in_the_session_exits_2`<br>`togi::reset::exactly_one_of_core_and_all_and_a_core_number` | A |
| `TestResetErrorEscapesDiagnostic` | `togi::reset::error_text_escapes_control_characters` | A |

### `cmd/togi/restart_limit_test.go` (4)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestRestartLimitCommandBootActions` | `togi::restart_limit::every_count_picks_its_boot_action_in_order` | A |
| `TestRestartLimitHelpAndUsageHaveNoSideEffects` | `togi::restart_limit::usage_errors_and_help_touch_nothing`<br>`togi::cli::restart_limit_help`<br>`togi::cli::administration_usage_errors_exit_2_before_touching_anything` | A |
| `TestRestartLimitFailuresStopOrRemoveRetryMarker` | `togi::restart_limit::a_failure_stops_at_its_step_and_leaves_the_marker_right`<br>`togi::restart_limit::a_marker_that_cannot_be_removed_after_a_failed_reboot_is_reported`<br>`togi::restart_limit::a_reboot_that_cannot_run_reports_the_cause_and_removes_the_retry_marker` | A |
| `TestRestartLimitDispatchRejectsConfig` | `togi::restart_limit::the_state_directory_flag_is_accepted_after_the_command_and_config_never`<br>`togi::cli::administration_usage_errors_exit_2_before_touching_anything` | A |

### `cmd/togi/run_test.go` (17)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestRunFlagsCycleLimit` | `togi::run::tests::the_cycle_limit_is_endless_by_default_and_takes_a_positive_count`<br>`togi::cli::run_usage_errors_exit_2_before_anything_is_touched` | A |
| `TestRunDeadEndEvidencePriority` | `togi::run::outcome::tests::every_dead_end_exits_with_its_own_status_and_prints_its_detail`<br>`togi::run::outcome::tests::a_refused_journal_is_exit_sixteen_and_beats_a_dead_end`<br>`togi::run::outcome::tests::the_summary_and_each_evidence_line_are_whole_decorated_lines_in_the_system_journal` | A |
| `TestParseDefectAnswer` | `togi::run::prompt::tests::only_a_complete_line_saying_y_or_yes_is_a_yes`<br>`togi::run::prompt::tests::the_question_names_every_core_and_a_no_or_end_of_input_is_no`<br>`togi::run::prompt::tests::an_answer_that_could_not_be_read_is_no_unless_the_run_was_stopped` | A |
| `TestDefectPromptRejectsCharacterDevicesThatAreNotTerminals` | `togi::system::tests::a_character_device_that_is_not_a_terminal_is_not_a_terminal` | A |
| `TestDefectPromptAnswersOnlyInputTypedAfterTheQuestion` | `togi::run::prompt::tests::only_input_typed_after_the_question_is_the_answer`<br>`togi::run::prompt::tests::a_typed_line_is_read_whole_on_a_terminal` | A |
| `TestDefectPromptReturnsWhenRunIsStopped` | `togi::run::prompt::tests::a_run_stopped_while_the_question_is_open_ends_the_wait_without_an_answer`<br>`togi::run::prompt::tests::a_signal_that_interrupts_the_wait_is_a_wake_up_not_an_error` | A |
| `TestCompatibilityRefusalClearsGRUBAndUsesErrPriority` | `togi::run::lifecycle::a_refusal_under_journald_is_printed_at_err_priority_in_bold_red`<br>`togi::run::lifecycle::a_refused_journal_in_the_tuning_boot_leaves_a_reason_and_clears_the_saved_entry`<br>`togi::run::lifecycle::a_refused_journal_without_the_tuning_boot_leaves_the_boot_entry_alone` | A |
| `TestPrintCleanStop` | `togi::run::outcome::tests::only_the_closing_events_of_the_stop_the_dashboard_hid_are_tracked`<br>`togi::run::outcome::tests::only_the_events_the_dashboard_hid_are_printed_each_once_in_journal_order`<br>`togi::run::lifecycle::a_restoration_the_dashboard_hid_at_a_resumes_startup_is_not_repeated_by_the_clean_stop`<br>`togi::dashboard::tests::the_gate_says_for_each_event_line_whether_it_kept_it_off_the_screen` | A |
| `TestRunRejectsInvalidCycles` | `togi::run::tests::a_cycle_limit_that_is_not_a_positive_count_is_refused`<br>`togi::cli::run_usage_errors_exit_2_before_anything_is_touched` | A |
| `TestRunRejectsOldCyclesFlag` | `togi::run::tests::the_old_cycle_flag_is_unknown`<br>`togi::cli::run_usage_errors_exit_2_before_anything_is_touched` | A |
| `TestRunResultExitCodes` | `togi::exit::tests::dead_ends_map_by_name`<br>`togi::exit::tests::each_dead_end_has_its_own_code_between_10_and_18`<br>`togi::run::outcome::tests::every_dead_end_exits_with_its_own_status_and_prints_its_detail`<br>`togi::run::outcome::tests::a_run_that_ended_without_a_stop_or_an_error_is_a_failure`<br>`togi::run::outcome::tests::a_signal_the_cycles_and_the_first_result_exit_zero_in_silence` | A |
| `TestRunMalformedJournalRefusal` | `togi::run::lifecycle::a_journal_that_is_not_a_journal_is_an_error_and_is_left_alone` | A |
| `TestCompatibilityRefusalReportsFailedClear` | `togi::run::lifecycle::the_refusal_reports_a_failed_clear_and_requests_no_reboot` | A |
| `TestRunRefusalPersistsFixedReasonBeforeClearing` | `togi::run::lifecycle::the_refusal_persists_its_reason_before_it_clears_the_entry` | A |
| `TestRunRefusalStillClearsAfterReasonWriteFailure` | `togi::run::lifecycle::the_refusal_still_clears_the_entry_when_the_reason_cannot_be_written` | A |
| `TestRunFirstResultFlag` | `togi::run::tests::the_first_result_stands_alone`<br>`togi::cli::run_usage_errors_exit_2_before_anything_is_touched` | A |
| `TestRunRefusesFirstResultWithCycles` | `togi::run::tests::the_first_result_stands_alone`<br>`togi::cli::run_usage_errors_exit_2_before_anything_is_touched` | A |

### `cmd/togi/status_hunt_signal_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestStatusHuntShowsDecisiveFailureSignal` | `togi::read::status::a_hunt_names_the_decisive_failure_signal`<br>`togi::read::status::a_hunt_opened_by_a_carried_trial_names_the_carried_signal`<br>`togi::read::status::a_hunt_opened_by_a_carried_idle_failure_names_the_carried_signal`<br>`togi_watch::tests::hunt::a_hunt_caused_by_a_carried_trial`<br>`togi_watch::tests::hunt::a_hunt_caused_by_a_carried_idle_failure` | A |

### `cmd/togi/status_r7_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestStatusR7SelfSufficiencyAndTopRequester` | `togi::read::status::scenarios_render_the_status`<br>`togi_tuner::r7_escalation_tests::r7_status_separates_workloads_offsets_and_top_requesters`<br>`togi_tuner::r7_escalation_tests::r7_status_ignores_failed_starts_as_top_requester`<br>`togi_tuner::r7_escalation_tests::r7_status_cost` | A |

### `cmd/togi/status_test.go` (11)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestStatus` | `togi::read::status::scenarios_render_the_status` | A |
| `TestHistoricalTierChangeReadOnlyViews` | `togi::read::events::an_older_schema_is_refused_too` | O(the Rust journal has no historical tiers, so there is no tier change to view; an older journal is refused, pinned by the test listed) |
| `TestBetweenTrialMCEReadOnlyViews` | `togi::read::status::a_between_trial_mce_is_listed_as_evidence_and_escaped`<br>`togi::read::events::a_between_trial_mce_is_listed_and_its_controls_are_escaped`<br>`togi::read::watch::a_between_trial_mce_shows_in_the_frame` | A |
| `TestStatusCombinationAndOpenHunt` | `togi::read::status::scenarios_render_the_status`<br>`togi::read::status::hunt_group_columns_stay_aligned_when_a_group_lists_sixteen_cores`<br>`togi::read::status::a_located_hunt_names_the_held_loaded_cores_as_its_parked_source` | A |
| `TestStatusPhases` | `togi::read::status::scenarios_render_the_status`<br>`togi::read::status::a_concluded_phase_names_the_latest_clean_cycle_not_the_running_cycle`<br>`togi::read::status::a_phase_two_round_with_completed_checks_counts_them`<br>`togi_tuner::projection_tests::the_last_clean_cycle_follows_clean_cycles_not_the_latest_started_cycle` | A |
| `TestStatusShowsUnresetDefectResetCommands` | `togi::read::status::a_too_cautious_defect_lists_reset_commands_until_each_affected_core_is_reset`<br>`togi::read::status::a_reset_of_every_core_removes_the_defect` | A |
| `TestStatusExceptionalActivity` | `togi::read::status::scenarios_render_the_status`<br>`togi::read::status::a_fallback_combination_names_the_hunt_it_covered_every_candidate_of`<br>`togi::read::status::a_queued_reset_shows_in_the_queued_column_until_the_search_restarts`<br>`togi::read::status::a_short_checking_cycle_names_the_coverage_it_lacks`<br>`togi::status::tests::a_short_cycle_names_each_regime_it_lacks` | A |
| `TestStatusRecordOnlyPartial` | `togi_watch::frames::help::tests::help_glosses_no_record_only_trial` | O(the Rust journal has no record-only trials, so the partial has no record-only state to show; the test listed pins that the help glosses none) |
| `TestWriteStatusWrapsWideBIOS` | `togi::read::status::wide_bios_fields_wrap_by_terminal_cells_and_stay_whole` | A |
| `TestWriteStatusWrapsWideDecisionNotes` | `togi::table::tests::wide_decision_notes_wrap_under_their_row_within_the_width`<br>`togi::table::tests::notes_wrap_under_their_row_and_do_not_widen_the_table` | A |
| `TestWriteStatusPreservesUnicodeWords` | `togi::table::tests::unicode_words_are_kept_whole`<br>`togi::table::tests::a_wide_word_that_exactly_fills_the_room_stays_on_the_first_line`<br>`togi::table::tests::a_combining_word_that_exactly_fills_the_room_stays_on_the_first_line` | A |

### `cmd/togi/terminal_linux_test.go` (5)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestDashboardQuietsInputUntilItStops` | `togi::dashboard::tests::echo_and_line_buffering_stay_off_until_the_input_is_given_back_and_the_signal_keys_stay_on`<br>`togi::dashboard::tests::a_stream_that_is_not_a_terminal_is_left_alone` | A |
| `TestDiscardInputDropsTypedAhead` | `togi::dashboard::tests::typed_ahead_input_is_dropped`<br>`togi::dashboard::tests::input_typed_while_the_dashboard_showed_is_dropped_when_the_input_is_given_back` | A |
| `TestRunHardwareRestoresTerminalWhenSessionPanics` | `togi::run::pty::a_session_panic_stops_the_dashboard_and_gives_the_terminal_back_before_the_panic_propagates` | A |
| `TestRunHardwareFirstFrameIsThisRunsNotThePreviousOnes` | `togi::run::lifecycle::the_dashboard_starts_after_the_last_event_of_the_earlier_run`<br>`togi::run::lifecycle::the_dashboard_starts_after_the_journal_is_open` | A |
| `TestRunHardwareRestoresCursorAndPaletteWhenSessionPanics` | `togi::run::pty::a_session_panic_over_the_real_linux_console_dashboard_restores_the_cursor_and_the_palette` | A |

### `cmd/togi/terminal_test.go` (3)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestHumanJournalBoundaries` | `togi_render::escape::tests::range_boundaries_escape_exactly_the_listed_characters`<br>`togi_render::tests::write_event_ends_with_exactly_one_newline`<br>`togi_render::tests::write_event_appends_one_line_per_call` | A |
| `TestHumanJournalReadErrorEscapesUnknownKind` | `togi::read::commands::a_hostile_unknown_kind_is_escaped_in_the_spec_form_on_stderr` | A |
| `TestHumanProjectedFieldsEscapeControls` | `togi::read::status::a_between_trial_mce_is_listed_as_evidence_and_escaped`<br>`togi::read::events::a_between_trial_mce_is_listed_and_its_controls_are_escaped`<br>`togi_render::escape::tests::every_c0_control_and_del_is_escaped` | A |

### `cmd/togi/version_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestVersion` | `togi::cli::tests::version_is_the_togi_version_on_stdout_only`<br>`togi::cli::tests::version_after_a_command_is_a_usage_error`<br>`togi::cli::version_prints_the_version_and_revision_on_stdout`<br>`togi::cli::a_local_build_reports_the_dev_revision`<br>`togi::cli::version_after_a_command_is_a_usage_error`<br>`togi::cli::completion_reads_and_writes_nothing_in_the_state_directory`<br>`togi::cli::run_usage_errors_exit_2_before_anything_is_touched` | A |
| `TestVersionInTopLevelUsageOnly` | `togi::cli::top_level_help`<br>`togi::cli::completion_help` | A |

### `cmd/togi/watch_test.go` (4)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestWatchWithoutJournal` | `togi::read::watch::a_missing_journal_prints_no_session_yet_and_exits_zero` | A |
| `TestWatchProblemFrame` | `togi::read::watch::an_unreadable_journal_prints_the_problem_frame_and_exits_one`<br>`togi::read::watch::an_incompatible_journal_prints_the_refusal_frame_and_exits_one`<br>`togi::read::watch::without_a_terminal_one_frame_of_the_requested_size_is_printed`<br>`togi::read::watch::the_frame_size_must_be_at_least_one` | A |
| `TestWatchEscapesJournalControls` | `togi::read::watch::journal_controls_are_escaped_in_the_frame_and_the_diagnostic` | A |
| `TestStatusEscapesJournalMessages` | `togi::read::status::controls_in_an_in_flight_message_are_escaped`<br>`togi::read::status::controls_in_a_carried_session_message_are_escaped`<br>`togi::read::status::controls_in_a_decision_message_are_escaped`<br>`togi::read::status::controls_in_bios_context_fields_are_escaped` | A |

### `cmd/togi/watchdog_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestRunWatchdogWarning` | `togi_session::session::tests::a_missing_watchdog_is_warned_about_outside_the_tuning_boot`<br>`togi::run::lifecycle::a_missing_watchdog_is_journaled_as_a_warning_before_the_first_offset_write_with_the_dashboard_up` | A |
