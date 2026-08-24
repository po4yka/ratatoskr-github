# GitHub Catalog implementation plan

1. Scaffold service, typed config, telemetry, errors, health, and schema. *(implemented)*
2. Implement encrypted account credentials, PAT then OAuth PKCE, scopes, revoke.
3. Implement stable repository identity, aliases, metadata, and conditional requests. *(implemented)*
4. Implement full star snapshot with atomic authority and checkpoints.
5. Add safe incremental scans and scheduled reconciliation.
6. Implement native star-list snapshots.
7. Add repository modes and idempotent star/list mutations with partial success.
8. Publish versioned desired backup policy to Vault.
9. Add watches and Knowledge analysis requests.
10. Import legacy data, run shadow sync, then cut over reads and writes.

Definition of Done: no false removals, credentials/scopes secure, rate limits respected, mutations
audited/idempotent, the current schema and workspace GitHub-to-Vault vertical slice pass. Deferred:
broad issue/PR/discussion archival and organization administration.
