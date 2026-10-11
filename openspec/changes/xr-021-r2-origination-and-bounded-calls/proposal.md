## Why

Changeset XR-021 round 2 (CONTRACTS.md section R2-08, R2-03, R2-04 rule 2, R2-10) found that the GitHub Catalog never originates a repository-analysis request at runtime: the relay, the dispatcher and the result consumers are real, but nothing in `services/` registers a watch or observes a repository, so Knowledge's repository durable receives nothing in production. Provider calls have no timeout, so previews, actions and account erasure hang on a stalled provider. A 404 from the grant revocation is accepted as already revoked on an undocumented assumption. A relay that cannot publish leaves the service ready. The README still says watches and event publication do not exist, and there is no deploy example.

## What Changes

- A `track` action registers a metadata-delta watch for the acting user with `WatchBaseline::Unevaluated`, so tracking a repository queues its first analysis request. `metadata` and `star` register no watch (R2-08 a).
- The action route and a new periodic observer share one function that fetches the README, applies the immutable source revision and evaluates every enabled watch; the queued request carries a real README `BlobRef` or the real absence reason, and `not_preserved` is no longer produced by a runtime path (R2-08 b, c).
- **Deliberate break**: `apply_fresh_source` no longer appends its own analysis command, the table `repository_analysis_publications` is removed from `schema.sql`, and `observe_repository` loses its `analysis_owner` parameter. Two producers of the same request would deliver two analyses for one track; a watch is the only origin because a request is always owned by a user who asked for it. The `repository-observation-events` requirements are removed.
- **Deliberate break**: `register_repository_analysis_watch` gains a `baseline` parameter; `repository_watches.last_evaluated_content_hash` becomes nullable (null means never evaluated); the request deduplication key becomes the idempotency digest of the whole source revision so a README-only change queues a request.
- `RATATOSKR__WATCH__OBSERVE_INTERVAL_SECONDS` (default 3600, 0 disables, at most 604800) and `RATATOSKR__WATCH__BATCH` (default 10, 1 to 100) drive the observer inside the bus supervisor.
- Every `ReqwestGithubApi` call is bounded: connect timeout 5 s, total timeout `RATATOSKR__PROVIDER__TIMEOUT_MS` (default 30000, 1000 to 120000); a timeout is `ProviderError::Transport` (R2-08 d).
- A 404 from `DELETE /applications/{client_id}/grant` is accepted only after `GET /user` with the same access token answers 401 (R2-08 e).
- The relay reports its health to `Lifecycle`: three failed attempts on the oldest publishable row, or 300 seconds, make `/ready` answer 503 with a `bus_publish` check; `outbox_events.undeliverable_at` marks an oversize row that can never be published (R2-03, R2-04 rule 2).
- `deploy/systemd/github.conf.example` ships and is loaded by a test; README and DEVELOPMENT.md describe what exists (R2-10).

## Capabilities

### New Capabilities

- `repository-origination`: GitHub originates repository-analysis requests at runtime from user-owned watches, through the action route and a periodic observer.

### Modified Capabilities

- `repository-watches`: registration takes an explicit baseline.
- `repository-observation-events`: the direct per-revision publication is removed.
- `provider-gateway`: provider calls are bounded.
- `github-oauth-grant-revocation`: a 404 is confirmed by a probe.
- `service-foundation`: relay health reaches readiness.

## Impact

- Affects `schema.sql` (edited in place, no migration), `crates/catalog` (`watches`, `metadata`, `observe`, `config`, `provider`, `account_erasure_state`), `services/catalog` (`repository_api`, `bus`, `lib`, `main`), `deploy/systemd`, README, DEVELOPMENT.md and AGENTS.md.
- Out of scope: the account-erasure NATS transport, the re-queue sweep for dropped events, star-list and incremental scans, the Vault policy lane, live calls to api.github.com, and any change to Knowledge.
