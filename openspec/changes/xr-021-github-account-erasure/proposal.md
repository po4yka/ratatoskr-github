## Why

Changeset XR-021 reproduced three erasure defects in GitHub: `delete_owner_state` matched `payload->>'account'`, a shape no producer writes, so outbox and inbox rows of an erased owner survived together with `repository_action_attempts`; repositories, README bytes and tracked-mode residue that only the erased owner reached were never collected; and the acknowledgement was recomputed from surviving state on redelivery, so a replay reported `Verified` after an `Incomplete` first answer. The wire behaviour is defined in XR-021 CONTRACTS.md section S12; this change cites it.

## What Changes

- Owner-keyed rows are found by one predicate `OWNER_KEYED_PAYLOAD` covering the real producers' shapes (`payload->>'owner'`, `payload->>'tenant_id'`, `payload#>>'{payload,account}'`) and used for the outbox, the inbox and the analysis publications; `repository_action_attempts` rows of the owner are deleted.
- Repositories that only the erased owner referenced are collected with their aliases, metadata, revisions, publications and backup policy; README bytes referenced only by deleted rows are collected; the backup policy is marked dirty so the next desired policy drops them. A census over `pg_constraint` keeps the classification of foreign keys into `repositories` complete.
- Acknowledgement is replay-safe: a new `account_erasure_operations` ledger commits atomically with the owner data deletion, a redelivered `operation_id` returns the recorded outcome with no provider call, and a reused `operation_id` for another owner is `OperationOwnerMismatch`.
- `revoke_oauth_grant` treats the documented already-revoked status as success.
- The delete list moves out of `account_erasure.rs` into `account_erasure_state.rs` (the 850 line cap).

## Capabilities

### New Capabilities

- `github-account-erasure`: erasure removes everything only the owner reached, converges after an interrupted attempt, and answers a redelivery with the recorded outcome.

### Modified Capabilities

None. `openspec/specs/` is empty by design.

## Impact

- Affects `schema.sql` (new ledger table), `crates/catalog` (`account_erasure`, `account_erasure_state`, `provider/authentication`, `backup_policy`) and its tests. No other repository is touched; transporting the acknowledgement over NATS is out of scope.
