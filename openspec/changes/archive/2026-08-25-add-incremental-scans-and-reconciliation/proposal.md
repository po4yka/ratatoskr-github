# Proposal: add safe incremental scans and scheduled reconciliation

## Why

Full snapshots are currently the only synchronization mode, so keeping star state fresh means re-enumerating the whole starred listing as often as freshness is wanted - the legacy backend's daily whole-list cron, repeated more often and at growing cost. GitHub orders the starred listing by each repository's `starred_at`, which allows a cheap frequent scan bounded by a persisted high-water mark, provided two safety properties hold: a partial scan never establishes a removal, and any anomaly that breaks ordering coverage forces a full rescan instead of being papered over. This change adds that incremental mode together with periodic full reconciliation that detects drift against incremental state and records explicit repairs.

## What Changes

- Add an incremental scan flow: fetch the starred listing newest-first (`sort=created&direction=desc`), ingest items strictly newer than the account's persisted high-water mark until coverage of that window is proven, upsert identity and star state for what it sees, then advance the watermark - never recording an unstar from an incremental pass.
- Persist a per-account star watermark in the owned schema; advance it only after durable success.
- Treat ordering anomalies as gaps that force a full rescan: a listed item without a provider `starred_at`, or a non-monotonic `starred_at` sequence within or across resumed pages (boundary value carried in the checkpoint), fails the incremental run with a recorded reason, leaves authority untouched, does not move the watermark, and chains into a full snapshot.
- An incremental scan requested before any full baseline exists defers to a full snapshot instead of inventing a watermark.
- Record drift repairs explicitly: when a completed full snapshot flips state that prior observation had established (starred locally but absent upstream -> `unstar_after_drift`) or restores state a partial pass missed (listed again while locally unstarred -> `restore_after_miss`), one repair row per drifted repository is written inside the same atomic swap transaction. Re-running reconciliation on converged state writes no repairs and changes nothing - repairs are idempotent.
- Consume this service's own sync commands following the platform scheduler command grammar: subject `cmd.github.sync.requested.v1`, eight-member command envelope as published by `ratatoskr-platform`. Validate envelopes strictly, dedupe durably through `github_catalog.inbox_events` keyed by the command identity, dispatch the payload's requested mode (incremental by default), and chain a forced full rescan when the dispatched incremental detects a gap. Schedule registration stays with the documented operator mechanism (an operator inserts rows into platform's `operations.schedules`); this service adds no registration API.
- Out of scope: native star lists (item 6), watches (item 9), credential storage (item 2), live JetStream transport subscription and event publication (broker integration is a later coordinated changeset); command consumption is exercised at the domain boundary where the raw envelope arrives.

## Capabilities

### New Capabilities

- `sync-scheduling`: consuming this service's own `github.sync.requested.v1` commands - envelope validation under the platform scheduler grammar, durable idempotent consumption through the inbox, mode dispatch, gap-chained forced rescans, and registration of the frequent-incremental plus periodic-full schedules through the documented operator mechanism.

### Modified Capabilities

- `star-snapshot`: adds the incremental scan requirements - watermark-governed window ingestion that never infers removals, watermark persistence and advancement rules, ordering-gap detection that forces a full rescan, the no-baseline defers-to-full rule, and explicit recorded drift repairs inside the atomic swap with idempotent re-reconciliation.

## Impact

- `crates/catalog`: new `incremental` module (watermark read/advance, windowed scan, gap detection) and new `commands` module (envelope validation, inbox consumption, dispatch); `snapshot` gains repair recording inside its existing swap transaction; `lib.rs` re-exports the new surface.
- `schema.sql` edited in place: new `star_watermarks` table, new `reconciliation_repairs` table, nullable `boundary_starred_at` on `sync_checkpoints`; `tests/schema.rs` expectations updated. No migration history.
- Provider gateway: additional descending-order starred listing call on the `GithubApi` seam; the existing unordered call and all current wiremock expectations stay untouched.
- Tests: new `incremental_flow`, `reconciliation_flow`, and `sync_commands` suites beside the existing flow tests, wiremock-driven with disposable databases.
- Documentation: README status and sync sections, `docs/DATA_MODEL.md`, `docs/ARCHITECTURE.md`, `docs/INTERFACES.md` (command consumption boundary and the operator registration INSERT).
- Cross-repository: none changed. The command grammar and schedule registration are cited from `ratatoskr-platform` documentation (ADR-0005 subject grammar; scheduler architecture S10/S14; `deploy/README.md` registration example), not restated as local contracts.
