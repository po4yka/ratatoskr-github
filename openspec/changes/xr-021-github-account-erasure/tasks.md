## 1. Real shapes and action attempts (S12)

- [x] 1.1 RED: rewrite the erasure seeding to the real producers' shapes and add `account_erasure_state.rs` tests; surviving `repository_action_attempts`, `outbox_events` and `inbox_events` rows fail the assertions.
- [x] 1.2 GREEN: move the delete list to `account_erasure_state.rs`, add `OWNER_KEYED_PAYLOAD` and the action attempts delete.

## 2. Collection of what only the erased owner reached (S12)

- [x] 2.1 RED: add `repositories_only_the_erased_owner_referenced_are_collected_with_readme_bytes` and `every_foreign_key_into_repositories_is_classified`.
- [x] 2.2 GREEN: capture candidates, delete unreferenced candidates with their children, collect README bytes, mark the policy dirty.

## 3. Replay-safe acknowledgement (S12)

- [x] 3.1 Confirm the already-revoked status of `DELETE /applications/{client_id}/grant` in GitHub's current REST documentation; documentation lookup, no RED. Result: the reference lists only 204 and 422 (validation failed, or the endpoint has been spammed) and no already-revoked status, so 404 is treated as already revoked as an undocumented provider behavior and 422 stays an error.
- [x] 3.2 RED: add `redelivered_erasure_returns_the_recorded_outcome`, `a_reused_operation_id_for_a_different_owner_is_a_mismatch`, `revoke_treats_404_as_already_revoked` and `ledger_row_commits_atomically_with_owner_data`.
- [x] 3.3 GREEN: add `account_erasure_operations`, the ledger lookup, owner digest check, atomic insert, `OperationOwnerMismatch` and the revoke status handling.

## 4. Final gate

- [x] 4.1 Run the full local gate and `openspec validate --all --strict`.
