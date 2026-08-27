## Why

The retired monolith still contains users' repository, star, and native-list history, while the new catalog has no safe path to establish that baseline or prove that its synchronization agrees before it becomes authoritative. A bounded import and shadow-to-cutover procedure is needed to move the state without carrying forward legacy schema access or encrypted credentials.

## What Changes

- Add a one-shot, operator-invoked legacy PostgreSQL importer that maps legacy repository identities, account connection metadata, stars, and native-list state into the current catalog schema. It preserves supplied `starred_at` values and is idempotent.
- Add the supported credential re-registration flow required to reconnect an imported account. Import account records only as `reauthorization_required`: the importer never selects, copies, decrypts, logs, or fixtures encrypted legacy credentials, and it cannot make the account connected.
- Add a bounded shadow-sync invocation that runs the catalog's normal synchronization against re-registered accounts and emits a redacted, machine-readable and human-readable diff report without changing legacy data or making legacy absence authoritative.
- Add an owner-gated cutover checklist and rollback procedure. Reads and writes may switch only after the owner has reviewed and approved a clean shadow report; rollback restores the previous application routing without re-importing or deleting catalog evidence.
- Add synthetic, secret-free fixture coverage for import idempotence, identity mapping, timestamp preservation, credential exclusion, shadow diff reporting, and refusal to execute an unapproved cutover.

## Capabilities

### New Capabilities

- `legacy-catalog-transition`: Safe, one-shot legacy-state import, shadow comparison, and owner-approved cutover controls for GitHub Catalog.
- `account-credential-reconnect`: Secure credential registration and account-status transition required before an imported account can synchronize or mutate GitHub state.

### Modified Capabilities

- None.

## Impact

- Affects the catalog domain library, service command surface, current `github_catalog` schema, synthetic test fixtures, operator documentation, and deployment/cutover runbook.
- The importer requires a temporary, owner-supplied legacy schema manifest and sanitized fixture dump before it can be exercised against retired data. No legacy source, dump, token, or credential material is committed, logged, or retained by the service.
- No cross-repository contract is introduced: the importer and shadow report are operator-local, and cutover routing remains unavailable until the owner supplies explicit approval.
