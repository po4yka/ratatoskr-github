# GitHub Catalog data model

## Owned schema: `github_catalog.*`

- `accounts`, encrypted `credentials`, scopes, expiry/status, rate-limit state.
- `repositories`, `repository_aliases`, owner/visibility/metadata revisions.
- `star_observations`, `current_star_state`, `star_snapshots`, pages/checkpoints.
- `star_lists`, `star_list_memberships`, list snapshots.
- `repository_modes`, `watch_rules`, `backup_policies`, analysis references.
- mutation audits, sync runs, outbox/inbox.

## Constraints

Repository provider ID is unique. Credentials are encrypted and excluded from events/logs. A snapshot becomes authoritative atomically only after complete enumeration. Observed timestamps are named honestly. Desired policy is versioned and distinct from Vault actual status. Cross-schema writes/foreign keys are forbidden.

Retention preserves audit and historical observations while respecting account disconnect, privacy deletion, and local backup retention policy.
