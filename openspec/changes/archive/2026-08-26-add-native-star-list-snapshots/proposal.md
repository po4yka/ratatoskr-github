# Proposal: add-native-star-list-snapshots

## Why

Legacy parity requires the user's native GitHub star lists (collections): the monolith tracked which native lists contain each repository through its `star_list_filing` tasks, and the catalog's bounded context owns "native GitHub star lists and memberships". Stars are already synchronized (plan items 4 and 5), but the catalog cannot yet answer "which native GitHub star lists contain a repository?" - a question README names as core.

## What Changes

- Add a provider gateway method that enumerates the authenticated user's native star lists together with their repository memberships through GitHub GraphQL, keeping all wire types inside the adapter.
- Add a star-list snapshot flow mirroring the full star snapshot discipline: rate-budget-governed pagination, durable resumable checkpoints, per-page staging, and one atomic transaction that swaps the completed enumeration into list authority.
- Record membership transitions as append-only observations; removals are inferred only from a complete successful snapshot and named as observation times (`observed_removed_at`) with the establishing run as evidence.
- Tombstone lists that disappear upstream instead of deleting them; demote their memberships in the same swap.
- Treat a list whose membership exceeds the provider page size as a truncated enumeration: the run fails with the reason recorded and authority stays untouched, per the existing truncation rule.
- Extend the schema in place (development status: no migrations): `sync_runs` gains a `star_lists` mode plus `lists_observed`/`removals` statistics, checkpoints gain a GraphQL cursor column, staging gains a flat list-membership table, `star_lists` gains tombstone state, `star_list_memberships` becomes the current membership projection with evidence columns, and a new append-only `star_list_membership_observations` records diffs.
- Chain an independent star-list snapshot into commanded synchronization after the star-mode dispatch, reporting both outcomes; a list-sync failure never invalidates an otherwise successful star snapshot and vice versa.
- Export internal read surface functions returning current lists and their current members.
- Document the consistency rules between star snapshots and list snapshots: star state and list membership are independent dimensions - a repository can be starred but unlisted, unlisted but starred, listed but unstarred, and every combination is truthful.

Out of scope: creating or editing lists upstream (write-back territory), local Ratatoskr collections (Knowledge domain), item-level pagination inside a single list beyond the provider page cap (overflow is a failed run, not silent truncation).

## Capabilities

### New Capabilities

- `star-list-snapshot`: authoritative per-account native star-list state by complete enumeration of the user's star lists and memberships under the same atomic-authority discipline as stars - completed snapshot as sole removal authority, evidenced observations, resumable checkpoints, truncation refusal, tombstoned lists, orphan-truthful memberships, and a read surface for current lists and members.

### Modified Capabilities

- `sync-scheduling`: a handled sync command now also runs an independent star-list snapshot after dispatching the requested star mode, with both outcomes reported and neither able to invalidate the other.

## Impact

- `schema.sql`: in-place edits to `github_catalog.sync_runs`, `sync_checkpoints`, `star_lists`, `star_list_memberships`; new tables `list_snapshot_items`, `star_list_membership_observations`.
- `crates/catalog/src/provider.rs`: new `GithubApi` method + `ReqwestGithubApi` GraphQL call, wire types stay in the adapter.
- New `crates/catalog/src/star_lists.rs`: snapshot flow, outcome types, read functions; exported from the crate root like the other flows.
- `crates/catalog/src/commands.rs`: dispatch chains the list snapshot independently.
- Tests: new `crates/catalog/tests/list_snapshot_flow.rs`, extended `tests/schema.rs`, new fixtures pinning the GraphQL parsing contract.
- Docs: README (native star lists section, status note), `docs/DATA_MODEL.md`, `docs/ARCHITECTURE.md` (star/list independence rules).
