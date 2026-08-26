## Why

GitHub Catalog already records repository mode and the desired backup level, but it does not yet turn that state into an authoritative, auditable desired-state document for Vault. The legacy scheduled mirror jobs implicitly decided what to preserve; publishing the explicit policy lets Vault reconcile actual preservation without taking ownership of catalog intent.

## What Changes

- Derive one complete, versioned `DesiredBackupPolicy` from catalog repository modes, backup policies, metadata size hints, and explicit exclusions.
- Persist monotonic publication revisions and an idempotent, debounced reconciliation request; publish `vault.target.desired.v1` through the transactional outbox only after the catalog transaction commits.
- Consume `vault.backup_policy.acknowledged.v1` through an idempotent inbox and retain accepted/rejected feedback and reasons for operator visibility.
- Depend on the already-published `ratatoskr-backup-contracts` contract crate pinned to its immutable commit; no new wire contract is introduced here.

## Capabilities

### New Capabilities

- `vault-backup-policy-publication`: Catalog-derived desired backup-policy publication, debounced reconciliation, and Vault acknowledgment projection.

### Modified Capabilities

None.

## Impact

The catalog schema gains its publication/reconciliation and feedback projections in place, with no migration history. `crates/catalog` adds the contract dependency and persistence/service logic; its disposable-PostgreSQL tests prove derivation, monotonic versions, debounce/idempotency, and acknowledgment recording. Vault remains a downstream consumer: this repository neither executes Git nor decides retention.
