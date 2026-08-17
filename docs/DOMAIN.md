# GitHub Catalog domain model

## Terms

- **Account connection:** GitHub identity, encrypted credential, scopes, and status.
- **Repository:** stable GitHub numeric ID and mutable aliases/metadata.
- **Star observation:** evidence that a repository was starred at a provider-observed time.
- **Full snapshot:** complete successful enumeration authoritative for absence.
- **Star list:** native provider collection and memberships.
- **Repository mode:** `metadata`, `track`, or `star` user intent.
- **Watch rule:** monitored repository policy.
- **Backup policy:** desired Vault target, LFS/auxiliary options, retention, pinning.

## Invariants

1. Partial scans cannot remove stars or memberships.
2. Provider identity and local user intent are separate.
3. Successful provider mutation is not rolled back because a later local step failed.
4. `pinned` preservation intent is never silently removed.
5. Catalog never executes Git or accesses Vault storage.
6. Every external write has consent, audit, and idempotency.
