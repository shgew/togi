# Stage 6 Go-test mapping

Every `func Test*`, `Benchmark*`, `Fuzz*` and `Example*` in the Go packages ported in stage 6 (#641) is mapped to the Rust tests that pin the same behavior: `internal/watch` → `togi-watch`, `internal/watch/watchtest` → `togi-watchtest` and `tools/replay` → `togi-tool-replay`. The Linux-only `notify_linux_test.go` is included. There are no Fuzz or Example functions and one Benchmark. `accessors_test.go` and `golden_test.go` hold only helpers, and `main_test.go` holds only `TestMain`.

Rust names are `crate::module::fn`; the tests of the `togi-watchtest` integration binary add its name after the crate (`togi_watchtest::watchtest::…`), and so do the tests of the `togi-tool-replay` integration binary (`togi_tool_replay::replay::…`). Every `togi_watch`, `togi_watchtest` and `togi_tool_replay` name was checked against the source tree that includes this closing pull request, and is a test function there (the macro-generated ones included). The pull requests that ported each part are #846 (the model), #850 (`togi-watchtest`), #853 (the terminal loop), #863 (the frames) and #885 (the replay tool, which closes #837).

A Go test that pinned a frame and the model often maps to tests of both: the model's under `togi_watch::tests`, the frames' under `togi_watch::frames`, the loop's under `togi_watch::terminal`. The goldens of `TestWatchFrames` and `TestWatchWithoutJournal` map to the insta snapshots of `togi_watch::frames::snapshots`; the native frames do not promise Go's bytes.

Status:

- **A**: a test of a stage 6 crate pins the behavior.
- **X**: a test of another Rust crate pins it.
- **O(reason)**: obsolete under the native contract.
- **G(reason)**: not pinned by any Rust test and not obsolete.
- **Sources:** every row's Rust names were read from the source tree that includes this closing pull request.

## Summary

| Package | Go tests | A | X | O | G |
|---|---:|---:|---:|---:|---:|
| watch | 162 | 160 | 0 | 2 | 0 |
| watchtest | 0 | 0 | 0 | 0 | 0 |
| replay | 10 | 9 | 0 | 1 | 0 |
| **total** | **172** | **169** | **0** | **3** | **0** |

## watch

### `internal/watch/cycle_resume_test.go` (3)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestRerunNamesTheStepCheckingResumesAt` | `togi_watch::tests::hunt::resume_step_skips_steps_the_tuner_records_without_a_trial`<br>`togi_watch::tests::model::a_running_rerun_names_the_step_checking_resumes_at`<br>`togi_watch::frames::story::tests::the_story::a_rerun_names_the_step_checking_resumes_at`<br>`togi_watch::frames::frame_tests::bands::a_rerun_that_completes_the_cycle_names_the_step_checking_resumes_at_never_the_next` | A |
| `TestRecoveryNamesTheStepTheCrashedTrialRanIn` | `togi_watch::tests::model::a_recovered_trial_keeps_its_place_in_the_cycle`<br>`togi_watch::tests::derive::a_recovered_trial_takes_its_step_and_count_from_the_facts`<br>`togi_watch::frames::story::tests::the_resting_band::a_recovery_says_what_the_crash_ended_how_late_and_what_runs_next`<br>`togi_watchtest::watchtest::fixtures::a_crashed_cycle_trial_of_the_crashed_cycle_session_leaves_its_cycle_at_another_step` | A |
| `TestRerunCompletingTheCycleNamesNoStepOfTheNext` | `togi_watch::frames::story::tests::the_story::a_rerun_that_completes_the_cycle_names_no_step_of_the_next`<br>`togi_watch::frames::frame_tests::bands::a_rerun_that_completes_the_cycle_names_the_step_checking_resumes_at_never_the_next`<br>`togi_watchtest::watchtest::fixtures::reruns_that_pass_complete_the_cycle_before_the_next_cycle_starts` | A |

### `internal/watch/fit_test.go` (6)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestCutWordsKeepsWholeWordsAndMarksTheCut` | `togi_watch::frames::fit::tests::cut_words_keeps_whole_words_and_marks_the_cut` | A |
| `TestCutWordsNoFittingWord` | `togi_watch::frames::fit::tests::cut_words_with_no_fitting_word_leaves_nothing`<br>`togi_watch::frames::fit::tests::a_cut_keeps_the_style_of_the_words_it_kept`<br>(Go's colour reset after a cut is an ANSI artefact; a buffer cell carries one style, and the cut marker keeps the style of the last kept word) | A |
| `TestRuleKeepsWholeTitleWords` | `togi_watch::frames::fit::tests::the_rule_keeps_whole_title_words`<br>`togi_watch::frames::help::tests::the_title_rule_keeps_whole_words_and_a_dash_track`<br>`togi_watch::frames::fit::tests::the_rule_puts_its_right_side_at_the_end_when_all_of_it_fits` | A |
| `TestFitClausesLeavesOutLowerPriorityClausesBeforeCutting` | `togi_watch::frames::fit::tests::fit_clauses_leaves_out_lower_priority_clauses_before_cutting` | A |
| `TestNoteLineKeepsItsValueAtEveryWidth` | `togi_watch::frames::notes::tests::a_note_line_keeps_its_value_at_every_width`<br>`togi_watch::frames::notes::tests::every_note_shows_its_value_whole_or_nothing_at_every_width` | A |
| `TestTopNoteIsAWordWholeOrNothing` | `togi_watch::frames::notes::tests::the_top_note_is_a_word_whole_or_nothing` | A |

### `internal/watch/frames_test.go` (11)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestWatchFrames` | `togi_watch::frames::frame_tests::bounds::between`<br>`togi_watch::frames::frame_tests::bounds::checking`<br>`togi_watch::frames::frame_tests::bounds::combination`<br>`togi_watch::frames::frame_tests::bounds::concluded`<br>`togi_watch::frames::frame_tests::bounds::confirm`<br>`togi_watch::frames::frame_tests::bounds::confirmation`<br>`togi_watch::frames::frame_tests::bounds::crashed`<br>`togi_watch::frames::frame_tests::bounds::deadend`<br>`togi_watch::frames::frame_tests::bounds::first_result`<br>`togi_watch::frames::frame_tests::bounds::hunt`<br>`togi_watch::frames::frame_tests::bounds::hunt_bios`<br>`togi_watch::frames::frame_tests::bounds::idle`<br>`togi_watch::frames::frame_tests::bounds::member_probe`<br>`togi_watch::frames::frame_tests::bounds::recovering`<br>`togi_watch::frames::frame_tests::bounds::round`<br>`togi_watch::frames::frame_tests::bounds::search`<br>`togi_watch::frames::frame_tests::bounds::stopped`<br>`togi_watch::frames::frame_tests::bounds::unconfirmed`<br>`togi_watch::frames::frame_tests::without_journal::a_journal_that_is_missing_starting_or_unreadable_stays_in_bounds_and_never_holds`<br>`togi_watch::frames::snapshots::every_scenario_fits_the_frame_at_every_main_size`<br>`togi_watch::frames::snapshots::between`<br>`togi_watch::frames::snapshots::checking`<br>`togi_watch::frames::snapshots::checking_in_each_colour_profile`<br>`togi_watch::frames::snapshots::combination`<br>`togi_watch::frames::snapshots::concluded`<br>`togi_watch::frames::snapshots::confirm`<br>`togi_watch::frames::snapshots::confirmation`<br>`togi_watch::frames::snapshots::crashed`<br>`togi_watch::frames::snapshots::deadend`<br>`togi_watch::frames::snapshots::every_scenario_has_a_main_view_snapshot`<br>`togi_watch::frames::snapshots::first_result`<br>`togi_watch::frames::snapshots::help_at_the_view_sizes`<br>`togi_watch::frames::snapshots::hunt`<br>`togi_watch::frames::snapshots::hunt_bios`<br>`togi_watch::frames::snapshots::idle`<br>`togi_watch::frames::snapshots::idle_in_each_colour_profile`<br>`togi_watch::frames::snapshots::member_probe`<br>`togi_watch::frames::snapshots::member_probe_in_each_colour_profile`<br>`togi_watch::frames::snapshots::no_color_and_journald_map_to_one_profile_and_the_console_to_its_own`<br>`togi_watch::frames::snapshots::recovering`<br>`togi_watch::frames::snapshots::round`<br>`togi_watch::frames::snapshots::search`<br>`togi_watch::frames::snapshots::stopped`<br>`togi_watch::frames::snapshots::the_journal_view_at_the_view_sizes`<br>`togi_watch::frames::snapshots::unconfirmed`<br>(Go kept three sizes for every cut. #536 is decided: a cut keeps 240x67, 160x45 and 120x33 only when it draws a size-dependent panel no other kept cut draws. In Rust nine cuts keep 240x67 only (`confirm`, `between`, `stopped`, `deadend`, `confirmation`, `concluded`, `unconfirmed`, `crashed`, `first_result`), which removed 18 snapshots, and `every_scenario_fits_the_frame_at_every_main_size` draws every cut at all three sizes through the bounds check without writing a snapshot) | A |
| `TestFramesSpendBytesOnCellsNotStaleStyles` | `togi_watch::terminal::show::tests::only_changed_rows_are_written`<br>(frames are ratatui cell buffers with one style per cell; the loop's diff writes only changed cells) | A |
| `TestFramesDrawOnlyIBM437Glyphs` | `togi_watch::frames::help::tests::help_stays_in_bounds_and_draws_only_console_glyphs_at_every_size`<br>`togi_watch::frames::frame_tests::without_journal::a_journal_that_is_missing_starting_or_unreadable_stays_in_bounds_and_never_holds` | A |
| `TestConsoleTextReplacesWhatTheFontCannotDraw` | `togi_watch::frames::fit::tests::console_text_replaces_what_the_font_cannot_draw` | A |
| `TestDashboardEscapesDiagnosticControls` | `togi_watch::frames::fit::tests::inert_escapes_controls_visibly_and_is_idempotent`<br>`togi_watch::frames::draw::tests::untrusted_text_is_written_escaped`<br>`togi_watch::frames::notes::tests::a_queued_note_is_escaped_untrusted_text`<br>`togi_watch::frames::frame_tests::escapes::a_problem_with_controls_in_its_text_draws_them_visibly_escaped_live_and_in_one_frame`<br>`togi_watch::frames::frame_tests::escapes::a_dead_end_a_retry_and_a_warning_with_controls_are_drawn_escaped` | A |
| `TestDashboardEscapesTrialDetails` | `togi_watch::frames::frame_tests::escapes::a_trial_id_with_controls_is_drawn_escaped`<br>`togi_watch::frames::frame_tests::escapes::a_dead_end_a_retry_and_a_warning_with_controls_are_drawn_escaped`<br>`togi_watch::frames::panels::log::tests::untrusted_tags_and_messages_are_escaped_at_output`<br>`togi_watch::frames::panels::history::tests::a_sentence_with_controls_is_escaped_at_output`<br>(the workload is a closed enum in Rust, so there is no raw workload name to escape) | A |
| `TestWatchWithoutJournal` | `togi_watch::frames::frame_tests::without_journal::a_missing_journal_says_no_session_yet_and_an_unreadable_one_says_why`<br>`togi_watch::frames::frame_tests::without_journal::a_problem_and_a_one_frame_problem_differ_only_by_the_retry_promise`<br>`togi_watch::frames::snapshots::journal_states_at_the_main_sizes`<br>`togi_watch::frames::snapshots::one_frame_prints_the_live_text_without_the_key_footer` | A |
| `TestRoundKeepsItsCandidatesAtEveryHeight` | `togi_watch::frames::panels::context::tests::the_round_keeps_its_candidates_at_every_height`<br>`togi_watch::frames::panels::context::tests::r7_keeps_its_rows_only_when_they_fit_under_the_candidates`<br>`togi_watch::frames::frame_tests::panels::the_round_keeps_its_candidate_rows_at_every_height` | A |
| `TestWatchBetweenTrialMCE` | `togi_watch::frames::frame_tests::panels::a_between_trial_machine_check_is_not_hidden` | A |
| `BenchmarkProject` | `project/checking` in the `frames` criterion bench (`crates/togi-watch/benches/frames.rs`), over the same checking-trial journal; a bench, not a test | A |
| `TestTrialBarShowsElapsedWithoutColour` | `togi_watch::frames::dash::tests::the_trial_bar_shows_elapsed_without_colour`<br>`togi_watch::frames::frame_tests::panels::the_trial_bar_shows_elapsed_without_colour_at_every_size` | A |

### `internal/watch/history_render_test.go` (6)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestHeldJournalArrivalNoticeFitsFrame` | `togi_watch::frames::panels::log::tests::arrivals_never_move_a_row`<br>`togi_watch::frames::panels::log::tests::the_footer_counts_what_arrived_and_names_the_way_back`<br>`togi_watch::frames::frame_tests::arrivals::arrivals_never_move_a_row_of_a_held_log_and_the_footer_counts_them` | A |
| `TestHeldLogArrivalKeepsKeys` | `togi_watch::frames::panels::log::tests::the_arrival_count_keeps_the_keys_in_the_footer` | A |
| `TestHistoryPanelTinyAndTallBudgetsCountDroppedEntries` | `togi_watch::frames::panels::history::tests::tiny_and_tall_budgets_count_every_entry_left_out` | A |
| `TestHistoryPanelEmptyAndSmallBudgets` | `togi_watch::frames::panels::history::tests::empty_and_small_budgets_leave_nothing_uncounted` | A |
| `TestRenderedTinyHistoryCountsAllHiddenEntries` | `togi_watch::frames::frame_tests::panels::a_history_too_short_for_its_entries_counts_every_one_it_leaves_out` | A |
| `TestRenderedCombinationOverflowNamesOmittedCoresAndBoundedLog` | `togi_watch::frames::panels::cycle::tests::combination_overflow_names_the_omitted_cores_and_the_journal_bound`<br>`togi_watch::frames::frame_tests::panels::a_combination_wider_than_its_columns_names_the_omitted_cores_and_the_journal_bound` | A |

### `internal/watch/history_test.go` (16)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestProjectRebootHistoryDuringSMUApplication` | `togi_watch::tests::history::reboot_history_during_smu_application` | A |
| `TestProjectRebootHistoryDuringTrial` | `togi_watch::tests::history::reboot_history_during_trial` | A |
| `TestProjectMultipleRebootsStayDistinct` | `togi_watch::tests::history::multiple_reboots_stay_distinct` | A |
| `TestProjectRebootHistoryRequiresMatchingCause` | `togi_watch::tests::history::reboot_history_requires_matching_cause` | A |
| `TestProjectFallbackHistory` | `togi_watch::tests::history::fallback_history` | A |
| `TestProjectCycleHistory` | `togi_watch::tests::history::cycle_history` | A |
| `TestProjectOrphanTrialIDIsEscaped` | `togi_watch::tests::history::orphan_trial_id_stays_raw`<br>`togi_watch::frames::frame_tests::escapes::a_trial_id_with_controls_is_drawn_escaped`<br>(the model keeps the id raw and the frames escape it where drawn) | A |
| `TestProjectDeepeningHistorySeparatesDeeperAndYieldedMembers` | `togi_watch::tests::history::deepening_history_separates_deeper_and_yielded_members` | A |
| `TestHistoryGroupsInferredAnswersWithoutHidingOutcomeChanges` | `togi_watch::tests::history::inferred_group_answers_do_not_hide_outcome_changes` | A |
| `TestTallyNeverCountsPastTheRequirement` | `togi_watch::tests::history::tally_never_counts_past_the_requirement` | A |
| `TestProjectProbeHistoryBeforeRetention` | `togi_watch::tests::history::probe_history_before_retention` | A |
| `TestProjectProbeHistoryAcrossRetentionBoundary` | `togi_watch::tests::history::probe_history_across_the_retention_boundary` | A |
| `TestProjectProbeHistoryLongRun` | `togi_watch::tests::history::probe_history_long_run` | A |
| `TestProjectProbeHistoryCompletesAfterRetention` | `togi_watch::tests::history::probe_history_completes_after_retention` | A |
| `TestProjectProbeHistoryCompleteTailRemainsMutable` | `togi_watch::tests::history::probe_history_complete_tail_remains_mutable` | A |
| `TestProjectRecordCrashHistoryShrinksAfterRetention` | — | O(record-only trials are gone, so no crash line carries a merge key and none refolds into the line before it after retention) |

### `internal/watch/journal_view_test.go` (7)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestScrolledBackJournalViewHoldsItsListAndCountsArrivals` | `togi_watch::terminal::source::tests::a_scrolled_back_log_holds_its_list_and_counts_arrivals`<br>`togi_watch::frames::frame_tests::arrivals::arrivals_never_move_a_row_of_a_held_log_and_the_footer_counts_them` | A |
| `TestClosingScrolledBackJournalViewLetsGoOfItsList` | `togi_watch::terminal::source::tests::closing_a_scrolled_back_log_lets_go_of_its_list` | A |
| `TestScrolledBackJournalViewRecoversUnavailableJournal` | `togi_watch::terminal::source::tests::a_scrolled_back_log_drops_its_list_when_the_journal_goes_away_or_breaks` | A |
| `TestScrolledBackJournalViewDiscardsChangedJournal` | `togi_watch::terminal::source::tests::a_scrolled_back_log_drops_its_list_when_the_journal_is_replaced_or_truncated`<br>`togi_watch::terminal::source::tests::a_journal_rewritten_in_place_to_a_longer_one_builds_a_fresh_model_and_drops_the_held_list` | A |
| `TestScrolledBackJournalViewHoldsSourceAppendsAndSessionStart` | `togi_watch::terminal::source::tests::a_scrolled_back_log_holds_through_appends`<br>`togi_watch::terminal::source::tests::a_second_session_start_is_a_fold_problem_that_drops_the_held_list`<br>(a second `session.start` in one journal is a fold problem in Rust, which drops the held list) | A |
| `TestProjectCountsEveryListedJournalEvent` | `togi_watch::tests::model::the_log_counts_every_listed_event`<br>`togi_watch::tests::words::every_kind_has_its_words_or_says_it_is_silent` | A |
| `TestHistoryPanelCountsEveryEntryItLeavesOut` | `togi_watch::frames::panels::history::tests::the_panel_counts_every_entry_the_model_left_out`<br>`togi_watch::tests::history::history_bounds_and_counts_what_it_leaves_out` | A |

### `internal/watch/log_render_test.go` (3)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestLogBodyRowKeepsWholeValuesAtEveryWidth` | `togi_watch::frames::panels::log::tests::a_row_keeps_whole_values_at_every_width` | A |
| `TestLogBodyEmptyNoticeKeepsWholeWords` | `togi_watch::frames::panels::log::tests::an_empty_journal_says_so_in_whole_words` | A |
| `TestLogBodyWideRowsAlignTagsInLocalTime` | `togi_watch::frames::panels::log::tests::rows_align_their_tags_in_local_time` | A |

### `internal/watch/main_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestMain` | — | O(set the process zone to UTC so goldens did not depend on the machine; Rust frames take the zone as an argument, and each test passes its own) |

### `internal/watch/narration_test.go` (11)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestNoTrialYetDoesNotClaimOneEnded` | `togi_watch::frames::story::tests::the_story::no_trial_yet_does_not_claim_one_ended`<br>`togi_watch::frames::frame_tests::bands::no_trial_yet_does_not_claim_one_ended` | A |
| `TestCompleteConfirmationTurnRecordsTheSoloLimit` | `togi_watch::frames::panels::search::tests::a_complete_confirmation_turn_records_the_solo_limit`<br>`togi_watch::tests::derive::a_core_confirming_its_solo_limit_shows_the_count` | A |
| `TestStoppedStageLineShowsNoRunningStage` | `togi_watch::frames::frame_tests::bands::a_stopped_frame_labels_saved_offsets_as_not_applied_and_shows_no_running_stage`<br>`togi_watch::frames::frame_tests::bands::a_running_stage_line_keeps_its_marker_and_its_repeat_note`<br>`togi_watch::frames::story::tests::the_story::a_stopped_session_says_why_and_whether_the_offsets_were_restored` | A |
| `TestUnreadableJournalIsNotAMissingOne` | `togi_watch::frames::story::tests::the_story::an_unreadable_journal_is_not_a_missing_one`<br>`togi_watch::frames::story::tests::the_story::the_error_text_is_escaped_visibly_at_output`<br>`togi_watch::frames::story::tests::the_story::a_missing_journal_is_no_session_yet`<br>`togi_watch::frames::frame_tests::without_journal::an_unreadable_journal_is_not_a_missing_one_live_and_in_one_frame` | A |
| `TestLoadTellsMissingFromUnreadable` | `togi_watch::terminal::source::tests::missing_and_unreadable_are_distinct`<br>`togi_watch::terminal::source::tests::reload_follows_a_replaced_grown_rewritten_removed_and_restored_journal`<br>`togi_watch::terminal::tests::one_frame_of_a_missing_journal_prints_the_frame_without_styling_and_succeeds`<br>`togi_watch::terminal::tests::one_frame_of_an_unreadable_journal_prints_its_problem_frame_then_returns_the_error` | A |
| `TestCrashedTrialNotYetRecoveredIsNotNarratedAsRunning` | `togi_watch::tests::model::a_prior_recovery_does_not_mask_a_later_boot_crash`<br>`togi_watch::frames::story::tests::the_story::a_crashed_trial_is_not_narrated_as_running`<br>`togi_watch::frames::story::tests::the_simulated_sessions::a_crashed_trial_and_its_recovery_are_told_apart`<br>`togi_watch::frames::frame_tests::bands::a_crashed_trial_that_a_later_boot_outlived_is_not_narrated_as_running` | A |
| `TestTrialPastItsPlannedEndShowsHowLongWithoutClaimingACrash` | `togi_watch::tests::hunt::past_end_counts_whole_seconds_past_the_planned_end`<br>`togi_watch::frames::story::tests::the_story::a_trial_past_its_end_says_how_long_without_claiming_a_crash`<br>`togi_watch::frames::frame_tests::bands::a_trial_past_its_planned_end_shows_how_long_without_claiming_a_crash` | A |
| `TestProjectPriorRecoveryDoesNotMaskLaterBootCrash` | `togi_watch::tests::model::a_prior_recovery_does_not_mask_a_later_boot_crash`<br>`togi_watch::frames::story::tests::the_story::a_prior_recovery_does_not_mask_a_later_boot_crash`<br>`togi_watch::frames::frame_tests::bands::a_prior_recovery_does_not_mask_a_later_boot_crash` | A |
| `TestNarrationResetAfterRebootShowsUnrecoveredCrash` | `togi_watch::tests::model::a_reset_in_a_later_boot_leaves_the_crashed_trial_unrecovered`<br>`togi_watch::frames::story::tests::the_story::a_command_shutdown_does_not_mask_an_unrecovered_crash`<br>`togi_watch::frames::frame_tests::bands::a_command_shutdown_does_not_mask_an_unrecovered_crash` | A |
| `TestNarrationOverdueWithProgressReportsMissingTrialEnd` | `togi_watch::tests::model::a_trial_reporting_progress_past_its_planned_end_is_still_running`<br>`togi_watch::tests::hunt::past_end_counts_whole_seconds_past_the_planned_end`<br>`togi_watch::frames::frame_tests::bands::a_trial_overdue_with_later_progress_reports_its_missing_end` | A |
| `TestNarrationR6OverdueKeepsQuietFrameUntilReboot` | `togi_watch::tests::hunt::a_started_idle_trial_holds_every_frame_until_its_end_is_recorded`<br>`togi_watch::frames::story::tests::the_story::an_idle_trial_holds_the_story_until_its_end_is_recorded`<br>`togi_watch::frames::frame_tests::holds::an_idle_trial_freezes_every_view_until_its_end_is_recorded`<br>`togi_watch::frames::frame_tests::holds::a_later_boot_releases_an_idle_trial_and_the_clock_moves_again`<br>`togi_watch::frames::frame_tests::holds::every_other_frame_moves_with_the_clock` | A |

### `internal/watch/notify_linux_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestJournalChangeWaitsForItsReload` | `togi_watch::terminal::notify::tests::a_change_waits_for_its_reload_before_the_notifier_goes_on` | A |

### `internal/watch/r7_status_test.go` (13)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestR7LinesNameTopRequestersAndSelfSufficiency` | `togi_watch::frames::dash::tests::r7_lines_name_top_requesters_and_self_sufficiency`<br>`togi_watch::frames::dash::tests::cut_top_requesters_keep_their_offset_label` | A |
| `TestR7WholeValues` | `togi_watch::frames::dash::tests::r7_values_stay_whole` | A |
| `TestR7NarrowRowsKeepOffsetProvenance` | `togi_watch::frames::dash::tests::narrow_r7_rows_keep_offset_provenance_and_fit` | A |
| `TestR7ProvenanceExactFit` | `togi_watch::frames::dash::tests::narrow_r7_rows_keep_offset_provenance_and_fit` | A |
| `TestR7EvidenceStaysOutOfTheForecast` | `togi_watch::frames::story::tests::the_outcome_lines::r7_evidence_stays_out_of_the_forecast` | A |
| `TestShortDashboardReachesEveryR7Workload` | `togi_watch::frames::dash::tests::every_r7_workload_has_its_row`<br>`togi_watch::frames::frame_tests::panels::a_short_dashboard_reaches_every_r7_workload_and_the_running_part` | A |
| `TestCoreRowsMarkTheRunningWorkloadsTopRequesters` | `togi_watch::tests::hunt::r7_workload_and_top_requesters`<br>`togi_watch::frames::dash::tests::core_rows_mark_the_running_workloads_top_requesters` | A |
| `TestR7BackoffForecastIsConditionalOnEarlierRequests` | `togi_watch::frames::story::tests::the_outcome_lines::a_backoff_forecast_is_conditional_on_earlier_requests` | A |
| `TestOutcomeRowsKeepEveryR7NamedOutcome` | `togi_watch::frames::story::tests::the_outcome_lines::every_r7_named_outcome_keeps_a_row` | A |
| `TestZeroOffsetCoresOnDifferentCCDsAreNamedApart` | `togi_watch::frames::story::tests::the_outcome_lines::zero_offset_cores_on_different_ccds_are_named_apart` | A |
| `TestPassForecastNamesTheDerivedPartial` | `togi_watch::frames::story::tests::the_outcome_lines::a_pass_forecast_names_the_derived_partial_and_its_basis` | A |
| `TestNamedZeroCoreMovingItsCCDRecordsNoOwnFailurePoint` | `togi_watch::frames::story::tests::the_outcome_lines::a_named_zero_core_moving_its_ccd_records_no_own_failure_point` | A |
| `TestTopNoteFitsBesideANoteAtItsExactWidth` | `togi_watch::frames::dash::tests::the_top_note_fits_beside_a_note_at_its_exact_width` | A |

### `internal/watch/render_test.go` (31)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestHelpWideUsesThreeColumns` | `togi_watch::frames::help::tests::wide_help_uses_three_columns` | A |
| `TestHelpLinksTunerSpecAndStatesNoRule` | `togi_watch::frames::help::tests::help_links_the_tuner_spec_and_states_no_rule`<br>(#539 is decided: the help says what the screen shows and links the tuner spec, and states no rule. The Rust test checks the link and that 28 rule phrases removed from WHAT TOGI DOES and WORDS are gone; it checked five before this change) | A |
| `TestHelpNarrowScrollsOneColumn` | `togi_watch::frames::help::tests::narrow_help_scrolls_one_column` | A |
| `TestLogFollowsNewEntriesOnlyAtEnd` | `togi_watch::frames::panels::log::tests::the_view_follows_new_entries_only_at_the_end`<br>`togi_watch::frames::panels::log::tests::a_scroll_at_or_past_the_extent_shows_the_end`<br>`togi_watch::terminal::keys::tests::log_reaching_its_end_by_page_follows_it` | A |
| `TestHelpLogBodiesFitSmallRectangles` | `togi_watch::frames::help::tests::help_bodies_fit_small_rectangles`<br>`togi_watch::frames::panels::log::tests::log_bodies_fit_small_rectangles` | A |
| `TestHelpLabelsKeepWholeWords` | `togi_watch::frames::help::tests::help_labels_keep_whole_words` | A |
| `TestHelpExamplesKeepWholeValues` | `togi_watch::frames::help::tests::help_examples_keep_whole_values` | A |
| `TestHelpViewExamplesKeepWholeValues` | `togi_watch::frames::help::tests::the_help_view_examples_keep_whole_values` | A |
| `TestRenderFooterKeepsKeys` | `togi_watch::frames::footer::tests::the_footer_keeps_all_four_keys_at_twenty_cells_and_wider` | A |
| `TestRenderFooterKeepsCloseKeyFirst` | `togi_watch::frames::footer::tests::the_footer_keeps_the_close_key_first`<br>`togi_watch::frames::footer::tests::q_is_the_last_key_left_out_and_shown_whole_when_one_cell_fits` | A |
| `TestPress` | `togi_watch::terminal::keys::tests::press_maps_every_key_of_go_test_press`<br>`togi_watch::terminal::keys::tests::quit_keys_quit_and_change_nothing`<br>`togi_watch::terminal::keys::tests::letter_aliases_scroll_like_the_named_keys`<br>`togi_watch::terminal::keys::tests::log_reaching_its_end_by_page_follows_it` | A |
| `TestDecodeKeys` | `togi_watch::terminal::keys::tests::decode_maps_every_case_of_go_test_decode_keys` | A |
| `TestScrollbarThumbTracksThePage` | `togi_watch::frames::scroll::tests::the_scrollbar_thumb_tracks_the_page` | A |
| `TestMediumCoreNotesRetainCombinationAndSoloOffsets` | `togi_watch::frames::dash::tests::medium_core_notes_retain_combination_and_solo_offsets`<br>`togi_watch::frames::frame_tests::panels::medium_core_notes_keep_the_combination_and_the_solo_offset` | A |
| `TestOutcomeLinesMergeBranchesAndNameTheSamePartsTrials` | `togi_watch::frames::story::tests::the_outcome_lines::branches_merge_and_name_the_next_trial_of_the_same_part`<br>(the Go fixture's record-only trial has no native counterpart; the same branches run on an ordinary trial) | A |
| `TestPassingOutcomeNamesTheNextPartOrStep` | `togi_watch::frames::story::tests::the_outcome_lines::a_passing_outcome_names_the_next_part_or_step` | A |
| `TestNarratorWrapsUnderItsText` | `togi_watch::frames::story::tests::the_narrator::the_narrator_wraps_under_its_text`<br>`togi_watch::frames::story::tests::the_narrator::a_story_too_tall_for_its_rows_falls_back_to_its_brief_then_to_cut_lines` | A |
| `TestRestingBandLeavesItsRowsToThePanels` | `togi_watch::frames::frame_tests::bands::a_resting_band_of_one_line_leaves_the_ccd_table_its_rows` | A |
| `TestUndrawableGlyphsAreMeasuredEscaped` | `togi_watch::frames::fit::tests::undrawable_glyphs_are_measured_escaped` | A |
| `TestCCDRoleCountsLoadAndProbes` | `togi_watch::frames::dash::tests::a_ccd_role_counts_load_and_probes` | A |
| `TestRestoredDeadEndLabelsSavedRows` | `togi_watch::frames::dash::tests::a_restored_session_shows_the_saved_profile_in_its_rows`<br>`togi_watch::frames::dash::tests::a_readback_is_never_clamped`<br>`togi_watch::frames::frame_tests::bands::a_restored_dead_end_labels_its_rows_as_saved` | A |
| `TestHuntStageStaysOnAnUnfinishedPartBetweenTrials` | `togi_watch::frames::panels::hunt::tests::the_stage_stays_on_an_unfinished_part_between_trials` | A |
| `TestProbeWithoutFailingOffsetRenders` | `togi_watch::frames::panels::hunt::tests::a_probe_without_a_failing_offset_renders`<br>`togi_watch::frames::panels::hunt::tests::probes_show_how_each_member_stands` | A |
| `TestPartialTrialUsesOrdinaryEvidence` | `togi_watch::frames::story::tests::the_story::a_partial_explains_that_it_is_ordinary_evidence`<br>`togi_watch::frames::frame_tests::panels::a_partial_trial_is_named_by_its_ordinary_evidence`<br>`togi_watch::tests::describe::an_r7_cycle_trial_names_the_part_it_loaded`<br>`togi_watch::tests::derive::a_recovered_trial_says_when_it_started_and_that_it_loaded_part_of_a_ccd`<br>`togi_watch::tests::words::chains_cycles_and_rounds_have_phrases` | A |
| `TestAllZeroRerunIsNamedForWhatItDecides` | `togi_watch::frames::story::tests::the_story::an_all_zero_rerun_is_named_for_what_it_decides`<br>`togi_watch::tests::describe::a_trial_is_named_by_what_it_ran` | A |
| `TestForecastNamesTheAllZeroRerunAfterARerun` | `togi_watch::frames::story::tests::the_outcome_lines::the_forecast_names_the_all_zero_rerun_after_a_rerun` | A |
| `TestEveryTimeOnScreenIsLocal` | `togi_watch::frames::dash::tests::every_time_on_screen_is_local`<br>`togi_watch::frames::frame_tests::local_time::every_time_on_screen_is_local` | A |
| `TestGaugeCombinationHeldDepth` | `togi_watch::frames::dash::tests::a_gauge_shows_the_depth_a_combination_holds`<br>`togi_watch::frames::dash::tests::a_gauge_shows_group_failures_and_judging` | A |
| `TestHuntStageSaysWhenPartsWereRecut` | `togi_watch::frames::panels::hunt::tests::the_stage_says_when_parts_were_recut` | A |
| `TestHeaderCountsAgreeInNumber` | `togi_watch::frames::dash::tests::header_counts_agree_in_number` | A |
| `TestStartingScreenNamesNoEarlierRun` | `togi_watch::frames::dash::tests::a_journal_that_is_not_folded_names_its_status_and_no_earlier_run`<br>`togi_watch::frames::frame_tests::without_journal::the_starting_screen_says_so_and_names_no_earlier_run` | A |

### `internal/watch/run_test.go` (14)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestShowWritesOnlyChangedRows` | `togi_watch::terminal::show::tests::only_changed_rows_are_written`<br>`togi_watch::terminal::show::tests::a_resize_clears_and_repaints_the_whole_screen_once`<br>`togi_watch::terminal::show::tests::the_console_palette_is_set_on_entry_and_reset_on_every_exit` | A |
| `TestLiveHoldsUntilJournalKeyResizeOrDeadline` | `togi_watch::terminal::show::tests::a_live_frame_holds_until_a_journal_change_key_resize_or_its_deadline` | A |
| `TestRefreshClockStopsTickerDuringHold` | `togi_watch::terminal::clock::tests::a_held_frame_stays_quiet_until_its_instant_then_ticks`<br>`togi_watch::terminal::clock::tests::a_clock_frame_wakes_every_second`<br>`togi_watch::terminal::clock::tests::the_ticker_is_not_restarted_by_a_redraw` | A |
| `TestShowIgnoresUntil` | `togi_watch::terminal::tests::a_caller_owned_feed_and_clock_drive_the_one_loop_with_no_directory_watched`<br>`togi_tool_replay::live::tests::the_feed_folds_the_events_played_so_far_and_only_when_the_count_changes`<br>(Go's caller-ticked `show` redrew on every tick whatever hold the frame asked for, for the replay tool; the Rust loop runs the same with a caller-owned feed and clock and no directory watched: the first test draws a frame that asks to be held three times in a row at the caller's clock, asking the feed for each frame, and the second pins that the replay feed folds only when the count of events played changes) | A |
| `TestRunRestoresCursorOnErrors` | `togi_watch::terminal::show::tests::terminal_failures_name_the_operation_keep_the_cause_and_restore_what_was_set_up`<br>`togi_watch::terminal::show::tests::a_failed_redraw_size_read_is_reported_after_a_good_first_frame`<br>`togi_watch::terminal::tests::the_reader_is_joined_on_every_exit` | A |
| `TestShowKeyboardDispatch` | `togi_watch::terminal::show::tests::keys_switch_views_and_scroll_and_a_view_change_repaints_whole` | A |
| `TestReadKeysFragmentedSequences` | `togi_watch::terminal::keys::tests::every_sequence_decodes_at_every_split_point`<br>`togi_watch::terminal::input::tests::a_split_sequence_waits_a_bounded_time_and_is_one_key` | A |
| `TestReadKeysEscapeTimeoutAndEOF` | `togi_watch::terminal::input::tests::a_quiet_escape_prefix_times_out_into_esc_or_nothing_and_keeps_the_next_key`<br>`togi_watch::terminal::input::tests::end_of_input_flushes_a_lone_escape_and_ends`<br>`togi_watch::terminal::input::tests::pipes::a_lone_escape_becomes_the_esc_key_after_the_timeout`<br>`togi_watch::terminal::input::tests::pipes::a_reader_on_a_real_pipe_decodes_a_sequence_and_reports_the_end` | A |
| `TestShowJoinsKeyboardOnEveryExit` | `togi_watch::terminal::tests::the_reader_is_joined_on_every_exit`<br>`togi_watch::terminal::input::tests::pipes::stopping_an_idle_reader_joins_it_and_leaves_the_callers_pipe_alone` | A |
| `TestShowFragmentedArrowStream` | `togi_watch::terminal::keys::tests::a_byte_by_byte_arrow_is_one_key`<br>`togi_watch::terminal::input::tests::an_ss3_arrow_byte_by_byte_waits_after_each_byte_and_is_one_key`<br>`togi_watch::terminal::keys::tests::a_held_prefix_flushes_as_escape_or_nothing` | A |
| `TestReadKeysCancelsBlockedDelivery` | `togi_watch::terminal::input::tests::a_receiver_that_is_gone_ends_the_reader`<br>(delivery to the loop's unbounded queue never blocks; a reader whose receiver is gone ends) | A |
| `TestRefreshClockArmsNothingForAnIdleTrialHold` | `togi_watch::terminal::clock::tests::a_frozen_frame_arms_nothing_for_hours`<br>`togi_watch::terminal::show::tests::the_clock_redraws_once_a_second_and_a_frozen_frame_never`<br>`togi_watch::terminal::show::tests::evidence_releases_a_frozen_frame_and_the_clock_runs_again`<br>`togi_watch::terminal::notify::tests::a_frozen_frame_is_never_polled_behind` | A |
| `TestShowClearsTheScreenOncePerSizeNotTwiceAtStartup` | `togi_watch::terminal::show::tests::a_resize_clears_and_repaints_the_whole_screen_once`<br>`togi_watch::terminal::show::tests::a_size_change_without_a_signal_is_still_repainted_whole_and_once`<br>`togi_watch::terminal::real_process::real_process_sigint_restores_the_terminal` | A |
| `TestShowRestoresTerminalBeforeAnOutsidePanicPropagates` | `togi_watch::terminal::show::tests::a_panic_in_the_frame_restores_the_terminal_before_it_propagates`<br>`togi_watch::terminal::tests::the_reader_is_joined_on_every_exit`<br>(each Rust call is synchronous, so no panic outside it can happen while it runs) | A |

### `internal/watch/snapshot_test.go` (21)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestProjectTrialLifecycle` | `togi_watch::tests::model::trial_lifecycle` | A |
| `TestProjectTrialStartSelection` | `togi_watch::tests::model::trial_start_selection` | A |
| `TestProjectParkedHunt` | `togi_watch::tests::hunt::a_parked_hunt_trial_shows_its_roles`<br>`togi_watch::tests::hunt::a_parked_group_judges_its_members_and_not_the_parked_cores`<br>`togi_watch::tests::hunt::a_located_hunt_reruns_with_every_unloaded_core_at_zero` | A |
| `TestProjectMemberProbeRows` | `togi_watch::tests::hunt::member_probe_rows` | A |
| `TestFoldEntryFoldsRepeatedOutcomes` | `togi_watch::tests::history::repeated_outcomes_fold_into_one_line`<br>`togi_watch::tests::history::lines_merge_only_with_a_line_that_continues_them` | A |
| `TestProjectBoundsHistory` | `togi_watch::tests::history::history_bounds_and_counts_what_it_leaves_out` | A |
| `TestProjectRawLogRecentSuffix` | `togi_watch::tests::model::the_raw_log_keeps_the_newest_eligible_events_in_order` | A |
| `TestProjectStoppedShowsTunedOffsets` | `togi_watch::tests::model::stopped_shows_the_saved_profile_not_the_applied_offsets` | A |
| `TestProjectSparseCoreIDs` | `togi_watch::tests::model::sparse_core_ids` | A |
| `TestProjectCrashClassification` | `togi_watch::tests::model::crash_classification` | A |
| `TestProjectCrashEndsOnlyItsBootTrial` | `togi_watch::tests::model::a_crash_ends_only_its_boot_trial` | A |
| `TestProjectSMUDeadEndKeepsReadback` | `togi_watch::tests::model::smu_dead_end_keeps_the_readback` | A |
| `TestProjectTrialPreparationDoesNotClaimLoad` | `togi_watch::tests::model::trial_preparation_does_not_claim_load` | A |
| `TestProjectFailureCountersSeparateObservedAndSkipped` | `togi_watch::tests::model::failure_counters_separate_observed_and_skipped`<br>(the record-only half is obsolete: the native journal has no record-only trials) | A |
| `TestProjectRecoveryEndsOnlyWhenTrialStarts` | `togi_watch::tests::model::recovery_ends_only_when_a_trial_starts` | A |
| `TestProjectResetCoreDropsItsSoloLimit` | `togi_watch::tests::model::a_reset_core_drops_its_solo_limit` | A |
| `TestProjectWriteAfterRestorationIsNotSaved` | `togi_watch::tests::model::write_after_restoration_is_not_saved` | A |
| `TestProjectFailedProbeMarksTheProbedMember` | `togi_watch::tests::hunt::a_failed_probe_marks_the_probed_member` | A |
| `TestProjectHuntCausedByCarriedFacts` | `togi_watch::tests::hunt::a_hunt_caused_by_a_carried_trial`<br>`togi_watch::tests::hunt::a_hunt_caused_by_a_carried_idle_failure`<br>`togi_watch::frames::story::tests::the_story::a_carried_idle_failure_cause_is_not_a_trial` | A |
| `TestProjectSoloLimitIsTheOffsetThePassedAlone` | `togi_watch::tests::model::solo_limit_is_the_offset_the_core_passed_alone`<br>(a core that leaves its search without a pass has no solo limit now) | A |
| `TestProjectPhaseAndBIOSProfile` | `togi_watch::tests::model::phase_and_bios_before_phase_one_ends`<br>`togi_watch::frames::panels::search::tests::before_a_confirmed_profile_the_bios_panel_says_so_or_stays_out` | A |

### `internal/watch/source_test.go` (6)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestSourceReloadTransitions` | `togi_watch::terminal::source::tests::reload_follows_a_replaced_grown_rewritten_removed_and_restored_journal`<br>`togi_watch::terminal::source::tests::an_append_folds_only_the_new_events_and_a_replacement_builds_a_fresh_model`<br>`togi_watch::terminal::source::tests::a_journal_rewritten_in_place_to_a_longer_one_builds_a_fresh_model_and_drops_the_held_list` | A |
| `TestLoadIncompatibleJournal` | `togi_watch::terminal::source::tests::a_journal_of_a_newer_schema_is_a_problem_not_a_session`<br>(Rust reads only its own schema; a newer one is a problem frame) | A |
| `TestJournalNotificationsReloadChanges` | `togi_watch::terminal::notify::linux::tests::a_creation_reaches_the_loop_and_is_watched_afterwards`<br>`togi_watch::terminal::notify::linux::tests::a_directory_made_while_the_climb_passes_it_is_still_watched`<br>`togi_watch::terminal::notify::linux::tests::a_missing_state_directory_is_waited_for_through_its_nearest_parent`<br>`togi_watch::terminal::notify::linux::tests::a_removal_reaches_the_loop`<br>`togi_watch::terminal::notify::linux::tests::a_replaced_state_directory_reaches_the_loop`<br>`togi_watch::terminal::notify::linux::tests::a_replacement_reaches_the_loop`<br>`togi_watch::terminal::notify::linux::tests::a_state_directory_below_a_file_is_waited_for_through_the_files_parent`<br>`togi_watch::terminal::notify::linux::tests::a_state_directory_that_can_only_be_traversed_is_watched_through_the_journal`<br>`togi_watch::terminal::notify::linux::tests::a_traverse_only_directory_without_a_journal_is_polled_until_one_can_be_watched`<br>`togi_watch::terminal::notify::linux::tests::an_append_reaches_the_loop`<br>`togi_watch::terminal::notify::linux::tests::an_existing_state_directory_is_watched_with_its_parent_and_never_polled`<br>`togi_watch::terminal::notify::linux::tests::an_unreadable_ancestor_is_polled_behind_only_while_the_state_directory_is_missing`<br>`togi_watch::terminal::notify::linux::tests::notifications_never_ask_for_the_events_of_reading_the_journal`<br>`togi_watch::terminal::notify::linux::tests::relevance_follows_the_journal_the_directory_and_what_is_above_it`<br>`togi_watch::terminal::real_process::real_process_inotify_a_creation_is_shown`<br>`togi_watch::terminal::real_process::real_process_inotify_a_missing_state_directory_is_waited_for`<br>`togi_watch::terminal::real_process::real_process_inotify_a_removal_is_shown_as_no_session`<br>`togi_watch::terminal::real_process::real_process_inotify_a_replaced_state_directory_is_noticed`<br>`togi_watch::terminal::real_process::real_process_inotify_a_replacement_is_shown`<br>`togi_watch::terminal::real_process::real_process_inotify_a_traverse_only_state_directory_is_watched_through_the_journal`<br>`togi_watch::terminal::real_process::real_process_inotify_an_append_is_shown` | A |
| `TestSourceReloadGuard` | `togi_watch::terminal::source::tests::an_unchanged_journal_and_an_unchanged_absence_are_not_reloaded` | A |
| `TestLiveJournalProblemAndRecovery` | `togi_watch::terminal::show::tests::a_live_journal_problem_stays_on_screen_and_recovers_when_the_journal_does`<br>`togi_watch::terminal::source::tests::a_journal_that_does_not_decode_is_a_problem_until_it_does` | A |
| `TestHeldSourceStartsWithTheRunNotTheJournalItFound` | `togi_watch::terminal::source::tests::a_run_in_progress_starts_with_the_run_not_the_journal_it_found`<br>`togi_watch::terminal::source::tests::a_run_in_progress_still_reports_a_journal_it_cannot_read` | A |

### `internal/watch/story_test.go` (12)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestIdleTrialHoldsEveryViewUntilItsEndIsRecorded` | `togi_watch::tests::hunt::a_started_idle_trial_holds_every_frame_until_its_end_is_recorded`<br>`togi_watch::frames::story::tests::the_story::an_idle_trial_holds_the_story_until_its_end_is_recorded`<br>`togi_watch::frames::story::tests::the_simulated_sessions::an_idle_session_holds_still_and_says_when_it_ends`<br>`togi_watch::frames::frame_tests::holds::an_idle_trial_freezes_every_view_until_its_end_is_recorded`<br>`togi_watch::frames::frame_tests::holds::an_idle_trial_shows_its_clock_at_the_trials_start_and_an_unfilled_bar`<br>`togi_watch::terminal::real_process::real_process_a_started_idle_trial_holds_the_dashboard_still` | A |
| `TestTrialIntentDoesNotFreezeBeforeStart` | `togi_watch::frames::story::tests::the_story::a_trial_intent_does_not_claim_the_offsets_are_set`<br>`togi_watch::frames::frame_tests::holds::a_trial_intent_before_its_start_does_not_freeze` | A |
| `TestBetweenTrialsNamesLastOutcomeInsteadOfCountdown` | `togi_watch::tests::model::between_trials_the_last_outcome_and_the_forecast_stand`<br>`togi_watch::frames::story::tests::the_resting_band::between_trials_names_the_last_outcome_and_what_comes_next_not_a_countdown`<br>`togi_watch::frames::story::tests::the_simulated_sessions::between_trials_the_last_outcome_and_the_next_trial_are_named`<br>`togi_watch::frames::frame_tests::bands::between_trials_names_the_last_outcome_and_the_next_action_not_a_countdown` | A |
| `TestStoppedFrameLabelsSavedNotAppliedOffsets` | `togi_watch::tests::model::stopped_shows_the_saved_profile_not_the_applied_offsets`<br>`togi_watch::frames::story::tests::the_story::a_stopped_session_says_why_and_whether_the_offsets_were_restored`<br>`togi_watch::frames::story::tests::the_simulated_sessions::a_stopped_session_labels_saved_not_applied_offsets`<br>`togi_watch::frames::frame_tests::bands::a_stopped_frame_labels_saved_offsets_as_not_applied_and_shows_no_running_stage` | A |
| `TestDeadEndFramePreservesRecordedCause` | `togi_watch::frames::story::tests::the_story::a_dead_end_preserves_the_recorded_cause`<br>`togi_watch::frames::story::tests::the_story::a_dead_end_detail_stays_raw_in_the_band_and_out_of_the_narration`<br>`togi_watch::frames::story::tests::the_simulated_sessions::a_dead_end_session_keeps_its_recorded_detail`<br>`togi_watch::frames::frame_tests::bands::a_dead_end_frame_keeps_the_recorded_cause_and_invents_no_diagnosis` | A |
| `TestDeepeningChecksSayHowTheyLoad` | `togi_watch::frames::story::tests::the_story::deepening_checks_say_how_they_load`<br>`togi_watch::frames::story::tests::the_story::a_round_names_the_cores_it_moves` | A |
| `TestSplitWordsParkAtZeroOnlyWhenTheProfileDoes` | `togi_watch::frames::story::tests::the_story::a_split_parks_at_zero_only_when_the_profile_does`<br>`togi_watch::frames::story::tests::the_story::a_hunt_without_a_running_part_says_how_the_rest_is_parked` | A |
| `TestCarriedIdleFailureCauseIsNotATrial` | `togi_watch::tests::hunt::a_hunt_caused_by_a_carried_idle_failure`<br>`togi_watch::frames::story::tests::the_story::a_carried_idle_failure_cause_is_not_a_trial` | A |
| `TestHuntCauseBriefAgreesWithNamedCore` | `togi_watch::frames::story::tests::the_story::a_hunt_cause_brief_agrees_with_the_named_core`<br>`togi_watch::frames::story::tests::the_story::a_hunt_cause_names_what_failed_and_how_late` | A |
| `TestStoryPartialExplainsOrdinaryEvidence` | `togi_watch::frames::story::tests::the_story::a_partial_explains_that_it_is_ordinary_evidence` | A |
| `TestTightNextKeepsTrialTargets` | `togi_watch::frames::story::tests::the_outcome_lines::tight_next_keeps_trial_targets` | A |
| `TestTightNextLeavesNonCoreOutcomesUnchanged` | `togi_watch::frames::story::tests::the_outcome_lines::tight_next_leaves_non_core_outcomes_unchanged` | A |

## watchtest

`internal/watch/watchtest` has no test files: `fixture.go` is test support (`Install`), exercised through the status tests. Its Rust counterpart `togi-watchtest` tests what it builds (its `scenarios`, `determinism`, `install`, `helpers` and `fixtures` tests); the rows above cite those tests where a watch test relied on a fixture's property.

### `tools/replay/main_test.go` (10)

The Rust tests are the unit tests of `togi-tool-replay` (`togi_tool_replay::tests::{cli,frames,load,playback}` and `togi_tool_replay::live::tests`) and its `replay` integration binary, which runs the built binary, two of its tests on a pseudo-terminal.

| Go test | Rust test(s) | status |
|---|---|---|
| `TestPlaybackWallClockCorrections` | `togi_tool_replay::tests::playback::a_wall_clock_correction_in_the_recording_costs_no_playback_time` | A |
| `TestPlaybackZeroMono` | `togi_tool_replay::tests::playback::a_monotonic_zero_is_a_stamp_not_a_missing_one` | A |
| `TestPlaybackFallback` | `togi_tool_replay::tests::playback::without_two_monotonic_stamps_of_one_known_boot_the_wall_clock_paces_the_gap` | A |
| `TestPlaybackBackwardFallbackRetainsFollowingInterval` | `togi_tool_replay::tests::playback::a_backward_wall_clock_gap_is_free_and_the_next_interval_is_kept`<br>`togi_tool_replay::tests::playback::a_monotonic_clock_that_ran_backwards_within_a_boot_is_free_too` | A |
| `TestPlaybackFrom` | `togi_tool_replay::tests::playback::playback_starts_at_the_prefix_a_sequence_number_names`<br>`togi_tool_replay::tests::playback::the_prefix_holds_the_events_up_to_a_sequence_number_and_at_least_the_first`<br>`togi_tool_replay::tests::playback::a_start_past_the_end_or_before_the_first_event_is_held_inside_the_recording`<br>`togi_tool_replay::tests::cli::the_defaults_play_from_the_first_event_at_300_times` | A |
| `TestPlaybackSpeed` | `togi_tool_replay::tests::playback::the_speed_scales_real_time_and_saturates_instead_of_overflowing`<br>`togi_tool_replay::tests::playback::playing_past_the_last_event_keeps_the_clock_running_from_it` | A |
| `TestRunRejectsInvalidSpeedBeforeReading` | `togi_tool_replay::tests::playback::a_speed_must_be_finite_and_positive`<br>`togi_tool_replay::tests::cli::an_invalid_speed_is_rejected_when_the_arguments_are_read_before_any_journal_is`<br>`togi_tool_replay::replay::an_invalid_speed_is_a_usage_error_before_the_journal_is_read` | A |
| `TestRunFrameRetainsRecordedClock` | `togi_tool_replay::tests::frames::the_frame_is_drawn_at_the_last_events_wall_time_plus_after`<br>`togi_tool_replay::tests::frames::a_frame_is_the_state_as_of_the_event_not_the_end_of_the_recording`<br>`togi_tool_replay::tests::frames::the_main_view_of_a_prefix_is_printed_as_plain_text`<br>`togi_tool_replay::tests::frames::the_help_and_log_views_are_printed_without_the_key_footer`<br>`togi_tool_replay::tests::cli::at_prints_one_frame_of_the_recording_in_the_system_time_zone`<br>`togi_tool_replay::tests::cli::every_page_can_be_printed`<br>`togi_tool_replay::replay::at_prints_a_frame_of_a_recording_and_nothing_else`<br>(Go's journal had a wall clock that jumped back; the Rust frame is drawn at the selected event's own wall time plus `--after`, a backward gap of the recording is the playback tests' subject) | A |
| `TestRunFrameCurrentRulesetAllSchemas` | `togi_tool_replay::tests::frames::every_scenario_can_be_replayed_to_its_last_event`<br>`togi_tool_replay::tests::load::the_events_of_a_recording_are_read_whole`<br>`togi_tool_replay::tests::load::a_recording_of_another_ruleset_or_a_newer_schema_is_refused`<br>(Rust reads only its own schema, so schemas 1 to 3 of the Go test have no counterpart; a recording of the current schema and ruleset plays to its last event, and an older or newer one is refused) | A |
| `TestRunFrameShippedSchemas` | `togi_tool_replay::tests::load::a_recording_of_another_ruleset_or_a_newer_schema_is_refused`<br>`togi_tool_replay::tests::load::a_refusal_comes_before_any_later_event_is_decoded` | O(the test played the Go-written recordings under `tools/replay/testdata` and `internal/carry/testdata`, which the native Rust build does not read; the refusal tests are what still applies of it) |
