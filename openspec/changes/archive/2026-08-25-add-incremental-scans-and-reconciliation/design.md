# Design: add safe incremental scans and scheduled reconciliation

## Context

Item 4 delivered `run_full_snapshot` in `crates/catalog/src/snapshot.rs`: page-ascending enumeration through the `GithubApi` seam, per-page staging + checkpoints in one transaction, and an atomic authority swap (`apply_authority_and_complete`) that promotes additions, preserves established starred-at values, and records evidenced unstars. The schema already admits `sync_runs.mode = 'incremental'`, `inbox_events` already admits subject `github.sync.requested.v1`, and no Rust code touches the inbox yet. The platform side is documented, not guessed: subjects are `<class>.<contract-type-name>` (platform ADR-0005), the scheduler publishes a fixed eight-member command envelope (`ratatoskr-platform` `crates/eventing/src/command.rs`) whose payload is the schedule row's jsonb passed through verbatim, delivery is JetStream at-least-once so consumers dedupe durably, and schedule registration is an operator INSERT into `operations.schedules` - platform explicitly has no registration route and no self-registration command today.

Constraints that shape everything below: no migrations (schema edits in place), strict lints (no unwrap/expect outside tests, every public item documented, functions capped at ~100 lines, files at 850), tokens arrive as a `TokenRef` parameter because credential storage is item 2, and there is no broker dependency in this workspace yet.

## Goals / Non-Goals

**Goals:**

- Incremental scans that are cheap (fetch only the newer-than-watermark window) and provably safe (coverage is demonstrated by ordering, never assumed).
- Gap detection with a single escalation path: any anomaly that breaks ordering coverage forces a full rescan.
- Drift between incremental state and full reconciliation made visible: named repair rows written atomically with the authority swap, idempotent under repetition.
- Command consumption that matches the platform envelope exactly and dedupes like the rest of the fleet (extractor's inbox pattern).

**Non-Goals:**

- No JetStream subscription, NATS dependency, or background loop in this change; consumption is a domain entry point taking the raw envelope. Transport wiring lands with broker integration once credentials (item 2) exist.
- No event/outbox publication; `github.star.removed.v1` etc. remain unwritten.
- No star-list or watch behavior (items 6/9).
- No telemetry/metrics beyond what item 4 shipped (none); establishing the metric pattern is deferred deliberately to avoid inventing conventions nobody consumes yet.

## Decisions

### D1. Watermark lives in its own table, not on sync_checkpoints

`star_watermarks(account_id pk/fk, high_water_mark timestamptz not null, updated_at)` - one row per account, updated only on durable success. Alternatives rejected: a column on `github_accounts` conflates account lifecycle with scan progress; reusing `sync_checkpoints.next_page` cannot express a timestamp mark and would overload page semantics shared with full snapshots. The watermark is read at scan start and advanced in the same transaction that records the last ingested page, so "advance only after durable success" holds structurally.

### D2. Coverage rule: ordering proof, not heuristics

GitHub serves the listing newest-first under `sort=created&direction=desc`. The scan ingests items while `starred_at > watermark`; when it observes an item `<= watermark` it stops (the remainder is covered by earlier runs). If the provider reports exhaustion first, everything was newer - also complete. The watermark then advances to the oldest ingested timestamp of this run. Anything else - an item without `starred_at`, a next item newer than the previously seen one within a page, across pages, or across a resume boundary - is a **gap**: ordering can no longer prove coverage, so the run fails with reason recorded, staging clears, authority and watermark stand still, and the outcome demands a full rescan. Alternative considered: treat missing timestamps as "skip item" - rejected, because silently narrowing the window is exactly the false-confidence this item exists to remove.

### D3. Resume-boundary checking rides the existing checkpoint

`sync_checkpoints` gains nullable `boundary_starred_at`: the smallest `starred_at` seen so far in the run, written with each checkpoint. On resume the scan restores its monotonicity guard from the latest checkpoint instead of trusting memory. Full-snapshot rows leave it null; their order carries no meaning. This reuses the proven resume machinery rather than adding a second cursor format.

### D4. Provider seam grows one method

`GithubApi::list_starred_newest_first(token, page)` hits `/user/starred?sort=created&direction=desc&page=N` with the same `star+json` accept header and reply types. The existing unordered `list_starred` stays untouched so current wiremock expectations and the full snapshot keep their exact wire shape. One new method, not a params struct: two clear call sites beat one call site with a flag nobody can grep for.

### D5. No-baseline incremental defers to full, inside the flow

With no watermark row, `run_incremental_scan` does not create an incremental run at all; it invokes the full-snapshot path and reports that it did. Callers get one honest outcome instead of an error they must translate. A completed full snapshot re-anchors the baseline: watermark := newest observed `starred_at` (empty listing leaves it unset - nothing invented).

### D6. Drift repairs are rows inside the swap transaction

`reconciliation_repairs(sync_run_id, repository_id, action, recorded_at, pk(run_id, repository_id))`, `action in ('unstar_after_drift', 'restore_after_miss')`. During `apply_authority_and_complete`, the absence branch already computes exactly the locally-starred-but-absent set (record `unstar_after_drift` per row) and the promote branch sees locally-unstarred-but-present ids (record `restore_after_miss`). Same transaction as the state flip, so repairs can never disagree with the state they explain. Idempotence falls out twice over: the primary key blocks duplicate writes, and a second full snapshot on converged state finds no drift at all. Alternative considered: emitting drift as outbox events - rejected here, events are contracts needing a coordinated changeset and the acceptance is about recorded, auditable repair facts.

### D7. Command handling mirrors the platform envelope member-for-member

`commands.rs` parses exactly the eight members platform publishes (`command_id`, `command_type`, `requested_at`, `operation_id`, `tenant_id`, `correlation_id`, `idempotency_key`, `payload`); unknown extra members are ignored (forward-compatible), required ones are validated: type equality with `github.sync.requested.v1`, UUID identity, tenant `user:<uuid>`. Payload must be an object naming an account by owner reference (matches the documented example payload `{"account": ...}`) that exists and is `connected`, plus optional `mode: incremental|full`. Validation failure = typed error, zero side effects. Accepted commands insert `inbox_events(message_id = command_id, subject, payload)` first; conflict on the key means duplicate -> report and stop before any dispatch. Then dispatch: mode selects `run_incremental_scan` or `run_full_snapshot`; a gap outcome chains into `run_full_snapshot` immediately and both results travel in the report. Inbox dedup keys on `command_id`, following extractor's precedent; platform's transport-level `Nats-Msg-Id` equals its occurrence id, so if broker integration ever surfaces a divergence it gets resolved in that changeset - noted here so the choice is visible.

### D8. Tokens stay injected

Every flow keeps taking `&TokenRef` from its caller. Nothing fabricates, stores, or logs tokens; when credentials land (item 2) the caller resolves accounts to tokens and these signatures do not move.

## Risks / Trade-offs

- [Provider ordering is only as good as GitHub's sort] → Coverage never trusts a single page: monotonicity is checked continuously and any violation escalates to the full rescan that defines truth. Worst case cost is one extra full pass, never a wrong removal.
- [Commanded scans run with a token parameter but credentials do not exist yet] → Explicit non-goal; the domain surface is testable now and the future caller is a thin adapter.
- [`starred_at` equal to the watermark after clock ties] → Strictly-newer ingestion plus the stop rule means ties are skipped as already-covered; a genuinely re-starred repository with an identical timestamp would wait for the periodic full pass, which is correct-by-authority rather than wrong-by-inference.
- [Repair rows grow unbounded across many drifted reconciliations] → Rows are small, keyed per run, and audit-valuable; retention belongs with the broader evidence-retention policy, not this change.

## Migration Plan

Schema edits land in `schema.sql` and disposable test databases pick them up automatically; development status means no migration steps. Rollback is reverting the branch; no data survives that needs converting.

## Open Questions

None blocking. Transport-level dedup-key alignment (D7 note) is deliberately left to the broker-integration changeset.
