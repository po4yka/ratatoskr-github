## 1. Contract-backed policy derivation

- [x] 1.1 Add `crates/catalog/tests/backup_policy_flow.rs::derived_policy_contains_only_mirror_governed_repositories`, run it against the current schema, and confirm its entry assertion fails because Catalog cannot yet derive the typed policy.
- [x] 1.2 Add the immutable shared-contract dependency, current-schema policy fields/projections, and minimum derivation path in `crates/catalog/src/backup_policy.rs`; rerun the derivation test and verify the typed entry preserves cadence, priority, bytes, exclusions, and stable IDs.

## 2. Monotonic transactional publication

- [x] 2.1 Add `published_policy_versions_advance_only_when_derived_state_changes` to `backup_policy_flow.rs`, run it, and confirm its version/outbox assertion fails before publication history exists.
- [x] 2.2 Implement serialized policy history, canonical fingerprinting, and the transactional `cmd.vault.target.desired.v1` outbox insertion; rerun the version test and verify duplicate reconciliation emits neither a version nor a command.

## 3. Debounced reconciliation

- [x] 3.1 Add `burst_policy_changes_publish_once_after_the_trailing_deadline` to `backup_policy_flow.rs`, run it, and confirm its deadline/count assertion fails before durable debounce scheduling exists.
- [x] 3.2 Implement durable dirty-generation scheduling and wire policy-affecting mode and star-governance commits to it; rerun the debounce test and verify one current-state policy emits after the trailing deadline.

## 4. Vault feedback projection

- [x] 4.1 Add `rejected_acknowledgment_is_recorded_once_under_redelivery` to `backup_policy_flow.rs`, run it, and confirm its feedback assertion fails before typed acknowledgment handling exists.
- [x] 4.2 Implement inbox-backed `evt.vault.backup_policy.acknowledged.v1` handling and operator feedback query; rerun the feedback test and verify the reason and previous Vault version persist once without claiming actual backup success.

## 5. Integration and evidence

- [x] 5.1 Update the catalog documentation for the desired-policy/actual-state boundary and shared-contract SHA; documentation has no meaningful RED test, so verify its links and terminology by inspection.
- [x] 5.2 Run `openspec validate publish-vault-backup-policy --strict`, the full `DEVELOPMENT.md` gate through `build-gate`, and inspect the final diff; verify all prior tests remain green before marking the change complete.
