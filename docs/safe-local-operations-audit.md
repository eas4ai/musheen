# Safe Local Operations Audit

Audit date: 2026-09-19

## Scope

This review covers local deletion, overwrite, move, restore, metadata privilege,
and path-conversion boundaries. It also records the tests that protect the last
valid copy of user data.

## Data-Loss Boundaries

| Boundary | Safety rule | Evidence |
| --- | --- | --- |
| Normal delete | Trash support and every target identity are checked before mutation. Unsupported Trash never falls back to permanent deletion. Each successful item returns a restore receipt. | `delete.rs`: `normal_delete_refuses_without_trash_and_never_falls_back`, `trash_reports_partial_failure_and_receipts_can_restore_without_replacement` |
| Permanent delete | A confirmation digest is bound to the exact target set. The local provider revalidates identity through a parent directory descriptor, uses `NOFOLLOW`, and does not cross nested mounts. This is the only path intended to remove the last copy. | `delete.rs`: `permanent_delete_requires_a_confirmation_bound_to_exact_scope`; `mutations.rs`: `local_permanent_delete_removes_the_selected_tree_without_following_symlinks` |
| Cross-filesystem move | The destination is staged, verified, published, and made durable before source removal. Source identity is rechecked before removal. An uncertain removal result retains the destination and requires review. | `move.rs`: `cross_filesystem_move_verifies_and_publishes_before_source_removal`, `ambiguous_source_removal_never_claims_the_source_still_exists` |
| Replace | The old destination is moved with `RENAME_NOREPLACE` to an app-owned sibling backup and the parent is synced. A failed transfer deletes the new destination only when the source is known to remain. Ambiguous atomic moves or source removal preserve both the possible new destination and the backup. | `move.rs`: `ambiguous_atomic_move_never_allows_destination_removal_during_rollback`; local mutation rollback tests |
| Directory merge | The full tree is preflighted before mutation. Entries move with `RENAME_NOREPLACE`; rollback reverses recorded moves. Backups are removed only after publication succeeds. Trash metadata is removed before the prior destination backup is discarded. | `mutations.rs`: `local_trash_directory_merge_preserves_both_trees`; `drop_operations.rs`: `resolved_directory_copy_merge_preserves_disjoint_children` |
| Recovery staging | Cleanup accepts only a parsed `StagingPath` with Musheen's exact owned-name format. Missing or ambiguous state becomes Needs Attention instead of an automatic destructive action. | `operations.rs`: `recovery_staging_can_be_discarded_only_through_an_app_owned_name`; `recovery_matrix.rs` |

## Privilege Boundary

The current local operation queue never invokes `sudo`, Polkit, a shell, or an
elevated Musheen process. Metadata preflight records `requires_privilege`; the
ordinary provider executes as the current user and returns permission errors.
The authorization broker specified by SYS-028 through SYS-030 must consume that
signal later without weakening these identity and path checks.

## Path Boundary

`StorePath` remains the lossless operation type. `DisplayPath` is used only for
labels and messages and cannot be passed to operation APIs. Local conversions
reject non-local paths; queued mutations require absolute paths and reject
parent traversal. Local mutations operate through opened parent descriptors,
revalidate identities, and use no-follow flags at destructive boundaries.
Non-UTF-8 names round-trip through the core, local mutation, link, and copy
tests.

## Crash Evidence

The deterministic recovery matrix injects failures before and after journal
append, sync, snapshot publication, parent sync, reset, metadata, publication,
and source-removal boundaries. A separate integration test kills a child
process after each of the seven durable journal phases and verifies that the
real file-backed journal reopens with the exact durable prefix.
