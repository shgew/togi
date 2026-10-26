# Stage 5 Go-test mapping

Every `func Test*`, `Benchmark*`, `Fuzz*` and `Example*` in the Go packages ported in stage 5 (#640) is mapped to the Rust tests that pin the same behavior: `internal/hostlock` → `togi-hostlock`, `internal/smu` → `togi-smu`, `internal/detect` → `togi-detect`, `internal/backend`, `internal/backend/mprime` and `internal/backend/ycruncher` → `togi-backend`, `togi-mprime` and `togi-ycruncher`, `internal/trial` → `togi-trial`, `internal/hardware` → `togi-hardware`. `togi-command` has no Go package (Go started its commands with `exec.CommandContext`), so it has no rows; its tests appear below only where they pin a behavior a Go test proved. There are no Fuzz, Example or Benchmark functions. Files tagged `integration` or `hardware` are included.

Rust names are `crate::module::fn`; an integration test binary adds its name after the crate (`togi_trial::hardware::hardware::…`). Each name resolves to a test that `cargo nextest list --workspace` reports on `main` at 34c29cd4 with this pull request's 28 added tests (27 functions that run in the default suite, listed in the pull request, and `togi_trial::hardware::hardware::the_scope_probe_creates_a_confined_scope_on_the_real_manager`); a parameterised `#[test_case]` function counts by its name. The names under `togi_smu::hardware`, `togi_detect::hardware`, `togi_trial::hardware`, `togi_trial::cached_errors` and `togi_hardware::hardware` are tests of the `hardware` feature: `cargo nextest list --features hardware` reports them, the default suite does not build them, and `just hardware-rs` runs them as root under the host lock on the target machine. The trial-scope VM check ([#809](https://github.com/shgew/togi/pull/809)) runs the `togi-trial` ones under real systemd; the rest have not run anywhere. No row names a test that only a non-Linux build lists (`togi_hardware::tests::other_platforms_refuse_hardware_runs` pins the unsupported-platform stub and has no Go twin). The pull requests that ported each package are #761 (hostlock), #771 (smu), #779 (detect), #778 (backends), #786 (trial, with #795 and #807) and #806 (hardware).

Status:

- **A**: a test of the port's own crate pins the behavior.
- **X**: a test of another Rust crate pins it.
- **O(reason)**: obsolete under the native contract.
- **G(reason)**: not pinned by any Rust test and not obsolete.

## Summary

| Package | Go tests | A | X | O | G |
|---|---:|---:|---:|---:|---:|
| hostlock | 10 | 10 | 0 | 0 | 0 |
| smu | 27 | 26 | 0 | 1 | 0 |
| detect | 33 | 32 | 0 | 1 | 0 |
| backend, mprime and ycruncher | 16 | 15 | 1 | 0 | 0 |
| trial | 111 | 107 | 2 | 2 | 0 |
| hardware | 23 | 23 | 0 | 0 | 0 |
| **total** | **220** | **213** | **3** | **4** | **0** |

## hostlock

### `internal/hostlock/hostlock_test.go` (10)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestExclusiveUntilClosed` | `togi_hostlock::tests::exclusive_until_dropped`<br>`togi_hostlock::tests::real_process_excludes_until_it_exits` | A |
| `TestLegacyLockMigrationPreservesInodeAndContents` | `togi_hostlock::tests::legacy_locks_are_tightened_in_place`<br>`togi_hostlock::policy::tests::only_the_owner_restricts_legacy_modes` | A |
| `TestLegacyReadOnlyHolderRemainsExclusiveDuringMigration` | `togi_hostlock::tests::legacy_read_only_holder_stays_exclusive_through_migration` | A |
| `TestProvisionedGroupLockPreservesPermissions` | `togi_hostlock::tests::provisioned_group_lock_keeps_its_mode`<br>`togi_hostlock::tests::host_policy_leaves_provisioning_to_root` | A |
| `TestConcurrentCreationHasOneOwner` | `togi_hostlock::tests::concurrent_creators_yield_one_owner` | A |
| `TestUnsafeLockObjectsAreNotModified` | `togi_hostlock::tests::unsafe_lock_objects_are_refused_untouched`<br>`togi_hostlock::tests::hardlinked_lock_is_named_as_multiply_linked`<br>`togi_hostlock::policy::tests::irregular_or_linked_objects_are_refused_before_ownership` | A |
| `TestWritableParentNeedsStickyBit` | `togi_hostlock::tests::writable_ancestor_needs_the_sticky_bit` | A |
| `TestDirectoryAuthorization` | `togi_hostlock::policy::tests::directory_authorization_matrix` | A |
| `TestLockOpenFailureCreatesNoDirectory` | `togi_hostlock::tests::missing_parent_is_not_created`<br>`togi_hostlock::tests::create_permission_failure_is_reported_as_create` | A |
| `TestReadOnlyDescriptorIsNotDelegation` | `togi_hostlock::tests::read_only_descriptor_to_a_secure_lock_is_refused_without_chmod`<br>`togi_hostlock::policy::tests::secure_modes_need_read_write_access` | A |

## smu

### `internal/smu/driver_test.go` (18)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestMain` | — | O(Go shared one fixture directory across tests and removed it after `m.Run`; each Rust test builds its own `Tree` in a `tempfile` directory that drops with the test) |
| `TestTopologyAndEncoding` | `togi_smu::tests::driver::topology_groups_online_cpus_and_encodes_offsets`<br>`togi_smu::tests::driver::core_absent_from_a_single_ccd_topology_is_unknown` | A |
| `TestMappingAndFallback` | `togi_smu::tests::slots::mapping_and_fallback` | A |
| `TestSlotIdentityPreflight` | `togi_smu::tests::slots::slot_identity_preflight`<br>`togi_smu::tests::slots::a_raw_core_id_outside_0_to_15_is_a_typed_topology_error` | A |
| `TestStaleCCDFuseRefusesPerCoreAccess` | `togi_smu::tests::slots::stale_ccd_fuse_read_refuses_per_core_access` | A |
| `TestFuseEqualToProbeAcceptedWhenFresh` | `togi_smu::tests::slots::fuse_equal_to_a_probe_is_accepted_when_fresh` | A |
| `TestNoDriverAndTopologyError` | `togi_smu::tests::driver::without_the_driver_directory_nothing_validates`<br>`togi_smu::tests::driver::missing_topology_is_an_open_error` | A |
| `TestConstructionRefusesUnsupportedIdentityWithoutAccess` | `togi_smu::tests::driver::unsupported_identity_never_reaches_hardware`<br>`togi_smu::tests::safety::a_driver_that_failed_its_identity_check_opens_no_mailbox_or_pm_table_file` | A |
| `TestUnvalidatedDriverRefusesEveryAccess` | `togi_smu::tests::safety::nothing_reaches_the_mailbox_before_validation`<br>`togi_smu::tests::safety::an_unvalidated_driver_opens_no_mailbox_or_pm_table_file`<br>`togi_smu::tests::slots::open_driver_has_no_slot_map_until_validated` | A |
| `TestOpenIdentityChecksWithoutMailbox` | `togi_smu::tests::driver::identity_only_driver_checks_but_never_opens_the_mailbox`<br>`togi_smu::tests::safety::an_identity_only_driver_refuses_every_entry_point`<br>`togi_smu::tests::safety::an_identity_only_driver_opens_no_mailbox_or_pm_table_file` | A |
| `TestCPUDriverReadbackAndBIOS` | `togi_smu::tests::driver::checks_report_exact_details_and_bios_context`<br>`togi_smu::tests::driver::bios_strings_are_lossy_utf8` | A |
| `TestSysfsMailbox` | `togi_smu::tests::sysfs::an_absent_driver_directory_is_not_a_mailbox`<br>`togi_smu::tests::sysfs::statuses_are_typed`<br>`togi_smu::tests::sysfs::arguments_round_trip_in_24_little_endian_bytes`<br>`togi_smu::tests::sysfs::typed_commands_write_their_code`<br>`togi_smu::tests::sysfs::smn_reads_write_the_address_and_read_the_value`<br>`togi_smu::tests::sysfs::a_response_of_the_wrong_length_is_refused` | A |
| `TestInvalidIdentityFilesPreventHardwareAccess` | `togi_smu::tests::driver::invalid_identity_files_prevent_hardware_access`<br>`togi_smu::tests::driver::malformed_cpu_identity_keeps_the_failing_field_and_its_cause` | A |
| `TestMailboxWriteFailureIsReported` | `togi_smu::tests::driver::mailbox_failures_keep_the_cause_and_the_clamped_argument` | A |
| `TestFuseReadFailureRefusesPerCoreAccess` | `togi_smu::tests::slots::fuse_read_failure_at_each_stage_refuses_per_core_access` | A |
| `TestTopologyRejectsMissingOrMalformedIdentity` | `togi_smu::tests::driver::malformed_topology_files_are_open_errors`<br>`togi_smu::tests::driver::missing_topology_files_are_open_errors`<br>`togi_smu::tests::driver::no_online_cpu_is_an_open_error` | A |
| `TestBIOSContextReadFailures` | `togi_smu::tests::driver::bios_context_read_failures_keep_their_cause` | A |
| `TestSysfsRejectsUnavailableMailboxFiles` | `togi_smu::tests::sysfs::unavailable_mailbox_files_fail_at_the_open_stage`<br>`togi_smu::tests::sysfs::statuses_are_typed` | A |

`TestTopologyAndEncoding` also checked that mutating the returned topology did not change the driver's copy; `Driver::topology` builds and returns an owned `Vec`, so there is nothing to alias. In `TestOpenIdentityChecksWithoutMailbox` Go's `ValidateSMU` succeeded on an identity-only driver; Rust's `validate` returns `IdentityOnly` instead, so an identity-only driver can never yield the `SmuValidated` proof. `TestSlotIdentityPreflight`'s refusals are pinned with Rust's wording, which names every core whose CCD disagrees with core ID / 8, not only those at the wrong modulo-eight slot.

### `internal/smu/hardware_test.go` (1)

`hardware && linux`; the Rust test needs `--features hardware`.

| Go test | Rust test(s) | status |
|---|---|---|
| `TestHardwareSMU` | `togi_smu::hardware::hardware_smu_round_trips_every_offset_and_restores_them` | A |

### `internal/smu/pm_table_hardware_test.go` (1)

`hardware && linux`; the Rust test needs `--features hardware`.

| Go test | Rust test(s) | status |
|---|---|---|
| `TestHardwarePMTable` | `togi_smu::hardware::hardware_pm_table_lanes_decode_and_residencies_sum_to_100` | A |

### `internal/smu/pm_table_linux_test.go` (5)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestReadPMTable` | `togi_smu::tests::pm::reads_cover_the_version_and_table_failures`<br>`togi_smu::tests::pm::the_version_is_exactly_four_little_endian_bytes`<br>`togi_smu::tests::pm::a_decoded_table_is_reported_with_its_version` | A |
| `TestPMTableTopology` | `togi_smu::tests::pm::layout_needs_cores_0_to_15_on_ccd_index_over_8`<br>`togi_smu::tests::pm::an_unsupported_topology_reads_the_version_but_no_payload` | A |
| `TestPMTableBlockingRead` | `togi_smu::tests::pm::a_stuck_read_is_never_replaced_and_its_late_result_is_discarded`<br>`togi_smu::tests::pm::check_waits_the_read_budget_for_a_stuck_read_and_stays_informational`<br>`togi_smu::tests::pm::check_never_waits_longer_than_what_is_left_of_the_read_budget` | A |
| `TestPMTableFreshness` | `togi_smu::tests::pm::a_cached_table_is_reused_for_two_seconds_from_its_read_start` | A |
| `TestPMTableCheckRefreshesCompletedRead` | `togi_smu::tests::pm::check_refreshes_a_completed_read` | A |

### `internal/smu/pm_table_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestDecodePMTable` | `togi_smu::tests::pm::decode_reads_six_lane_blocks_and_ignores_unknown_fields`<br>`togi_smu::tests::pm::decode_refuses_any_other_size`<br>`togi_smu::tests::pm::reads_cover_the_version_and_table_failures`<br>`togi_smu::tests::pm::layout_needs_cores_0_to_15_on_ccd_index_over_8` | A |
| `TestDecodePMTableRejectsNonfiniteLanes` | `togi_smu::tests::pm::decode_rejects_a_non_finite_lane_in_every_block` | A |

## detect

### `internal/detect/cursor_test.go` (6)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestCursorReadsRetainEmptyIntervals` | `togi_detect::tests::cursor::cursor_reads_keep_empty_intervals_resumable` | A |
| `TestCursorRejectsNearestEntryAndOtherBoot` | `togi_detect::tests::cursor::a_cursor_that_is_not_the_first_row_or_in_another_boot_is_unresumable` | A |
| `TestCursorReadErrorRetainsMCEEvidencedBeforeMalformedEntry` | `togi_detect::tests::cursor::a_malformed_entry_keeps_the_earlier_checks_and_no_cursor` | A |
| `TestCursorMetadataLossRetainsOnlyValidatedMCEs` | `togi_detect::tests::cursor::lost_metadata_keeps_only_the_checks_read_before_the_bad_row` | A |
| `TestKernelObservationFailuresDoNotBecomeCleanIntervals` | `togi_detect::tests::cursor::observation_failures_never_become_clean_intervals`<br>`togi_detect::tests::cursor::a_failed_read_keeps_the_error_kind_and_the_command_cause` | A |
| `TestMalformedKernelMessagesRetainOnlyEarlierEvidence` | `togi_detect::tests::cursor::malformed_messages_keep_only_earlier_evidence`<br>`togi_detect::tests::kernel::malformed_entries_are_errors_not_clean_intervals` | A |

`TestCursorMetadataLossRetainsOnlyValidatedMCEs` also expects Go's partial read to keep the cursor of the last valid row. In Rust, a failed `read_mces` returns `Partial { value, error }` and never returns a cursor, so a failed interval cannot be resumed. The checks read before the bad row are kept and pinned.

### `internal/detect/deadline_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestKernelReadTimeout` | `togi_detect::tests::deadline::every_seam_call_starts_its_command_with_the_whole_budget`<br>`togi_detect::tests::deadline::a_command_that_times_out_leaves_no_observation`<br>`togi_detect::tests::deadline::the_commands_of_one_call_share_one_budget`<br>`togi_detect::tests::deadline::later_commands_get_what_is_left_of_the_budget`<br>`togi_detect::tests::deadline::a_used_up_budget_runs_nothing`<br>`togi_detect::tests::deadline::every_seam_call_that_times_out_keeps_the_timeout_as_its_cause` | A |

### `internal/detect/detect_test.go` (6)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestCapturedBlocks` | `togi_detect::tests::parse::captured_blocks_keep_every_line_and_the_status_time` | A |
| `TestSMCALongNames` | `togi_detect::tests::parse::every_smca_long_name_is_a_named_bank_or_other` | A |
| `TestStatusAndBlockBoundaries` | `togi_detect::tests::parse::statuses_decode_corrected_deferred_and_raw_bits`<br>`togi_detect::tests::parse::unknown_cpu_and_unrelated_lines_close_the_record` | A |
| `TestRecordContinuationBoundaries` | `togi_detect::tests::parse::truncated_and_interleaved_records_attribute_conservatively`<br>`togi_detect::tests::parse::a_surplus_bank_line_withdraws_every_tentative_record_back_to_the_first_unresolved`<br>`togi_detect::tests::parse::a_truncated_record_keeps_its_own_lines_and_no_bank` | A |
| `TestInterleavedBankAttribution` | `togi_detect::tests::parse::interleaved_orders_are_unknown_and_sequential_records_are_attributed` | A |
| `TestJournalBootAndByteArray` | `togi_detect::tests::kernel::a_message_may_be_a_byte_array`<br>`togi_detect::tests::kernel::missing_boot_no_match_and_failure_are_told_apart`<br>`togi_detect::tests::kernel::whole_boot_reads_the_grep_query_of_the_boot_without_hyphens` | A |

### `internal/detect/hardware_test.go` (1)

`hardware && linux`; the Rust test needs `--features hardware`.

| Go test | Rust test(s) | status |
|---|---|---|
| `TestHardwareKernelLog` | `togi_detect::hardware::the_host_journal_answers_for_its_boots` | A |

### `internal/detect/journalctl_integration_test.go` (2)

`integration && linux`; the Rust tests are Linux-only and run in the default suite.

| Go test | Rust test(s) | status |
|---|---|---|
| `TestHungJournalctlHelper` | — | O(re-exec helper that stood in for a hung `journalctl`; the Rust tests write a shell-script program to a temporary directory and run it by path) |
| `TestJournalctlKilledAtDeadline` | `togi_detect::tests::process::real_process_a_hung_journalctl_is_killed_at_the_deadline`<br>`togi_detect::tests::process::real_process_a_descendant_holding_the_pipes_cannot_extend_the_deadline`<br>`togi_detect::tests::process::real_process_a_hung_command_of_a_later_step_is_bounded_by_the_same_budget` | A |

### `internal/detect/pstore_span_test.go` (3)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestPstoreRequiresKnownBootSpan` | `togi_detect::tests::pstore::an_unknown_boot_span_records_no_dump` | A |
| `TestMatchingPstoreReadError` | `togi_detect::tests::pstore::unsafe_archive_entries_are_refused_from_the_descriptor`<br>`togi_detect::tests::pstore::a_failed_open_of_a_matching_dump_keeps_its_io_cause` | A |
| `TestPstoreSurvivesLostJournalTail` | `togi_detect::tests::pstore::a_dump_survives_a_lost_journal_tail` | A |

### `internal/detect/pstore_test.go` (4)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestSavedPstore` | `togi_detect::tests::pstore::a_record_in_the_boots_wall_clock_span_is_selected` | A |
| `TestPstoreTailBounds` | `togi_detect::tests::pstore::tails_keep_the_last_32_lines_and_drop_a_cut_first_line`<br>`togi_detect::tests::pstore::the_tail_window_is_the_budget_minus_the_header` | A |
| `TestSavedPstoreErrors` | `togi_detect::tests::pstore::a_root_that_is_not_a_directory_is_an_error`<br>`togi_detect::tests::pstore::a_boot_list_that_cannot_be_read_is_an_error_not_an_absent_record` | A |
| `TestPstoreHeaderIdentityAndBudget` | `togi_detect::tests::pstore::the_header_of_every_matching_chunk_must_identify_its_dump`<br>`togi_detect::tests::pstore::the_tail_window_is_the_budget_minus_the_header` | A |

### `internal/detect/reset_test.go` (10)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestResetReason` | `togi_detect::tests::reset::reset_reason_kinds_priority_and_support` | A |
| `TestResetReasonMissingBoot` | `togi_detect::tests::reset::a_vacuumed_boot_has_no_reset_reason` | A |
| `TestResetReasonWithoutMatchingLines` | `togi_detect::tests::reset::a_boot_without_matching_lines_is_unsupported_and_empty` | A |
| `TestResetReasonAfter` | `togi_detect::tests::reset::the_reset_reason_after_a_boot_is_its_immediate_successors`<br>`togi_detect::tests::reset::a_successor_that_cannot_be_found_or_read_is_a_missing_boot` | A |
| `TestResetReasonAfterRequiresContinuousJournal` | `togi_detect::tests::reset::succession_needs_consecutive_sequence_numbers_of_one_journal`<br>`togi_detect::tests::reset::positions_follow_each_other_by_exactly_one` | A |
| `TestResetReasonAfterBoundaryErrors` | `togi_detect::tests::reset::boundary_failures_are_told_apart` | A |
| `TestResetReasonAfterWithoutSystemJournal` | `togi_detect::tests::reset::an_empty_system_journal_means_the_boot_is_missing` | A |
| `TestResetReasonAfterBootListErrors` | `togi_detect::tests::reset::a_boot_list_that_cannot_be_read_is_never_clean_or_missing` | A |
| `TestMCEsMonotonic` | `togi_detect::tests::kernel::the_monotonic_lower_bound_decides_which_checks_are_eligible` | A |
| `TestResetReasonMalformedOutputDoesNotInventReason` | `togi_detect::tests::reset::malformed_output_never_invents_a_reason` | A |

## backends

### `internal/backend/hardware_test.go` (1)

Build tag `hardware && linux`; the Rust twin is a `togi-trial` test that needs `--features hardware`.

| Go test | Rust test(s) | status |
|---|---|---|
| `TestHardwareWorkloads` | `togi_trial::hardware::hardware::every_r1_and_r2_workload_passes_twenty_seconds_on_cpu_2` | X |

### `internal/backend/mprime/mprime_test.go` (7)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestMissingBinary` | `togi_mprime::tests::missing_binary_is_backend_missing`<br>`togi_backend::tests::only_absence_maps_to_backend_missing` | A |
| `TestPrepare` | `togi_mprime::tests::identity_and_detail`<br>`togi_mprime::tests::plans_match_exactly`<br>`togi_mprime::tests::sse_single_cpu_local_txt_is_literal`<br>`togi_mprime::tests::thread_count_and_hyperthreading_follow_the_cpu_list`<br>`togi_mprime::tests::unsupported_workload_is_refused_before_the_binary_is_checked`<br>`togi_mprime::tests::a_plan_needs_a_cpu` | A |
| `TestPrepareCatalogWorkloads` | `togi_mprime::tests::every_catalog_workload_plans` | A |
| `TestCheckNonExecutable` | `togi_mprime::tests::non_executable_binary_fails_check`<br>`togi_backend::tests::file_without_execute_bit_is_refused` | A |
| `TestClassify` | `togi_mprime::tests::every_computation_alternative_is_recognised_in_any_case`<br>`togi_mprime::tests::every_setup_alternative_is_recognised`<br>`togi_mprime::tests::progress_keeps_the_reported_spelling`<br>`togi_mprime::tests::ordinary_lines_are_other`<br>`togi_mprime::tests::failure_outranks_setup_outranks_progress`<br>`togi_mprime::tests::pass_stdout_fixture_progresses_without_errors`<br>`togi_mprime::tests::pass_results_fixture_has_no_errors`<br>`togi_mprime::tests::spliced_errors_are_not_erased_by_later_success` | A |
| `TestPrepareFailureProducesNoLaunch` | `togi_mprime::tests::missing_binary_yields_no_plan`<br>`togi_mprime::tests::planning_writes_nothing`<br>`togi_trial::files::tests::an_input_named_like_a_log_or_repeated_fails_exclusively` | A |
| `TestCheckStatErrorIsNotMissingBackend` | `togi_mprime::tests::stat_failure_is_not_backend_missing`<br>`togi_backend::tests::cyclic_symlink_is_a_stat_error_not_missing` | A |

The input cases of `TestPrepareFailureProducesNoLaunch` (a directory at `prime.txt` or `local.txt`) move to `togi-trial`: `plan` writes nothing, and the trial creates each input exclusively in a fresh instance directory, failing the start with the input named.

### `internal/backend/ycruncher/ycruncher_test.go` (8)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestMissingBinaries` | `togi_ycruncher::tests::missing_binaries_directory_is_backend_missing`<br>`togi_ycruncher::tests::missing_zen5_fails_check_and_every_plan` | A |
| `TestCheckBinaries` | `togi_ycruncher::tests::identity_and_detail`<br>`togi_ycruncher::tests::both_chosen_binaries_must_be_executable_regular_files`<br>`togi_ycruncher::tests::unrelated_entries_are_ignored`<br>`togi_ycruncher::tests::creation_order_does_not_decide_the_choice`<br>`togi_ycruncher::tests::select_picks_the_lexical_minimum_whatever_the_order`<br>`togi_ycruncher::tests::select_ignores_names_that_are_not_binaries`<br>`togi_ycruncher::tests::select_can_find_zen5_without_a_lower_binary`<br>`togi_ycruncher::tests::select_compares_non_utf8_names_bytewise`<br>`togi_ycruncher::tests::non_utf8_names_are_selected_and_checked`<br>`togi_ycruncher::tests::only_a_non_matching_non_utf8_name_is_no_binary` | A |
| `TestPrepare` | `togi_ycruncher::tests::identity_and_detail`<br>`togi_ycruncher::tests::plans_match_exactly`<br>`togi_ycruncher::tests::config_is_literal_native_syntax`<br>`togi_ycruncher::tests::memory_is_32_mib_per_cpu`<br>`togi_ycruncher::tests::unsupported_workload_is_refused_before_discovery`<br>`togi_ycruncher::tests::a_plan_needs_a_cpu` | A |
| `TestPrepareCatalogWorkloads` | `togi_ycruncher::tests::every_catalog_workload_plans` | A |
| `TestMissingZen5` | `togi_ycruncher::tests::missing_zen5_fails_check_and_every_plan` | A |
| `TestClassify` | `togi_ycruncher::tests::affinity_cpu_is_parsed_after_stripping_colour`<br>`togi_ycruncher::tests::affinity_without_a_number_is_not_an_affinity_error`<br>`togi_ycruncher::tests::every_computation_alternative_is_recognised`<br>`togi_ycruncher::tests::every_setup_alternative_is_recognised`<br>`togi_ycruncher::tests::progress_names_the_test`<br>`togi_ycruncher::tests::carriage_return_separated_progress`<br>`togi_ycruncher::tests::ordinary_lines_are_other`<br>`togi_ycruncher::tests::precedence_is_affinity_computation_setup_progress`<br>`togi_ycruncher::tests::passing_fixtures_report_progress`<br>`togi_ycruncher::tests::setup_fixture_reports_setup_error`<br>`togi_ycruncher::tests::affinity_fixture_reports_the_cpu`<br>`togi_ycruncher::tests::spliced_errors_are_not_erased_by_earlier_success` | A |
| `TestBinaryDiscoveryErrors` | `togi_ycruncher::tests::no_matching_names_is_backend_missing`<br>`togi_ycruncher::tests::dangling_chosen_binary_is_backend_missing`<br>`togi_ycruncher::tests::cyclic_chosen_binary_is_not_backend_missing`<br>`togi_ycruncher::tests::binaries_path_that_is_a_file_is_not_backend_missing`<br>`togi_ycruncher::tests::unreadable_binaries_directory_is_not_backend_missing`<br>`togi_backend::tests::absent_binary_is_missing` | A |
| `TestPrepareInputFailureProducesNoLaunch` | `togi_ycruncher::tests::planning_writes_nothing`<br>`togi_trial::files::tests::an_input_named_like_a_log_or_repeated_fails_exclusively` | A |

## trial

### `internal/trial/compress_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestTrialEndCompressesSamplesAndBackendLogs` | `togi_trial::tests::runner::ending_a_trial_compresses_it_and_leaves_every_older_retained_trial_untouched`<br>`togi_trial::tests::runner::the_runner_sweeps_probes_compresses_and_reads_samples`<br>`togi_trialfiles::tests::compress_covers_samples_and_every_instance_log`<br>`togi_trialfiles::tests::open_prefers_the_plain_file_and_falls_back_to_gzip` | A |

### `internal/trial/deadline_integration_test.go` (1)

`integration && linux`.

| Go test | Rust test(s) | status |
|---|---|---|
| `TestProcessScopeSystemctlDeadline` | `togi_trial::tests::process::real_process_systemctl_calls_are_bounded_and_a_timed_out_missing_scope_text_is_not_success`<br>`togi_command::run::real_process_hung_child_is_killed_at_the_deadline` | A |

### `internal/trial/hardware_deadline_test.go` (1)

`hardware && linux`: the Rust tests need `--features hardware`, a host lock and a scope-capable host.

| Go test | Rust test(s) | status |
|---|---|---|
| `TestHardwareScopeOversizedOutput` | `togi_trial::hardware::hardware::oversized_output_on_every_source_is_inconclusive_and_the_scope_ends`<br>`togi_trial::tests::process::real_process_oversized_output_on_any_source_is_inconclusive_with_the_exact_prefix_and_no_containment` | A |

### `internal/trial/hardware_identity_test.go` (1)

`hardware && linux`.

| Go test | Rust test(s) | status |
|---|---|---|
| `TestHardwareScopeBackendIdentity` | `togi_trial::hardware::hardware::a_backend_runs_as_the_account_with_no_groups_and_only_its_own_files_writable` | A |

### `internal/trial/hardware_recovery_test.go` (2)

`hardware && linux`.

| Go test | Rust test(s) | status |
|---|---|---|
| `TestHardwareScopePartialStartRollback` | `togi_trial::hardware::hardware::a_rolled_back_start_is_confirmed_and_both_scopes_end` | A |
| `TestHardwareScopeSweepOrphan` | `togi_trial::hardware::hardware::the_sweep_removes_orphaned_trial_scopes_and_keeps_every_other_scope`<br>`togi_trial::hardware::hardware::stray_cleanup_accepts_an_unloaded_scope_only_once_it_is_gone`<br>`togi_trial::hardware::hardware::stray_cleanup_preserves_kill_reap_and_scope_failures`<br>`togi_trial::hardware::hardware::stray_cleanup_stops_the_scope_after_a_kill_panic`<br>`togi_trial::hardware::hardware::stray_cleanup_stops_the_scope_after_a_reap_timeout` | A |

The `stray_cleanup_*` tests pin the cleanup helpers of the hardware tests themselves (Go's `cleanup` closures).

### `internal/trial/hardware_test.go` (2)

`hardware && linux`.

| Go test | Rust test(s) | status |
|---|---|---|
| `TestHardwareScope` | `togi_trial::hardware::hardware::a_helper_cannot_pin_outside_its_scope_and_the_scope_ends_with_the_trial`<br>`togi_trial::hardware::hardware::the_scope_probe_creates_a_confined_scope_on_the_real_manager` | A |
| `TestHardwareScopeKillsDetachedDescendant` | `togi_trial::hardware::hardware::a_detached_descendant_dies_with_its_scope` | A |

### `internal/trial/helper_test.go` (1)

`(integration || hardware) && linux`.

| Go test | Rust test(s) | status |
|---|---|---|
| `TestHelperProcess` | — | O(test infrastructure, not a test: the Go re-exec entry point; the Rust helpers are `tests::process::helper`, `launch::tests::attributes::launch_attributes_helper` and `hardware::hardware::helper`, dispatched through `helper_command`) |

### `internal/trial/identity_test.go` (10)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestLookupIdentity` | `togi_trial::identity::tests::an_ordinary_account_resolves_to_its_uid_and_primary_gid`<br>`togi_trial::identity::tests::an_empty_or_unknown_account_is_refused_not_defaulted`<br>`togi_trial::identity::tests::root_is_refused_by_name`<br>`togi_trial::identity::tests::ids_are_bounded_away_from_root_and_the_unchanged_sentinel` | A |
| `TestScopeArgv` | `togi_trial::launch::tests::the_scope_argv_is_exactly_the_documented_command`<br>`togi_trial::launch::tests::a_user_manager_adds_the_user_flag_first` | A |
| `TestScopeInstanceOwnership` | `togi_trial::files::tests::the_inputs_and_then_the_directory_change_owner_and_only_when_scoped`<br>`togi_trial::files::tests::the_container_is_recreated_with_fixed_modes_when_scoped`<br>`togi_trial::files::tests::instances_get_exclusive_logs_inputs_and_fixed_modes` | A |
| `TestScopeRefusesInaccessibleAncestor` | `togi_trial::files::tests::an_ancestor_the_account_cannot_search_is_refused_without_chmod`<br>`togi_trial::files::tests::the_account_searches_by_exactly_one_permission_class`<br>`togi_trial::files::tests::the_deepest_directory_the_account_cannot_search_is_the_one_named` | A |
| `TestScopeRefusesIdentityBeforePreparing` | `togi_trial::tests::teardown::an_invalid_spec_or_missing_backend_or_identity_never_touches_the_disk` | A |
| `TestOwnershipFailureDoesNotLaunch` | `togi_trial::tests::trial::an_ownership_failure_names_the_input_and_launches_nothing`<br>`togi_trial::tests::trial::an_ownership_failure_of_a_later_instance_rolls_the_earlier_one_back` | A |
| `TestScopeLogSymlinkRace` | `togi_trial::files::tests::retained_log_descriptors_survive_a_path_replacement`<br>`togi_trial::files::tests::both_retained_logs_survive_a_symlink_swap_of_their_paths`<br>`togi_trial::files::tests::an_input_named_like_a_log_or_repeated_fails_exclusively` | A |
| `TestReusedTrialDirectoryDoesNotFollowSymlinks` | `togi_trial::files::tests::an_old_trial_is_removed_without_following_symlinks`<br>`togi_trial::files::tests::a_trial_directory_that_is_a_symlink_is_replaced_not_followed`<br>`togi_trial::files::tests::a_reused_trial_directory_never_writes_through_old_input_or_log_symlinks`<br>`togi_trial::files::tests::an_input_named_like_a_log_or_repeated_fails_exclusively` | A |
| `TestRelativeTrialDirectory` | `togi_trial::files::tests::a_relative_trials_directory_resolves_against_its_base_and_touches_nothing_else`<br>`togi_trial::tests::process::real_process_a_relative_trial_directory_works_and_the_watched_file_lands_in_the_work_directory` | A |
| `TestInputOwnershipFailureNamesInput` | `togi_trial::files::tests::a_failed_input_ownership_names_the_input_and_changes_nothing_after_it`<br>`togi_trial::files::tests::a_failed_directory_ownership_is_named_once_every_input_was_handed_over` | A |

Go's malformed, negative and overflowing id strings need no Rust case: ids are `u32` from the `nix` lookup, so only `0` and `u32::MAX` remain to refuse.

### `internal/trial/output_session_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestOutputLimitSessionContainmentDeadEnd` | `togi_trial::tests::output::computation_and_affinity_errors_win_over_the_cap_in_either_order`<br>`togi_trial::evidence::tests::the_cap_error_is_suppressed_by_stronger_evidence_in_both_orders`<br>`togi_session::evidence::tests::an_escape_names_the_cpus_and_keeps_the_diagnostic`<br>`togi_session::recovery::tests::containment_evidence_names_the_escaped_cpus_or_carries_the_error_until_its_dead_end`<br>`togi_session::run_trial::tests::evidence_survives_a_runner_error_and_the_error_stays_in_the_reason`<br>`togi_session::run_trial::tests::an_escaped_worker_ends_the_trial_inconclusive_with_its_cpus_and_dead_ends_on_containment` | X |
| `TestWatchedBacklogSessionContainmentDeadEnd` | `togi_trial::tests::teardown::slow_watched_output_of_many_files_is_bound_by_one_cleanup_budget`<br>`togi_trial::tests::output::unread_accepted_watched_bytes_that_vanish_are_lost_output_and_containment`<br>`togi_session::run_trial::tests::a_containment_error_from_the_wait_withholds_the_proof_a_successful_stop_gives`<br>`togi_session::run_trial::tests::an_unconfirmed_containment_withholds_the_proof_and_ends_the_trial_as_such` | X |

### `internal/trial/output_test.go` (22)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestOutputLineBoundaries` | `togi_trial::output::tests::a_line_of_exactly_the_limit_is_accepted_terminated_or_not`<br>`togi_trial::output::tests::lf_cr_and_crlf_each_end_a_line`<br>`togi_trial::output::tests::lines_split_across_chunks_join`<br>`togi_trial::output::tests::one_byte_over_the_limit_exceeds_and_keeps_the_prefix`<br>`togi_trial::output::tests::nothing_is_consumed_after_exceeding`<br>`togi_trial::output::tests::memory_stays_bounded_by_the_limit`<br>`togi_trial::output::tests::limit_lines_end_wherever_their_terminator_lands_and_an_overrun_split_across_chunks_exceeds` | A |
| `TestWatchedOutputLargeAppend` | `togi_trial::watch::tests::a_large_append_is_read_in_bounded_polls`<br>`togi_trial::watch::tests::an_oversized_line_stops_the_file_for_good_and_keeps_its_prefix_unclassified` | A |
| `TestWatchedRejectsNonRegularFiles` | `togi_trial::watch::tests::symlinks_directories_and_fifos_are_rejected_without_reading` | A |
| `TestOversizedOutputWait` | `togi_trial::tests::output::an_oversized_line_is_inconclusive_keeps_its_prefix_and_is_not_containment`<br>`togi_trial::tests::process::real_process_oversized_output_on_any_source_is_inconclusive_with_the_exact_prefix_and_no_containment` | A |
| `TestExactOutputLineLimitWait` | `togi_trial::tests::output::a_line_of_exactly_the_limit_is_accepted_in_every_source` | A |
| `TestOutputLimitEvidencePrecedence` | `togi_trial::tests::output::computation_and_affinity_errors_win_over_the_cap_in_either_order`<br>`togi_trial::evidence::tests::the_cap_error_is_suppressed_by_stronger_evidence_in_both_orders` | A |
| `TestOutputLimitRetainsCleanupAndIOErrors` | `togi_trial::tests::output::the_cap_never_hides_cleanup_or_ordinary_io_errors`<br>`togi_trial::evidence::tests::ordinary_output_errors_always_surface_even_beside_strong_evidence` | A |
| `TestOutputLimitPrecedesQueuedExit` | `togi_trial::tests::output::the_cap_diagnostic_prevents_a_false_unexpected_exit`<br>`togi_trial::evidence::tests::the_cap_error_alone_makes_the_trial_inconclusive` | A |
| `TestWatchedShortLineFloodCancellation` | `togi_trial::watch::tests::a_stop_requested_while_a_line_is_classified_leaves_the_rest_of_the_chunk_unread`<br>`togi_trial::watch::tests::a_sink_break_on_the_last_byte_of_a_chunk_still_ends_the_poll`<br>`togi_trial::watch::tests::stopping_between_lines_resumes_at_the_exact_offset`<br>`togi_trial::watch::tests::the_stop_condition_is_asked_before_each_chunk`<br>`togi_trial::tests::process::real_process_cancelling_during_a_watched_flood_ends_supervision_and_the_teardown_drains_the_backlog` | A |
| `TestWatchedFloodKeepsSupervision` | `togi_trial::tests::output::a_flooding_file_cannot_starve_the_quiet_instance_beside_it`<br>`togi_trial::tests::output::a_flooded_instance_does_not_skip_the_required_samples_of_its_neighbour`<br>`togi_trial::tests::trial::an_output_flood_neither_skips_load_steps_nor_delays_the_deadline` | A |
| `TestWatchedFloodSamplesEveryReadyInstance` | `togi_trial::tests::output::every_ready_instance_is_sampled_each_tick_while_both_watched_floods_are_outstanding` | A |
| `TestWatchedPollFairness` | `togi_trial::tests::output::a_flooding_file_cannot_starve_the_quiet_instance_beside_it`<br>`togi_trial::tests::output::a_flooded_instance_does_not_skip_the_required_samples_of_its_neighbour`<br>`togi_trial::tests::output::every_ready_instance_is_sampled_each_tick_while_both_watched_floods_are_outstanding` | A |
| `TestWatchedDeadlineBeforeCancellationPublication` | `togi_trial::tests::trial::a_watched_poll_that_uses_the_rest_of_the_duration_leaves_no_required_sample_after_it`<br>`togi_trial::tests::trial::a_watched_poll_that_uses_the_rest_of_the_duration_still_drains_a_quiet_instances_final_error` | A |
| `TestWatchedPartialLineAcrossBatches` | `togi_trial::watch::tests::a_line_split_across_polls_is_one_line_and_the_tail_waits_for_writers`<br>`togi_trial::watch::tests::a_large_append_is_read_in_bounded_polls` | A |
| `TestWatchedErrorBeyondReadBudget` | `togi_trial::tests::output::an_error_beyond_the_first_read_batches_is_found_by_the_teardown_drain`<br>`togi_trial::tests::output::a_watched_flood_with_a_final_unterminated_error_is_drained_once_without_containment`<br>`togi_trial::tests::teardown::a_watched_append_after_the_last_drain_poll_is_classified_before_the_proof` | A |
| `TestWatchedTeardownMultiInstanceDeadline` | `togi_trial::tests::teardown::slow_watched_output_of_many_files_is_bound_by_one_cleanup_budget` | A |
| `TestWatchedLostBacklogCannotPass` | `togi_trial::tests::output::unread_accepted_watched_bytes_that_vanish_are_lost_output_and_containment`<br>`togi_trial::watch::tests::removal_truncation_and_replacement_of_unread_bytes_are_lost`<br>`togi_trial::watch::tests::a_fifo_replacing_a_file_with_unread_backlog_loses_the_backlog`<br>`togi_trial::watch::tests::truncating_the_same_inode_below_the_accepted_size_loses_the_backlog`<br>`togi_trial::watch::tests::a_read_failing_on_a_backlog_accepted_by_the_same_poll_is_lost_output`<br>`togi_trial::watch::tests::end_of_file_before_the_accepted_end_keeps_the_backlog_pending_and_never_drains` | A |
| `TestWatchedRejectedOutputIsInconclusive` | `togi_trial::tests::output::rejecting_a_symlinked_watched_file_without_backlog_is_an_ordinary_diagnostic`<br>`togi_trial::tests::output::rejecting_a_fifo_watched_file_without_backlog_is_an_ordinary_diagnostic`<br>`togi_trial::watch::tests::symlinks_directories_and_fifos_are_rejected_without_reading` | A |
| `TestWatchedMissingFileBeforeCreation` | `togi_trial::tests::output::a_watched_file_that_is_never_created_is_harmless`<br>`togi_trial::watch::tests::a_missing_file_is_idle_and_harmless` | A |
| `TestWatchedPartialSurvivesGraceUntilScopeKill` | `togi_trial::tests::output::a_watched_partial_is_classified_only_after_the_scope_kill_confirms_the_writers`<br>`togi_trial::tests::teardown::an_unverifiable_scope_scan_withholds_the_proof_and_the_watched_partial`<br>`togi_trial::watch::tests::a_final_line_needs_a_drained_file` | A |
| `TestWatchedQueuedExitBeyondReadBudget` | `togi_trial::tests::output::an_exit_queued_ahead_of_a_watched_error_beyond_the_first_batch_does_not_win`<br>`togi_trial::tests::output::an_exit_queued_ahead_of_watched_setup_or_affinity_text_beyond_the_first_batch_does_not_win` | A |
| `TestWatchedFloodPreservesLoadStepSchedule` | `togi_trial::tests::output::a_slow_classified_watched_flood_is_polled_during_the_trial_and_leaves_the_load_steps_on_time` | A |

`TestWatchedDeadlineBeforeCancellationPublication`: the already-expired case is obsolete (Rust refuses a zero duration and has no context whose deadline lags its timer).

### `internal/trial/pm_table_linux_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestBlockedPMTablePreservesSupervision` | `togi_trial::tests::trial::a_blocked_pm_transfer_neither_delays_the_deadline_nor_puts_lanes_in_samples`<br>`togi_trial::tests::trial::a_blocked_pm_transfer_does_not_delay_cancellation`<br>`togi_trial::tests::trial::a_blocked_pm_transfer_does_not_delay_a_computation_error`<br>`togi_trial::tests::trial::a_blocked_pm_transfer_does_not_move_the_r6_burst_steps` | A |

### `internal/trial/process_linux_test.go` (6)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestSystemdPreflightTimeout` | `togi_trial::tests::sweep::the_probe_is_bounded_by_thirty_seconds_and_needs_an_account` | A |
| `TestScopeTimeoutUsesRemainingTeardownTime` | `togi_trial::tests::teardown::a_scope_kill_that_outlives_its_deadline_is_cut_off_at_the_shared_slot`<br>`togi_trial::tests::teardown::a_timed_out_scope_kill_is_never_read_as_a_missing_scope` | A |
| `TestRootLauncherClearsSupplementaryGroups` | `togi_trial::launch::tests::attributes::real_process_a_root_launcher_clears_its_supplementary_groups`<br>`togi_trial::launch::tests::attributes::real_process_a_launcher_is_killed_when_its_forking_thread_dies`<br>`togi_trial::tests::process::real_process_a_zombie_leader_keeps_its_group_signalable_until_it_is_reaped` | A |
| `TestUnprivilegedLauncherPreservesCredentials` | `togi_trial::launch::tests::attributes::real_process_an_unprivileged_launcher_keeps_its_credentials_and_never_attempts_setgroups` | A |
| `TestProcStatDiagnostic` | `togi_trial::procfs::tests::malformed_stats_are_diagnosed_with_a_capped_quoted_excerpt`<br>`togi_trial::procfs::tests::a_stat_excerpt_is_exactly_the_first_256_bytes_and_says_when_it_was_cut` | A |
| `TestPreflightRejectsIdentityBeforeCommand` | `togi_trial::tests::sweep::the_probe_is_bounded_by_thirty_seconds_and_needs_an_account` | A |

The root-only test runs only as root and the unprivileged one only otherwise; each returns at once on the other. Go's `Setpgid` assertion is the group check in `real_process_a_zombie_leader_keeps_its_group_signalable_until_it_is_reaped` (the descendant's group equals the leader's pid); `Pdeathsig` is `real_process_a_launcher_is_killed_when_its_forking_thread_dies`.

### `internal/trial/process_sampling_linux_test.go` (4)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestProcUsageValidity` | `togi_trial::procfs::tests::usage_distinguishes_missing_from_malformed`<br>`togi_trial::procfs::tests::comm_with_spaces_and_parentheses_parses_and_cpu_time_uses_the_tick_rate`<br>`togi_trial::procfs::tests::malformed_stats_are_diagnosed_with_a_capped_quoted_excerpt`<br>`togi_trial::procfs::tests::cpu_time_overflow_is_none`<br>`togi_trial::procfs::tests::each_numeric_stat_field_that_is_not_a_number_is_a_lost_observation_not_a_disappearance` | A |
| `TestProcThreadsValidity` | `togi_trial::procfs::tests::threads_keep_the_readable_ones_when_siblings_fail_or_vanish`<br>`togi_trial::procfs::tests::per_process_reads_fail_on_a_non_file_stat_and_threads_keep_readable_ones` | A |
| `TestProcThreadsMalformedID` | `togi_trial::procfs::tests::threads_keep_the_readable_ones_when_siblings_fail_or_vanish`<br>`togi_trial::procfs::tests::thread_ids_that_are_not_positive_numbers_are_lost_observations_beside_the_readable_one` | A |
| `TestMissingTaskDirectoryPreservesDisappearance` | `togi_trial::procfs::tests::a_missing_task_directory_is_a_disappearance`<br>`togi_trial::procfs::tests::a_task_entry_that_is_not_a_directory_is_lost_observation` | A |

### `internal/trial/process_test.go` (10)

`integration && linux`: the Rust twins are plain `cargo nextest` tests (real helper processes, unscoped).

| Go test | Rust test(s) | status |
|---|---|---|
| `TestProcessTrials` | `togi_trial::tests::process::real_process_a_working_helper_passes_a_failing_one_fails_and_an_exiting_one_exits_early` | A |
| `TestProcessOversizedOutput` | `togi_trial::tests::process::real_process_oversized_output_on_any_source_is_inconclusive_with_the_exact_prefix_and_no_containment` | A |
| `TestProcessOutputLimitEvidencePrecedence` | `togi_trial::tests::process::real_process_affinity_and_computation_errors_beat_an_overlong_line` | A |
| `TestProcessEscape` | `togi_trial::tests::process::real_process_a_helper_that_pins_itself_elsewhere_is_an_escape`<br>`togi_trial::tests::trial::a_thread_on_a_foreign_cpu_is_an_escape_and_ends_the_trial` | A |
| `TestProcessR6StartsStopped` | `togi_trial::tests::process::real_process_r6_workers_are_stopped_before_the_trial_starts_to_count` | A |
| `TestProcessPartialStartRollback` | `togi_trial::tests::process::real_process_a_failed_second_launch_keeps_its_cause_first_targets_both_scopes_and_is_containment_while_a_holder_lives`<br>`togi_trial::tests::process::real_process_a_detached_pipe_holder_makes_the_drain_unconfirmed_and_the_stop_containment` | A |
| `TestProcessSweepAfterKilledOwner` | `togi_trial::tests::sweep::killed_owner::real_process_a_sweep_after_a_killed_owner_removes_its_detached_descendant`<br>`togi_trial::tests::sweep::killed_owner::real_process_owner_setup_failures_leave_no_owner_or_detached_descendant` | A |
| `TestProcessOwnedGroupIdentity` | `togi_trial::tests::process::real_process_a_zombie_leader_keeps_its_group_signalable_until_it_is_reaped` | A |
| `TestProcessRelativeTrialDirectory` | `togi_trial::tests::process::real_process_a_relative_trial_directory_works_and_the_watched_file_lands_in_the_work_directory` | A |
| `TestProcessWatchedFloodCancellation` | `togi_trial::tests::process::real_process_the_watched_flood_helper_survives_a_group_term_and_only_sigkill_ends_it`<br>`togi_trial::tests::process::real_process_a_flooding_watched_file_is_drained_once_while_the_helper_ignores_term`<br>`togi_trial::tests::process::real_process_cancelling_during_a_watched_flood_ends_supervision_and_the_teardown_drains_the_backlog` | A |

`TestProcessEscape`: the real-process test pins the escaped cpu, the `outside allowed cpus` warning sample is pinned on the fake OS by the second test.

`TestProcessOwnedGroupIdentity`: the Rust test pins the half that still applies, a signal to the group of an unreaped zombie leader succeeds and reaches a same-group descendant. The forged-start refusal has no Rust counterpart because there is no start-time identity check: the pidfd-held leader cannot be a reused pid, and `reap(self)` consumes it, so a later signal after the reap cannot be written.

### `internal/trial/samples_test.go` (7)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestTrialConditionsSamples` | `togi_trial::tests::samples::each_tick_persists_a_conditions_sample_with_sensors_workers_and_power`<br>`togi_trial::sensors::tests::k10temp_labels_map_to_whole_degrees_and_zenpower_tctl_beats_tdie`<br>`togi_trial::sensors::tests::clocks_come_from_the_first_cpu_of_each_loaded_core_only`<br>`togi_trial::sensors::tests::package_power_is_energy_delta_over_elapsed_time`<br>`togi_trial::tests::samples::the_trial_keeps_the_highest_tctl_it_saw_and_leaves_out_a_ccd_that_reads_as_no_number` | A |
| `TestConditionsMissingAndPowerWrap` | `togi_trial::tests::samples::missing_sensors_are_omitted_not_zeroed`<br>`togi_trial::sensors::tests::a_counter_wrap_uses_the_range_only_when_it_exceeds_the_previous_reading`<br>`togi_trial::sensors::tests::a_missing_reading_resets_the_baseline_so_the_next_good_one_omits_power` | A |
| `TestSamplesOpenFailureDuration` | `togi_trial::tests::samples::an_open_failure_ends_the_trial_early_without_delaying_teardown`<br>`togi_trial::tests::samples::an_open_failure_before_the_deadline_ends_the_trial_when_it_happens`<br>`togi_trial::tests::samples::samples_setup_blocked_past_the_join_bound_reports_timeout_after_teardown`<br>`togi_trial::tests::samples::an_open_failure_during_the_post_teardown_join_keeps_the_original_create_error`<br>`togi_trial::samples::tests::an_open_failure_is_reported_and_never_delays_the_caller` | A |
| `TestSamplesPersistenceFailureEndsTrial` | `togi_trial::tests::samples::a_write_failure_ends_the_trial_and_keeps_the_close_error_too`<br>`togi_trial::tests::samples::a_writer_that_cannot_start_ends_the_trial_with_a_storage_error`<br>`togi_trial::samples::tests::an_append_failure_wakes_the_supervisor_ends_the_writer_and_keeps_the_close_error`<br>`togi_trial::samples::tests::a_failed_sync_and_a_failed_close_are_both_returned` | A |
| `TestSlowSamplesPreserveScheduleAndJoinWriter` | `togi_trial::tests::samples::slow_storage_neither_delays_load_steps_nor_the_trial_and_the_join_is_bounded`<br>`togi_trial::tests::samples::blocked_samples_setup_keeps_the_load_schedule_and_the_end_waits_for_the_writer`<br>`togi_trial::samples::tests::a_slow_write_drops_the_newest_sample_without_waiting`<br>`togi_trial::samples::tests::a_stuck_writer_is_abandoned_at_the_join_bound` | A |
| `TestSamplesSetupCancellation` | `togi_trial::tests::samples::cancelling_while_samples_setup_is_blocked_stops_the_workload_before_the_setup_ends`<br>`togi_trial::tests::samples::a_stuck_writer_join_does_not_block_the_runtime_thread` | A |
| `TestSamplesMissingAndEarlyStop` | `togi_trial::files::tests::samples_are_read_only_from_regular_files_below_the_validated_root`<br>`togi_trial::samples::tests::the_real_file_is_exclusive_durable_and_one_json_object_per_line`<br>`togi_machine::tests::read_samples_stops_early`<br>`togi_trial::tests::runner::the_runner_sweeps_probes_compresses_and_reads_samples` | A |

### `internal/trial/sampling_test.go` (4)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestSamplingLoss` | `togi_trial::tests::trial::a_lost_sampling_read_makes_the_whole_trial_inconclusive_and_teardown_still_confirms`<br>`togi_trial::tests::trial::a_lost_sample_sticks_through_the_whole_trial` | A |
| `TestSamplingDisappearance` | `togi_trial::tests::trial::a_disappearing_process_is_not_a_sampling_loss`<br>`togi_trial::tests::trial::a_usage_read_that_finds_the_exiting_process_gone_leaves_an_unexpected_exit_not_a_loss`<br>`togi_trial::procfs::tests::only_a_missing_process_counts_as_gone_not_io_errors_or_denials` | A |
| `TestSamplingLossKeepsEscape` | `togi_trial::tests::trial::a_partial_thread_listing_still_proves_an_escape_despite_other_failures` | A |
| `TestSamplingLossKeepsBackendFailures` | `togi_trial::tests::trial::a_computation_error_that_follows_a_thread_read_loss_is_not_replaced_by_the_loss`<br>`togi_trial::tests::trial::a_stall_that_follows_a_thread_read_loss_is_not_replaced_by_the_loss`<br>`togi_trial::evidence::tests::sampling_loss_sticks_but_never_replaces_stronger_evidence` | A |

### `internal/trial/stop_test.go` (1)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestStopJoinsWithoutWait` | `togi_trial::tests::runner::stopping_without_waiting_tears_down_and_proves_it`<br>`togi_trial::tests::runner::stopping_without_waiting_has_terminated_the_workload_before_it_returns` | A |

`TestStopJoinsWithoutWait`: the second Stop is obsolete (stopping takes the trial by value).

### `internal/trial/sweep_linux_test.go` (5)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestScopeProcessDiscoveryAndIdentity` | `togi_trial::procfs::tests::scope_scan_captures_identity_skips_zombies_and_disappeared`<br>`togi_trial::procfs::tests::scope_discovery_takes_a_path_component_and_the_last_match`<br>`togi_trial::procfs::tests::a_component_with_spaces_is_a_different_cgroup_not_a_trial_scope`<br>`togi_trial::procfs::tests::scope_membership_is_the_path_suffix`<br>`togi_trial::procfs::tests::a_captured_process_that_left_its_scope_stays_alive_until_its_start_time_changes` | A |
| `TestSystemdScopeListFiltersNames` | `togi_trial::tests::sweep::only_exact_trial_units_survive_the_listing_filter`<br>`togi_trial::tests::sweep::a_hung_initial_listing_is_containment_at_the_shared_deadline` | A |
| `TestOwnedProcessGroupRejectsUnverifiedIdentity` | — | O(an owned group is signalled only through its pidfd-held, unreaped leader (`reap(self)` consumes it), so no start-time, state or `/proc` group check exists to refuse; the zombie-leader case is pinned under `TestProcessOwnedGroupIdentity`) |
| `TestScopeDiscoveryReadFailures` | `togi_trial::procfs::tests::scope_scan_fails_on_unreadable_data`<br>`togi_trial::procfs::tests::scope_scan_fails_when_the_cgroup_or_stat_is_not_a_readable_file`<br>`togi_trial::procfs::tests::scope_scan_fails_on_permission_denied_but_not_on_a_vanished_file`<br>`togi_trial::procfs::tests::scope_scan_skips_a_vanished_cgroup_and_keeps_the_readable_siblings`<br>`togi_trial::procfs::tests::scope_scan_captures_identity_skips_zombies_and_disappeared`<br>`togi_trial::procfs::tests::malformed_stats_are_diagnosed_with_a_capped_quoted_excerpt`<br>`togi_trial::procfs::tests::a_scope_process_with_an_invalid_start_time_fails_the_scan`<br>`togi_trial::tests::sweep::cancelling_during_initial_process_scan_finishes_listed_scope_cleanup_and_keeps_errors` | A |
| `TestOwnedProcessIdentityReadFailures` | `togi_trial::procfs::tests::alive_rejects_zombies_and_surfaces_read_errors`<br>`togi_trial::procfs::tests::per_process_reads_fail_on_a_non_file_stat_and_threads_keep_readable_ones`<br>`togi_trial::procfs::tests::a_scope_process_with_an_invalid_start_time_fails_the_scan` | A |

`TestOwnedProcessIdentityReadFailures`: the `ProcessAlive` half is pinned; the `SignalGroup` half is obsolete with the identity check, as above.

### `internal/trial/sweep_test.go` (8)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestSweepExactScopes` | `togi_trial::tests::sweep::the_sweep_signals_each_exact_scope_in_order_and_removes_them_all`<br>`togi_trial::tests::sweep::a_captured_process_that_outlives_the_term_holds_the_kill_back_for_the_whole_grace`<br>`togi_trial::tests::sweep::only_exact_trial_units_survive_the_listing_filter` | A |
| `TestSweepCannotConfirmCleanup` | `togi_trial::tests::sweep::a_lingering_unit_or_a_process_moved_out_of_its_scope_is_containment_after_the_verify_slot` | A |
| `TestSweepSharedDeadline` | `togi_trial::tests::sweep::a_hung_initial_listing_is_containment_at_the_shared_deadline`<br>`togi_trial::tests::sweep::hung_kills_stay_inside_the_slot_and_every_scope_is_still_attempted`<br>`togi_trial::tests::sweep::a_stop_that_hangs_or_fails_after_a_successful_kill_is_kept_and_the_other_scopes_still_stop`<br>`togi_trial::tests::sweep::a_hung_verification_listing_is_containment_inside_the_shared_deadline` | A |
| `TestSweepVerificationError` | `togi_trial::tests::sweep::a_failing_verification_listing_is_containment`<br>`togi_trial::tests::sweep::a_final_listing_that_cannot_run_withholds_stopped_even_when_the_machine_is_clean` | A |
| `TestSweepScopeSignalDeadline` | `togi_trial::tests::sweep::hung_signals_share_the_three_second_grace_and_still_end_in_kill_and_stop`<br>`togi_trial::tests::sweep::a_hung_signal_of_either_kind_is_containment_inside_the_grace_and_ends_in_kill_and_stop`<br>`togi_trial::tests::teardown::a_timed_out_scope_kill_is_never_read_as_a_missing_scope` | A |
| `TestSweepScopeSignalFailure` | `togi_trial::tests::sweep::an_uncertain_signal_failure_fails_the_sweep_even_when_everything_ends_up_gone`<br>`togi_trial::tests::sweep::a_signal_command_that_cannot_run_is_kept_even_when_everything_ends_up_gone` | A |
| `TestSweepObservationFailureIsContainment` | `togi_trial::tests::sweep::a_failed_initial_process_scan_is_containment_and_the_listed_scopes_are_still_killed`<br>`togi_trial::tests::sweep::a_hanging_process_scan_is_cut_short_of_the_deadline_and_listed_scopes_are_still_killed`<br>`togi_trial::tests::sweep::a_failing_verification_discovery_withholds_stopped_and_keeps_the_earlier_failure`<br>`togi_trial::tests::sweep::a_failed_grace_observation_still_kills_and_stops_every_known_scope` | A |
| `TestProcessDisappeared` | `togi_trial::procfs::tests::only_a_missing_process_counts_as_gone_not_io_errors_or_denials` | A |

Rust's sweep deadlines are shaped differently from Go's, so the tests assert the bounds the Rust sweep promises: a hung listing ends at 13 s (discovery stops the 2 s kill slot short of the 15 s budget), kills and stops stay in the 2 s slot, and everything finishes inside 15 s.

### `internal/trial/trial_test.go` (20)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestWait` | `togi_trial::tests::trial::a_quiet_trial_runs_its_whole_duration_and_teardown_stops_it`<br>`togi_trial::tests::trial::progress_comes_from_stdout_and_stderr_progress_is_ignored`<br>`togi_trial::tests::trial::a_computation_error_ends_the_trial_and_names_the_core`<br>`togi_trial::tests::trial::an_early_exit_is_an_unexpected_exit_of_that_core`<br>`togi_trial::tests::trial::a_watched_file_error_is_found_at_the_next_tick`<br>`togi_trial::tests::trial::a_worker_that_stops_advancing_stalls_after_grace_plus_window`<br>`togi_trial::tests::trial::cancelling_ends_the_trial_with_a_cancelled_error_and_a_confirmed_teardown`<br>`togi_trial::tests::trial::r4_load_steps_signal_the_group_and_count_stops_and_continues`<br>`togi_trial::tests::trial::r7_runs_one_instance_per_core_and_progress_names_the_core`<br>`togi_trial::tests::trial::a_thread_on_a_foreign_cpu_is_an_escape_and_ends_the_trial` | A |
| `TestScopeNeverReady` | `togi_trial::tests::trial::a_scope_that_is_never_entered_makes_the_trial_inconclusive_not_failed`<br>`togi_trial::evidence::tests::a_scope_never_entered_is_a_weak_setup_observation` | A |
| `TestQueuedExitBeforeTeardown` | `togi_trial::tests::trial::an_exit_queued_before_a_cancellation_is_still_an_unexpected_exit`<br>`togi_trial::evidence::tests::a_queued_exit_is_unexpected_unless_something_explains_it` | A |
| `TestTeardownEvidencePrecedence` | `togi_trial::evidence::tests::computation_error_replaces_setup_error_in_either_order`<br>`togi_trial::evidence::tests::affinity_error_wins_over_computation_error_in_either_order`<br>`togi_trial::evidence::tests::a_thread_escape_beats_everything_and_names_the_cpu` | A |
| `TestPartialStartRollbackBound` | `togi_trial::tests::teardown::a_partial_start_that_cannot_confirm_is_containment_with_both_scopes_targeted` | A |
| `TestTeardownSharedDeadline` | `togi_trial::tests::teardown::teardown_shares_one_fifteen_second_budget_across_instances`<br>`togi_trial::tests::teardown::a_scope_kill_that_outlives_its_deadline_is_cut_off_at_the_shared_slot`<br>`togi_trial::tests::teardown::a_timed_out_scope_kill_is_never_read_as_a_missing_scope`<br>`togi_trial::tests::teardown::a_completed_not_loaded_answer_counts_as_a_missing_scope`<br>`togi_trial::tests::teardown::another_failure_text_is_a_failed_kill` | A |
| `TestTeardownFinalOutput` | `togi_trial::tests::teardown::final_unterminated_pipe_output_survives_confirmed_and_unconfirmed_cleanup` | A |
| `TestPipeReadFailureIsContainment` | `togi_trial::tests::teardown::a_pipe_read_error_is_kept_as_containment_and_evidence` | A |
| `TestPartialStartConfirmedRollback` | `togi_trial::tests::teardown::a_partial_start_that_confirms_keeps_its_cause_without_containment` | A |
| `TestStopIsIdempotent` | `togi_trial::tests::runner::stopping_without_waiting_tears_down_and_proves_it`<br>`togi_trial::tests::runner::an_unconfirmed_teardown_is_containment_for_wait_and_for_stop` | A |
| `TestR6Bursts` | `togi_trial::tests::r6::r6_stops_every_core_before_timing_then_bursts_one_core_at_a_time`<br>`togi_trial::tests::r6::r6_workers_that_write_on_their_own_are_silent_while_stopped_and_write_in_bursts` | A |
| `TestPlans` | `togi_trial::schedule::tests::duty_schedule_alternates_stop_and_cont_before_the_end`<br>`togi_trial::schedule::tests::random_schedule_is_reproducible_and_prefix_stable`<br>`togi_trial::schedule::tests::bursts_start_after_half_the_trial_one_core_at_a_time`<br>`togi_trial::schedule::tests::continuous_load_sends_nothing`<br>`togi_trial::plan::tests::load_steps_come_from_the_spec`<br>`togi_trial::plan::tests::r6_bursts_follow_the_machine_order_mapped_to_instances` | A |
| `TestStalled` | `togi_trial::stall::tests::strictly_less_than_half_a_cpu_per_cpu_stalls_and_equality_is_healthy`<br>`togi_trial::tests::trial::a_worker_advancing_at_half_a_cpu_per_cpu_is_healthy` | A |
| `TestTctl` | `togi_trial::sensors::tests::k10temp_labels_map_to_whole_degrees_and_zenpower_tctl_beats_tdie` | A |
| `TestR6PlanTermination` | `togi_trial::schedule::tests::consumers_may_stop_early`<br>`togi_trial::schedule::tests::a_burst_stop_reaching_the_end_is_omitted_and_cores_cycle` | A |
| `TestPreparationFailureDoesNotLaunch` | `togi_trial::tests::teardown::nothing_launches_when_preparation_fails`<br>`togi_trial::tests::teardown::an_invalid_spec_or_missing_backend_or_identity_never_touches_the_disk`<br>`togi_trial::files::tests::backend_failures_and_an_empty_program_stop_before_any_log_exists`<br>`togi_trial::files::tests::an_input_named_like_a_log_or_repeated_fails_exclusively`<br>`togi_trial::tests::trial::a_backend_that_fails_its_check_is_refused_with_its_error_and_nothing_launches` | A |
| `TestLoadStepFailureContainsWorkload` | `togi_trial::tests::r6::a_failed_scheduled_stop_ends_the_trial_and_contains_the_workload`<br>`togi_trial::tests::r6::a_failed_initial_stop_rolls_the_start_back_and_names_the_signal`<br>`togi_trial::tests::r6::a_refused_scheduled_stop_with_confirmed_cleanup_keeps_the_signal_error_in_r4`<br>`togi_trial::tests::r6::a_refused_initial_stop_with_confirmed_cleanup_keeps_the_signal_error_and_the_proof` | A |
| `TestSystemdStartupDiagnosticsAreInconclusive` | `togi_trial::evidence::tests::systemd_startup_text_on_stderr_is_a_setup_error_not_an_exit` | A |
| `TestR6ScopeReadinessBeforeInitialStop` | `togi_trial::tests::r6::r6_waits_for_the_scope_before_the_initial_stop`<br>`togi_trial::tests::r6::r6_stops_a_worker_that_never_entered_its_scope_after_the_grace_and_goes_on`<br>`togi_trial::tests::r6::cancelling_during_the_scope_wait_still_stops_the_worker_then_rolls_back` | A |
| `TestTrialLaunchTimeout` | `togi_trial::tests::runner::a_stalled_start_is_containment_without_a_value_and_the_trial_it_finishes_late_is_abandoned`<br>`togi_trial::tests::runner::a_timed_out_start_launches_no_further_instance_and_rolls_back_the_first`<br>`togi_trial::timing::tests::every_synchronous_bound_covers_what_the_runtime_thread_may_take`<br>`togi_trial::tests::trial::a_preparation_stuck_past_its_budget_ends_the_start_with_a_typed_error` | A |

`TestStopIsIdempotent`: a repeated Stop is obsolete (stopping takes the trial by value, so it cannot be retried).

### `internal/trial/writer_evidence_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestTeardownPreservesIndependentWriterEvidence` | `togi_trial::tests::output::writers_confirmed_for_one_instance_keep_their_evidence_when_another_scope_fails`<br>`togi_trial::tests::output::writers_confirmed_for_one_instance_keep_their_evidence_when_another_group_kill_fails`<br>`togi_trial::tests::output::writers_of_an_instance_whose_scope_is_already_gone_keep_their_evidence_when_another_scope_fails`<br>`togi_trial::tests::teardown::a_completed_not_loaded_answer_counts_as_a_missing_scope` | A |
| `TestNoScopeWatchedPartialRequiresConfirmedGroupKill` | `togi_trial::tests::output::an_unscoped_watched_partial_needs_a_group_kill_that_actually_succeeded`<br>`togi_trial::tests::output::an_unscoped_watched_partial_after_a_failed_group_kill_is_withheld_as_containment` | A |

`TestTeardownPreservesIndependentWriterEvidence`: a second teardown of the same trial is obsolete (the supervisor finishes once).

## hardware

### `internal/hardware/diagnose_hardware_test.go` (1)

Build tag `hardware && linux`; the Rust twin needs `--features hardware`, the real SMU and root.

| Go test | Rust test(s) | status |
|---|---|---|
| `TestHardwareDiagnoseLeavesOffsets` | `togi_hardware::hardware::diagnosing_leaves_every_offset_unchanged` | A |

### `internal/hardware/grub_test.go` (9)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestGRUBCleanupFilesystemEffectWindows` | `togi_hardware::grub::tests::clearing_fails_in_each_of_its_three_commands_with_what_was_known` | A |
| `TestGRUBCleanupRejectsRemainingSavedEntry` | `togi_hardware::grub::tests::a_saved_entry_that_survives_the_unset_is_rejected_with_both_observations`<br>`togi_hardware::grub::tests::the_bootloader_trait_reports_a_clear_failure_as_an_io_error_holding_the_typed_error` | A |
| `TestGRUBCleanupAlreadyUnset` | `togi_hardware::grub::tests::an_absent_saved_entry_clears_idempotently` | A |
| `TestGRUBSavedEntryParsing` | `togi_hardware::grub::tests::saved_entry_parsing_keeps_exactly_the_value`<br>`togi_hardware::grub::tests::get_reads_through_the_listing_and_reports_absent_as_none` | A |
| `TestGRUBEnvironmentOperations` | `togi_hardware::grub::tests::set_writes_every_pair_in_one_sorted_command_and_unset_leaves_the_saved_entry`<br>`togi_hardware::grub::tests::edits_become_the_grub_editenv_verbs`<br>`togi_hardware::grub::tests::an_empty_set_or_unset_runs_nothing` | A |
| `TestGRUBRejectsInvalidEnvironmentValues` | `togi_hardware::grub::tests::invalid_names_and_values_are_refused_before_any_command`<br>`togi_hardware::grub::tests::a_value_of_exactly_300_printable_bytes_is_accepted` | A |
| `TestGRUBRetryRejectsWhitespaceCounts` | `togi_hardware::grub::tests::whitespace_counts_are_read_exactly_and_grant_no_retry` | A |
| `TestGRUBRetryStateFailureWindows` | `togi_hardware::grub::tests::exhausting_the_restart_limit_fails_in_each_command_without_granting_a_retry` | A |
| `TestGRUBReasonReadWriteUnsetFailures` | `togi_hardware::grub::tests::reason_reads_writes_and_clears_keep_the_cause_whether_or_not_the_effect_happened`<br>`togi_hardware::grub::tests::a_grub_error_names_the_environment_and_verb_and_keeps_its_cause` | A |

### `internal/hardware/host_linux_test.go` (11)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestRanking` | `togi_hardware::tests::ranking::reads_each_core_through_its_first_logical_cpu`<br>`togi_hardware::tests::ranking::a_missing_file_ends_the_read_and_keeps_the_ranks_before_it`<br>`togi_hardware::tests::ranking::a_core_without_a_cpu_is_an_error`<br>`togi_hardware::tests::host::the_host_ranking_reads_each_cores_first_cpu`<br>`togi_hardware::tests::host::an_unreadable_ranking_is_a_partial_without_values` | A |
| `TestClockCancellation` | `togi_hardware::tests::clock::a_cancelled_sleep_returns_cancelled_at_once`<br>`togi_hardware::tests::clock::cancelling_during_a_sleep_wakes_it` | A |
| `TestClockSleepDeadline` | `togi_hardware::tests::clock::an_uncancelled_sleep_lasts_its_duration`<br>`togi_hardware::tests::clock::the_monotonic_clock_counts_time_since_boot` | A |
| `TestRankingInvalidValue` | `togi_hardware::tests::ranking::text_that_is_not_an_integer_is_an_error_naming_the_cpu_and_text`<br>`togi_hardware::tests::ranking::a_value_beyond_i64_is_refused_instead_of_wrapping`<br>`togi_hardware::tests::ranking::a_negative_rank_is_read_as_is` | A |
| `TestBackendUserPreflight` | `togi_hardware::preflight::tests::a_resolved_account_reports_its_ids`<br>`togi_hardware::preflight::tests::an_unconfigured_account_is_refused_and_provides_no_identity`<br>`togi_hardware::preflight::tests::an_empty_account_name_is_refused_without_a_lookup`<br>`togi_hardware::tests::host::a_resolved_account_runs_the_scope_probe` | A |
| `TestBackendPreflight` | `togi_hardware::preflight::tests::unconfigured_backends_fail_naming_both`<br>`togi_hardware::preflight::tests::a_missing_backend_fails_and_the_other_is_still_reported`<br>`togi_hardware::preflight::tests::non_executable_backends_fail_with_every_cause`<br>`togi_hardware::preflight::tests::ready_backends_pass_and_name_their_binaries` | A |
| `TestPreflightRefusesUnvalidatedHardware` | `togi_hardware::tests::host::unvalidated::an_unsupported_cpu_and_a_missing_driver_skip_every_gated_check_and_touch_no_hardware`<br>`togi_hardware::tests::host::a_supported_cpu_with_no_driver_loaded_is_refused_and_skips_every_gated_check`<br>`togi_hardware::tests::host::unvalidated::a_supported_cpu_without_the_driver_is_refused_before_any_mailbox_access`<br>`togi_hardware::tests::host::unvalidated::a_missing_cpuinfo_is_refused_before_any_mailbox_access`<br>`togi_hardware::tests::host::unvalidated::root_with_a_valid_identity_reaches_the_mailbox_and_reports_a_refused_account`<br>`togi_hardware::preflight::tests::the_steps_run_in_the_order_of_the_runtime_spec`<br>`togi_hardware::preflight::tests::a_failed_identity_check_skips_every_check_that_needs_the_identity`<br>`togi_hardware::preflight::tests::both_identity_checks_run_even_after_the_first_fails`<br>`togi_hardware::preflight::tests::a_skipped_step_becomes_a_failed_check_carrying_its_reason` | A |
| `TestDiagnoseUnprivilegedMakesNoMailboxCalls` | `togi_hardware::tests::diagnose::an_unprivileged_diagnosis_touches_no_mailbox_or_pm_table_file`<br>`togi_hardware::tests::diagnose::an_unprivileged_diagnosis_of_an_unidentified_machine_skips_only_what_needs_root`<br>`togi_hardware::diagnose::tests::without_root_the_comparison_needs_root_and_reads_nothing`<br>`togi_hardware::preflight::tests::the_ran_and_skipped_steps_of_an_unprivileged_run_cover_every_step_once` | A |
| `TestDiagnoseCheckSetsPartitionPreflight` | `togi_hardware::tests::diagnose::the_privileged_and_unprivileged_check_sets_partition_the_preflight`<br>`togi_hardware::preflight::tests::a_privileged_identified_run_skips_nothing`<br>`togi_hardware::preflight::tests::without_root_the_hardware_and_systemd_steps_are_skipped_for_root` | A |
| `TestDiagnoseComparesBIOSContextOnlyAfterPassingChecks` | `togi_hardware::tests::diagnose::failed_checks_skip_the_bios_comparison`<br>`togi_hardware::diagnose::tests::a_failed_check_skips_the_comparison_without_reading_the_context`<br>`togi_hardware::diagnose::tests::a_failing_watchdog_does_not_gate_the_comparison`<br>`togi_hardware::diagnose::tests::no_recorded_context_makes_no_comparison_and_no_skip` | A |
| `TestBIOSContextCheck` | `togi_hardware::diagnose::tests::a_matching_context_is_compared_after_every_other_check_passed`<br>`togi_hardware::diagnose::tests::a_changed_context_fails_its_check_and_says_run_archives_the_session`<br>`togi_hardware::diagnose::tests::a_context_that_cannot_be_read_keeps_the_checks_already_made`<br>`togi_smu::tests::driver::checks_report_exact_details_and_bios_context`<br>`togi_smu::tests::driver::bios_context_read_failures_keep_their_cause` | A |

`TestBIOSContextCheck` read the context through a fake mailbox; the Rust comparison takes it through the `Survey` seam, and `togi-smu` pins reading it (DMI files, cpuinfo microcode, the boost limit command) and the cause of each read failure.

### `internal/hardware/watchdog_linux_test.go` (2)

| Go test | Rust test(s) | status |
|---|---|---|
| `TestWatchdogReadiness` | `togi_hardware::tests::watchdog::no_watchdog_state_is_not_ready`<br>`togi_hardware::tests::watchdog::an_inactive_device_is_not_ready`<br>`togi_hardware::tests::watchdog::an_active_hardware_device_is_ready`<br>`togi_hardware::tests::watchdog::the_software_watchdog_is_not_hardware`<br>`togi_hardware::tests::watchdog::an_empty_identity_is_not_hardware`<br>`togi_hardware::tests::watchdog::a_missing_identity_is_a_read_error`<br>`togi_hardware::tests::watchdog::an_unknown_state_is_not_ready`<br>`togi_hardware::tests::watchdog::a_later_hardware_device_makes_it_ready`<br>`togi_hardware::tests::watchdog::a_device_without_a_state_file_is_not_a_candidate`<br>`togi_hardware::tests::watchdog::directories_that_are_not_watchdogs_are_ignored`<br>`togi_hardware::tests::watchdog::every_refusal_is_reported_when_none_is_ready`<br>`togi_hardware::tests::host::the_host_watchdog_reads_its_root` | A |
| `TestWatchdogUnreadableState` | `togi_hardware::tests::watchdog::an_unreadable_state_is_refused`<br>`togi_hardware::tests::watchdog::an_unreadable_state_does_not_hide_a_later_hardware_device` | A |

macOS-only names: `togi_hardware::tests::other_platforms_refuse_hardware_runs` (cfg `not(target_os = "linux")`, twin of the Go unsupported-platform stubs, which have no Go test). No row of this slice uses it.
