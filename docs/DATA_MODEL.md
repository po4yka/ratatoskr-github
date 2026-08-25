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

Full star snapshots (item 4) use `sync_runs` (account reference, terminal status, failure reason, item statistics), `sync_checkpoints` (the next page to fetch after each durably processed page), and the per-run staging table `snapshot_items`; the authority swap promotes a completed snapshot into `current_star_state` - whose `starred_at` preserves the earliest established provider value - and appends evidenced unstars to `star_observations` in one transaction.

Incremental scans and scheduled reconciliation (item 5) extend the same tables in place: `sync_runs.mode = 'incremental'` marks windowed passes; `sync_checkpoints.boundary_starred_at` carries each checkpoint's smallest seen provider `starred_at`, restoring the ordering guard across resumed runs; `star_watermarks` holds one high-water mark per account, advanced only on durable success and re-anchored by a completed snapshot to its newest observation; `reconciliation_repairs` records one named action per drifted repository per completing run (`unstar_after_drift` for locally starred but absent, `restore_after_miss` for locally unstarred but listed again), keyed `(sync_run_id, repository_id)` so repetition cannot duplicate a repair. Command consumption claims deliveries through `inbox_events`, keyed by the platform command identity.

Native star-list snapshots (item 6) extend the schema in place as a peer authority: `sync_runs.mode = 'star_lists'` marks list runs with their own `lists_observed` and `removals` statistics; `sync_checkpoints.graphql_cursor` carries the Relay continuation token of cursor-paginated GraphQL enumeration; `list_snapshot_items` stages one flat row per observed membership per run. `star_lists` holds provider list identity keyed `(account_id, provider_list_id)` with tombstone state (`status`, `observed_removed_at`, `evidence_run_id`) instead of deletion. `star_list_memberships` is the current membership projection - `member`, `last_observed_at`, `observed_removed_at`, `evidence_run_id` - where rows persist across removals so every transition stays explainable; the provider supplies no per-item added-at, so membership timing is modeled purely as observation times. `star_list_membership_observations` is append-only evidence: one row per membership seen by a completed enumeration plus one row per evidenced removal, all bound to the completing run.

## Constraints

Repository provider ID is unique. Credentials are encrypted and excluded from events/logs. A snapshot becomes authoritative atomically only after complete enumeration. Observed timestamps are named honestly. Desired policy is versioned and distinct from Vault actual status. Cross-schema writes/foreign keys are forbidden.

Retention preserves audit and historical observations while respecting account disconnect, privacy deletion, and local backup retention policy.
