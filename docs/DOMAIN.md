# GitHub Catalog domain model

## Terms

- **Account connection:** GitHub identity, encrypted credential, scopes, and status.
- **Repository:** stable GitHub numeric ID and mutable aliases/metadata.
- **Star observation:** evidence that a repository was starred at a provider-observed time.
- **Full snapshot:** complete successful enumeration authoritative for absence.
- **Star list:** native provider collection and memberships.
- **Repository mode:** whose decision governs a catalog entry: `auto` (star-driven presence), `tracked` (explicitly kept without a star), or `ignored` (deliberately excluded); unclassified means known but never classified.
- **Mutation audit:** append-only record of every provider write attempt and mode transition - who confirmed it, through which calling source, what was targeted, how it ended - keyed by idempotency so retries converge on one outcome.
- **Watch rule:** user-owned enabled/paused metadata-delta policy for one repository. It records the
  last evaluated metadata checkpoint and names `repository_analysis` as its downstream action.
- **Repository analysis request:** Catalog-owned outstanding-work record. `queued` and `pending` are
  visible still-indexing states; only Knowledge can resolve it with a matching terminal fact.
- **Backup policy:** desired Vault target, LFS/auxiliary options, retention, pinning.

## Invariants

1. Partial scans cannot remove stars or memberships.
2. Provider identity and local user intent are separate.
3. Successful provider mutation is not rolled back because a later local step failed.
4. `pinned` preservation intent is never silently removed.
5. Catalog never executes Git or accesses Vault storage.
6. Every external write has consent, audit, and idempotency.
7. Synchronization promotes only unclassified repositories to `auto`; explicit modes are never overridden by sync evidence.
8. A repository cannot be `ignored` while starred, and starring cannot bypass `ignored`.
9. A watch's current checkpoint establishes its registration baseline; the same immutable revision
   can create at most one request for that watch.
10. Catalog sends only bounded repository metadata and an explicit README absence state to Knowledge;
    it never makes an LLM, budget, or retry decision.
