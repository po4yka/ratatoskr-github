# Design: add-native-star-list-snapshots

## Context

The full star snapshot (`crates/catalog/src/snapshot.rs`) established the authority discipline this change mirrors: rate-budget-governed page fetches, per-page staging transactions with durable checkpoints, one atomic swap transaction promoting a completed enumeration into authority, evidenced removals named as observation times, and failure paths that leave prior authority untouched. Placeholder tables `star_lists` and `star_list_memberships` exist in `schema.sql` but no code reads or writes them. Commanded synchronization (`commands.rs`) dispatches star modes only. Development status forbids migrations; schema edits happen in place.

Native star lists are read through GitHub GraphQL, not the REST listing endpoints; REST v3 offers no read path for collections. All wire types stay inside the adapter (`provider.rs`), matching how REST shapes are handled today.

## Goals / Non-Goals

**Goals:**

- Complete enumeration of the account's native lists and their memberships under the same atomic-authority discipline as stars.
- Membership transitions recorded as append-only observations; removals only from a complete successful snapshot.
- Truthful representation of orphans: a listed repository that is locally unstarred - or never star-observed at all - remains a plain membership row; star state and list state never constrain each other.
- Independent failure domains: list-sync outcome never rewrites star outcomes and vice versa.
- Internal read surface for current lists and their current members.

**Non-Goals:**

- Writing lists upstream (star/list mutations are plan item 7).
- Local Ratatoskr collections/tags (Knowledge domain).
- Item-level pagination inside one list beyond the provider page cap: overflow fails the run instead.
- Public HTTP API surface; the service exposes library entry points exactly as every earlier plan item did.

## Decisions

### D1: Enumeration via GraphQL over `User.lists`, inline items, cursor checkpoints

Verified against the live dotcom schema (`docs.github.com/public/fpt/schema.docs.graphql`, fetched 2026-08-25): star lists are exposed only through GraphQL - REST v3 has no list endpoints - via `User.lists(first:, after:) -> UserListConnection` whose `UserList` nodes carry `id` (stable GID string), `name`, `slug`, `isPrivate`, and item connections whose `UserListItemsEdge` holds ONLY `cursor` and `node` (union with `Repository`). There is no per-item added-at timestamp anywhere in the wire shape, so the catalog records none and models membership timing purely as observation times. One gateway method pages `viewer.lists` (first: 100) with each collection's items requested inline (first: 100); Relay cursors do not map to the existing integer `next_page`, so `sync_checkpoints` gains a nullable `graphql_cursor text`: null means first page, otherwise the lists connection's `endCursor` captured with the last staged page. Resume passes the stored cursor, preserving "no completed page is fetched again". Alternative rejected: encoding cursors into `next_page` integers (opaque string corruption risk) and two-phase traversal with per-list resume state (checkpoint complexity disproportionate to real list sizes).

### D2: Overflow inside a single list is a failed run, not silent truncation

If any collection's item connection reports `hasNextPage` after the inline page, the run terminates failed with reason naming the truncated list, staging clears, authority stays untouched. This is the existing truncation rule ("failed, cancelled, rate-limited, truncated snapshots do not change absence-based state") applied to lists; it keeps the checkpoint model single-sequence and honest. A future item-paginated traversal can lift the bound without touching authority semantics.

### D3: Flat staging keyed by run position

New table `list_snapshot_items(sync_run_id, position, provider_list_id, list_name, provider_repository_id)` stages one row per observed membership with list metadata denormalized; the swap deduplicates list identity. Mirrors `snapshot_items`; no second staging table or traversal segment.

### D4: `star_lists` mode in the run vocabulary

`sync_runs.mode` check gains `'star_lists'`. List runs are peer runs, not a sub-mode of `full`: they carry their own statistics columns (`lists_observed`, `removals`) rather than stretching `unstars` to mean membership removals. Alternative rejected: reusing mode `full` (statistics and failure reasons would conflate two authorities).

### D5: Membership projection carries both states with evidence

The provider supplies no per-item added-at timestamp (`UserListItemsEdge` exposes only cursor and node), so the projection records none and models membership timing purely as observation times. `star_list_memberships` is redefined in place as the current projection: `member boolean`, `last_observed_at`, `observed_removed_at` (inferred removal time, named as observation per bounded-context rules), `evidence_run_id`. Checks: `member = false` implies `observed_removed_at is not null`. Rows persist across removals (tombstone semantics) so history stays explainable; FK targets never vanish mid-evidence.

### D6: Observations record every seen membership plus every removal

Append-only `star_list_membership_observations(observation_id, list_id, repository_id, member, observed_at, evidence_run_id)`: one row per staged membership on completion (confirmations included, like stars) and one `member = false` row per demotion, all inside the swap transaction carrying the completing run as evidence. Scope explicitly asks diffs be observations, not repair rows; `reconciliation_repairs` stays star-only.

### D7: Swap transaction order mirrors the star swap

`apply_list_authority_and_complete` in ONE transaction: count statistics; upsert `star_lists` identity/name from staging (rename propagates, local `created_at` preserved); insert completion observations; promote staged pairs (`member = true`, clear `observed_removed_at`, set evidence); demote locally-member-but-absent pairs (`member = false`, `observed_removed_at = now()`, evidence); tombstone absent lists (`status = 'removed'`, `observed_removed_at`) and demote their memberships; clear staging; complete the run row. Readers see prior or new authority, never a mixture.

### D8: Commanded sync chains the list snapshot independently

After dispatching the requested star mode, `handle_sync_command` attempts `run_star_list_snapshot` for the same account and reports both outcomes. Either part may fail, pause, or complete without altering the other's result or rows. Envelope grammar is unchanged (no new payload keys); legacy parity wants lists refreshed by every scheduled sync, which this achieves without operator action.

### D9: Read surface as exported crate functions

`current_star_lists(database, account_id) -> Vec<StarListSummary>` (active lists only) and `current_list_members(database, list_id) -> Vec<ListMember>` (`member = true`), following the crate's private-module-plus-re-export convention. No HTTP layer exists yet; tests exercise the functions directly over disposable databases.### D10: Rate accounting reuses the ledger

Each page acquires from the shared per-token budget first and observes the reply's limit data after; GraphQL replies carry a `rateLimit { cost remaining resetAt }` object (headers remain preferred by GitHub but the object is queryable in the same request), normalized into the existing internal header shape inside the adapter so the ledger stays unchanged. GraphQL errors returned in the response envelope map through the provider error classification.

## Risks / Trade-offs

- [Provider wire shape drifts] → A parsing-contract test pins the GraphQL response shape against a committed synthetic fixture, like `fixtures_contract.rs` does for REST bodies; adapter normalization isolates all field names.
- [Lists larger than 100 items fail every run] → Recorded failure reason names the list; acceptable while real-world lists stay small; lifting requires only a deeper traversal, not new authority semantics.
- [GraphQL rate budget differs from REST budget] → Normalization maps cost/remaining/resetAt into the ledger shape; conservative acquisition still bounds request rates.
- [Commanded sync now performs more requests] → List snapshot runs after the star dispatch under the same budget; a paused star scan naturally precedes a paused list scan rather than doubling spend.

## Migration Plan

None permitted or needed: development status edits `schema.sql` in place and disposable databases are created fresh from it. Rollback is reverting the branch; no deployed data exists.

## Open Questions

None blocking. Exact GraphQL field names are settled against the public schema before the fixture contract task lands; if the public API cannot express the enumeration, that finding changes the approach and returns to planning rather than being patched in code.
