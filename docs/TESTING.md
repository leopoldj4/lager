# Tests and plan checks

Each check and test group of the [implementation plan](../IMPLEMENTATION_PLAN.md)
maps to tests that check its exact expected result. Test names are
`file::function` under `tests/`, or `module::tests::function` for unit
tests under `src/`.

## Running the tests

```bash
cargo test
```

Provider tests need an S3-compatible service. In the development shell,
they start SeaweedFS themselves; set `RUSTGIT_TEST_S3_*` to test another
provider (see `tests/provider_s3.rs`):

```bash
cargo test --test provider_s3 -- --ignored
```

Load tests measure delay, capacity, and recovery. Set the requirements
with `RUSTGIT_LOAD_*` (see `tests/load.rs`):

```bash
cargo test --release --test load -- --ignored --nocapture
```

The randomized history test repeats with `RUSTGIT_TEST_SEED=<n>`.
`RUSTGIT_TEST_LOG=rustgit=debug` shows server logs in tests, and
`RUSTGIT_TEST_GIT` names the Git program for servers.

Before each release, run the provider tests and `rustgit check-storage`
against every intended provider (IMPLEMENTATION_PLAN.md Stage 3 item 11).

## Stage 1 checks

| Check | Tests |
|---|---|
| Two standard Git clients exchange changes | `stage1_local::two_clients_exchange_changes_through_the_server` |
| No success response before storage accepts the head change | `stage1_local::push_success_is_reported_only_after_storage_accepts_the_head`, `server_failures::a_stop_after_the_entry_upload_keeps_the_previous_state` |
| Thin packs, loose objects, annotated tags, deletion-only pushes | `stage1_local::loose_objects_and_thin_packs_survive_complete_cache_loss`, `stage1_local::annotated_tags_and_deletion_only_pushes_are_stored` |
| Updates that need no new objects | `stage1_local::ref_updates_that_need_no_new_objects_are_accepted` |
| An invalid ref change rejects the complete push | `stage1_local::an_invalid_ref_change_rejects_the_complete_push` |
| Recovery after complete cache removal | `stage1_local::loose_objects_and_thin_packs_survive_complete_cache_loss`, `stage2_servers::all_caches_lost_still_permits_recovery_from_storage` |
| A lost CAS response causes no duplicate change | `stage1_local::a_lost_cas_response_does_not_duplicate_the_push`, `stage1_local::an_unanswered_cas_is_repeated_once_and_applied_once`, `provider_s3::a_lost_cas_answer_from_the_provider_does_not_duplicate_the_push` |
| Large transfers stay within the memory limit | `memory::a_large_push_and_clone_stay_within_the_memory_limit` |
| All client access passes permission and state checks | `access_http::clients_need_credentials_and_repository_permission`, `access_http::paths_outside_the_repository_table_are_not_served`, `stage2_servers::a_server_without_storage_access_rejects_new_reads_and_writes` |
| Storage service: conditional creation, CAS, current reads | `provider_s3::the_provider_meets_the_storage_requirements`, `storage::provider_check::tests::in_memory_store_meets_the_storage_requirements` |
| `proc-receive` controls ref changes and success responses | [decision 1](decisions/0001-receive-with-proc-receive-hook.md) |

## Stage 2 checks

| Check | Tests |
|---|---|
| A read on server B includes a push that server A accepted | `stage2_servers::a_read_on_server_b_includes_a_push_that_server_a_accepted`, `provider_s3::three_server_processes_share_one_provider` |
| Competing changes to the same old ref cannot both succeed | `stage2_servers::competing_pushes_from_the_same_old_value_cannot_both_succeed`, `stage2_servers::a_delayed_cas_cannot_overwrite_a_newer_push` |
| Changes to different refs succeed without lost changes | `stage2_servers::concurrent_pushes_to_different_refs_all_succeed` |
| Readers never observe part of a transaction | `stage2_servers::readers_never_observe_part_of_a_transaction` |
| A server with an empty cache serves after recovery | `reads::simultaneous_reads_on_an_empty_cache_rebuild_it_once`, `stage2_servers::all_caches_lost_still_permits_recovery_from_storage` |
| Server loss, delayed requests, lost responses lose no accepted change | `server_failures::*`, `stage2_servers::a_history_with_random_storage_faults_obeys_the_transaction_rules`, `stage2_servers::a_change_accepted_by_storage_without_an_answer_is_visible_everywhere` |
| Loss of all caches still permits recovery | `stage2_servers::all_caches_lost_still_permits_recovery_from_storage` |
| A server without storage access rejects unchecked reads and writes | `stage2_servers::a_server_without_storage_access_rejects_new_reads_and_writes` |
| Requests to different servers; preferred server selection | `routing::git_operations_work_when_each_request_goes_to_another_server`, `routing::requests_go_to_the_preferred_server_and_move_when_it_stops`, `routing::accepted_pushes_notify_other_servers` |
| Three server processes with separate caches | `provider_s3::three_server_processes_share_one_provider` |
| Recorded request histories obey the transaction rules | `stage2_servers::a_history_with_random_storage_faults_obeys_the_transaction_rules` (and `check_history` in every multi-push test) |

## Stage 3 checks

| Check | Tests |
|---|---|
| Snapshots restore the same state as the complete log | `stage3_maintenance::a_snapshot_restores_the_same_state_as_the_complete_log` |
| Compaction cannot remove a push that another server accepts | `stage3_maintenance::a_push_that_overlaps_a_snapshot_is_retained`, `maintenance_faults::competing_compactors_keep_every_accepted_push` |
| Old read views remain usable until their readers finish | `stage3_maintenance::old_read_views_remain_usable_until_their_readers_finish`, `reads::a_cache_replacement_lets_active_readers_finish` |
| Data removal cannot break active requests or recovery | `stage3_maintenance::data_removal_does_not_break_an_active_push`, `stage3_maintenance::data_removal_reports_first_and_keeps_recovery_possible`, `maintenance_faults::the_removal_fence_stops_a_delayed_push_from_using_removed_data` |
| Recovery works in an empty independent system | `stage3_maintenance::a_backup_restores_into_an_empty_independent_system` |
| Limits prevent disk exhaustion and uncontrolled processes or memory | `stage1_local::a_push_without_disk_space_is_rejected_before_receipt`, `limits::*`, `memory::a_large_push_and_clone_stay_within_the_memory_limit`, `git::tests::client_request_processes_get_the_cpu_limit` |
| Another customer's data never becomes visible | `stage3_maintenance::one_customer_cannot_read_another_customers_repository`, `stage3_maintenance::a_reused_name_starts_an_empty_repository` |
| Measurements meet the agreed requirements | `load::repositories_of_three_sizes_meet_the_load_requirements` |
| Measurements per step, storage use, Git work, no series per repository | `access_http::measurements_cover_push_steps_storage_use_and_git_work` |
| Production identity, ref permissions, catalog, TLS | `production_access::*` |
| Diagnostic records | `diagnostics::diagnostic_records_name_requests_but_not_credentials_or_contents` |
| Automatic compaction on one server | `maintenance_faults::the_preferred_server_creates_snapshots_automatically` |

## Test groups

| Group | Case | Tests |
|---|---|---|
| Git objects | Thin packs, loose objects | `stage1_local::loose_objects_and_thin_packs_survive_complete_cache_loss` |
| | Missing dependencies | `protocol::a_pack_with_missing_dependencies_is_rejected` |
| | Damaged packs | `protocol::a_damaged_pack_in_a_push_is_rejected`, `stage3_maintenance::a_damaged_pack_is_detected_and_not_served` |
| | Empty receives | `stage1_local::a_push_to_an_empty_repository_with_no_commits_changes_nothing` |
| Refs | Create, delete, recreate | `protocol::a_deleted_branch_can_be_created_again` |
| | Tags, force push | `access_http::force_pushes_and_tag_moves_need_force_permission`, `stage1_local::annotated_tags_and_deletion_only_pushes_are_stored` |
| | Duplicate commands | `protocol::duplicate_commands_in_one_request_reject_the_push`, `model::tests::duplicate_commands_and_missing_deletions_are_rejected` |
| | Conflicting names | `stage1_local::conflicting_ref_names_are_rejected`, `model::tests::directory_file_name_conflicts_are_rejected` |
| | Default branch | `stage3_maintenance::a_default_branch_change_applies_to_new_clones` |
| Transactions | Atomic and ordinary pushes | `stage1_local::an_atomic_push_of_several_refs_is_one_log_entry`, `stage1_local::an_invalid_ref_change_rejects_the_complete_push` |
| | Rejected changes | `stage1_local::an_invalid_ref_change_rejects_the_complete_push`, `production_access::ref_rules_protect_selected_refs` |
| | Simultaneous writes | `stage2_servers::competing_pushes_from_the_same_old_value_cannot_both_succeed`, `stage2_servers::concurrent_pushes_to_different_refs_all_succeed` |
| HTTP protocol | Ref advertisements, content types | `access_http::http_responses_use_git_content_types_and_forbid_caching` |
| | Requests without saved sessions | `routing::git_operations_work_when_each_request_goes_to_another_server` |
| | Progress messages, final results | `protocol::progress_messages_and_final_results_reach_the_client` |
| | Disconnects | `access_http::a_push_request_cut_off_by_the_client_changes_nothing`, `limits::too_many_processes_get_a_busy_answer_and_a_canceled_request_frees_its_process` |
| Request results | Lost responses | `stage1_local::a_lost_cas_response_does_not_duplicate_the_push`, `request_results::lost_answers_for_records_do_not_apply_a_push_twice` |
| | Delayed requests | `request_results::delayed_storage_requests_do_not_change_the_result`, `stage2_servers::a_delayed_cas_cannot_overwrite_a_newer_push` |
| | Reused identifiers with different contents | `request_results::a_reused_request_identifier_with_different_contents_is_rejected` |
| Server failure | Stop before and after upload, ref preparation, CAS, local installation, success response | `server_failures::*`, `provider_s3::a_server_process_crash_before_or_after_the_cas_loses_no_accepted_push` |
| Reads | Old cache | `stage2_servers::a_read_on_server_b_includes_a_push_that_server_a_accepted` |
| | Long fetch, overlapping push | `reads::a_long_fetch_does_not_block_a_push_and_keeps_its_state` |
| | Cache replacement | `reads::a_cache_replacement_lets_active_readers_finish`, `server_failures::a_stop_during_cache_reconstruction_leaves_no_partial_cache` |
| | Partial local change | `reads::a_partial_local_change_is_not_served_after_a_restart` |
| Maintenance | Competing compactors, new pushes | `maintenance_faults::competing_compactors_keep_every_accepted_push`, `stage3_maintenance::a_push_that_overlaps_a_snapshot_is_retained` |
| | Interrupted uploads | `maintenance_faults::an_interrupted_snapshot_upload_leaves_the_head_unchanged`, `stage1_local::a_failed_pack_upload_keeps_the_previous_head` |
| | Damaged snapshots | `maintenance_faults::a_damaged_snapshot_is_detected_and_not_served` |
| | Missing packs | `maintenance_faults::a_missing_pack_is_detected_and_not_served` |
| Limits | Full disk | `stage1_local::a_push_without_disk_space_is_rejected_before_receipt` |
| | Slow clients | `reads::a_long_fetch_does_not_block_a_push_and_keeps_its_state`, `limits::too_many_processes_get_a_busy_answer_and_a_canceled_request_frees_its_process` |
| | Too many processes | `limits::too_many_processes_get_a_busy_answer_and_a_canceled_request_frees_its_process`, `limits::a_customer_at_its_process_limit_does_not_block_other_customers` |
| | Repeated retries | `limits::repeated_conflicts_end_with_a_retryable_answer_at_the_retry_limit` |
| | Canceled requests | `limits::too_many_processes_get_a_busy_answer_and_a_canceled_request_frees_its_process`, `access_http::a_push_request_cut_off_by_the_client_changes_nothing` |
| | Sizes and traffic | `stage1_local::a_push_larger_than_the_receive_limit_is_rejected_without_changes`, `limits::objects_over_the_size_limits_reject_the_push`, `limits::a_customer_over_its_received_bytes_budget_cannot_push`, `limits::a_customer_over_its_request_rate_is_told_to_retry_later` |
| Access | Invalid paths | `access_http::paths_outside_the_repository_table_are_not_served`, `config::tests::repository_names_reject_paths_and_special_segments` |
| | Unsafe Git configuration | `git_environment::hostile_git_environment_and_configuration_have_no_effect` |
| | Forbidden refs | `protocol::refs_outside_branches_and_tags_are_rejected`, `production_access::ref_rules_protect_selected_refs` |
| | Rejected-push objects | `access_http::objects_of_a_rejected_push_are_not_fetchable` |
| Repository lifetime | Empty repository | `stage1_local::a_push_to_an_empty_repository_with_no_commits_changes_nothing` |
| | Deletion during push | `stage3_maintenance::a_push_that_overlaps_deletion_does_not_restore_the_repository` |
| | Reused name | `stage3_maintenance::a_reused_name_starts_an_empty_repository`, `production_access::repositories_in_the_storage_catalog_are_created_deleted_and_reused` |
| | Complete cache loss | `stage2_servers::all_caches_lost_still_permits_recovery_from_storage` |

## Operation

Not plan checks; they support deployment behind a load balancer.

| Behavior | Tests |
|---|---|
| The health check fails while storage does not answer | `shutdown::the_health_check_fails_while_storage_does_not_answer` |
| A stopping server fails its health check and still serves requests during the drain delay | `shutdown::a_stopping_server_fails_its_health_check_and_still_serves_requests` |
| A running push completes and is answered before the server stops | `shutdown::a_running_push_completes_before_the_server_stops` |
| The shutdown timeout bounds the stop | `shutdown::the_shutdown_timeout_bounds_the_stop` |
| Other servers stop forwarding to a stopping server | `shutdown::other_servers_stop_forwarding_to_a_stopping_server` |
