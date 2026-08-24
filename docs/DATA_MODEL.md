# GitHub Catalog data model

## Owned schema: `github_catalog.*`

- `accounts`, encrypted `credentials`, scopes, expiry/status, rate-limit state.
- `repositories`, `repository_aliases` (with live/superseded status and redirect targets), owner/visibility/metadata revisions.
- `repository_metadata`, one current projection plus conditional-request validator, and `repository_metadata_revisions`, raw observed bodies pruned to a bounded recent window.
- `star_observations`, `current_star_state`, `star_snapshots`, pages/checkpoints.
- `star_lists`, `star_list_memberships`, list snapshots.
- `repository_modes`, `watch_rules`, `backup_policies`, analysis references.
- mutation audits, sync runs, outbox/inbox.

## Implemented tables

`repositories` and `repository_aliases` (implementation plan item 3): the provider numeric ID is unique; exactly one repository may hold an alias value live (`status = 'active'`) at a time, enforced by a partial unique index; superseded rows keep redirect history resolvable after renames, transfers, or name reuse. `repository_metadata` and `repository_metadata_revisions`: the projection carries description, language, stargazer count, topics, default branch, `pushed_at`, and the stored ETag; every distinct observed body appends one revision and history is pruned to the most recent window per repository.

## Constraints

Repository provider ID is unique. Credentials are encrypted and excluded from events/logs. A snapshot becomes authoritative atomically only after complete enumeration. Observed timestamps are named honestly. Desired policy is versioned and distinct from Vault actual status. Cross-schema writes/foreign keys are forbidden.

Retention preserves audit and historical observations while respecting account disconnect, privacy deletion, and local backup retention policy.
