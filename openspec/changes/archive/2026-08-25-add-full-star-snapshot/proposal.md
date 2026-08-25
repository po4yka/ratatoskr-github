# Proposal: add full star snapshot with atomic authority and checkpoints

## Why

The catalog currently holds no star state at all, so nothing can answer "which repositories are starred" - the central question of this bounded context. Legacy imported the whole stars list daily with authority in one wholesale-updated table; this change brings that authority forward on the new schema while keeping the bounded-context invariant: absence from an incomplete listing never proves an unstar.

## What Changes

- Add a full-scan job that enumerates a GitHub account's complete starred-repository set page by page through the provider gateway, under the shared per-token rate-limit ledger.
- Persist scan checkpoints after each durably processed page so an interrupted run resumes from the next page without refetching completed pages.
- Upsert observed repositories into stable identity as pages arrive, before any authority decision; metadata projection refreshes stay the existing observe flow's business.
- Swap snapshot authority atomically: one transaction promotes the completed snapshot's observations into `current_star_state`, preserving `provider_starred_at` for continuing stars and recording absent repositories as unstar observations with `observed_unstarred_at` plus the establishing snapshot as evidence - never silent deletions.
- Record every snapshot attempt as a `sync_runs` row (mode `full`) with terminal outcome and item statistics.
- Out of scope: incremental scans and high-water marks (implementation plan item 5), native star lists (item 6), external mutations, event publication.

## Capabilities

### New Capabilities

- `star-snapshot`: full-enumeration star snapshots - pagination under rate budgets, resumable checkpoints, atomic authority swap, unstar observation evidence, and run accounting.

### Modified Capabilities

- None. Repository identity, metadata projection, and the provider gateway keep their current requirements; the snapshot flow consumes them as-is.

## Impact

- `crates/catalog`: new `snapshot` module composing existing `identity`, `metadata`, `rate_limit`, and `provider` seams; exports for tests and future callers.
- `schema.sql`: extend `sync_runs` (account reference, statistics, failure reason) and `sync_checkpoints` (page cursor position) in place; no migration history is created.
- Provider gateway: add paginated starred-repository listing (`GET /user/starred`) to the `GithubApi` seam; response types stay inside the adapter.
- Tests: wiremock-driven pagination harness including mid-run failures; disposable-database integration tests for resume, atomic visibility, unstar evidence, and timestamp continuity.
- No public API or cross-repository contract changes in this change.
