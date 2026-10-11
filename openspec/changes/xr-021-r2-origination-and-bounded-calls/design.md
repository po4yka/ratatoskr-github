## Context

See proposal.md. The behaviour is defined by XR-021 CONTRACTS.md sections R2-08, R2-03, R2-04 and R2-10, which this change cites and does not restate.

## Decisions

### D1. A watch is the only origin of an analysis request

`apply_fresh_source` used to append an analysis command for the observing owner in the same transaction as the source revision, and watches queued a second, paced command. With the action route and the observer both feeding watches, a track would deliver two analyses to Knowledge. The direct publication is removed together with `repository_analysis_publications`; `apply_fresh_source` only persists the immutable source revision (metadata and README evidence) and `evaluate_metadata_watches` queues one request per enabled watch of each owner.

### D2. Registration order for a track, and evaluation from stored state

`WatchBaseline::Unevaluated` stores no checkpoint. Inside `record_fresh_repository` the source revision is applied first (`apply_fresh_source`), the `Unevaluated` watch is registered next, and `evaluate_metadata_watches` runs last, so the evaluation finds the null checkpoint and queues the first request. Registration of an `Unevaluated` watch needs no metadata row.

These three steps are separate transactions, so a process can die between them. Convergence does not depend on them being atomic: `evaluate_metadata_watches` takes no body and derives the analysis attributes and the README revision from the stored projection and the raw revision whose hash equals the projection's `content_hash`. Every observation evaluates, including one answered 304, and a watch whose `last_evaluated_content_hash` differs from the stored hash queues its request. A request lost between the commit and the evaluation is therefore queued by the next observation, even though the stored ETag already matches the provider. The action route self-heals the same way: a client retry refetches with no validator and evaluates again. `repository_metadata.readme_revision` is not null so the evaluation never has to guess the README.

### D3. One shared function for the route and the observer

`observe::record_fresh_repository` takes the already fetched fresh body, acquires from the rate-limit ledger and fetches the README through the same gateway (conditionally, with the stored validator), stores the blob, applies the source revision, optionally registers an `Unevaluated` watch for the tracking user, and evaluates the watches. One source revision therefore spends two units of the budget, one per provider request. The observer calls it with unauthenticated access; the route calls it with the caller's credential. A withheld README budget returns `ObserveError::RateLimited` with nothing applied, which the observer reports as `ObserveOutcome::RateLimited` and the route as a provider failure.

### D4. Request deduplication follows the whole source revision

The unique key becomes `(watch_id, idempotency_digest_hex, requested_contract)`. The idempotency digest already covers owner, repository, attributes and README revision, so a README-only change queues a request and an identical revision still queues none.

### D5. The observer is selected in SQL and runs as its own supervised task

A pass selects up to `BATCH` repositories with an enabled watch, never-attempted first and then by `repository_metadata.observe_attempted_at` ascending (ties by `fetched_at`), observes each through `observe_repository` with a shared `RateLimitLedger` (token label `watch-observer`), and a withheld budget ends the pass. The attempt is recorded after every outcome except a withheld budget, so a repository that cannot be read (a private repository, a deleted or moved one, a persistent 5xx) rotates behind the others instead of occupying the front of the queue; ordering by `fetched_at` alone would rotate only on success. The observer is its own task in the bus supervisor's `JoinSet` beside `relay_loop` and the consumers, so the supervisor stops the process when it fails. Its first pass runs after at most five seconds, then every interval, so a service restarted more often than the interval still observes. Interval 0 disables it.

### D6. Bounded provider calls

`ReqwestGithubApi::for_base_url` keeps the fixed 5 second connect timeout and the 30 second default total timeout; `with_timeout` rebuilds the client for the configured bound. Every request of the adapter goes through that client, so a stalled provider surfaces as `ProviderError::Transport`.

### D7. Revocation confirmation

`revoke_oauth_grant` returns success on 204. On 404 it sends `GET {base}/user` with the same access token and succeeds only on 401; any other status or a transport failure is a `ProviderError`, which `erase_account` already reports as `incomplete_external_grant_revocation`. GitHub's reference lists only 204 and 422 for the delete, so the probe, not the undocumented 404, is what makes the revocation verified.

### D8. Relay health

`flush_outbox` reports `Lifecycle::set_publisher_healthy(false)` when a pass attempted a publish, at least one failed, and the oldest unpublished publishable row has `attempt_count >= 3` or is older than 300 seconds. A row whose serialized envelope exceeds the connected server's `max_payload` minus 1 KiB is marked `undeliverable_at` at once and never counts. The unhealthy state never stops the relay.

Deviation from the literal R2-03 sentence, for the contract owner to accept or reject: R2-03 says the check is healthy again after a pass that publishes every due row or finds none. The implementation sets healthy only when the oldest unpublished publishable row is no longer stuck (or none waits); a pass with no failures that leaves a stuck head row backing off keeps the earlier verdict. Otherwise readiness would flap to 200 between retries of a row that is the reason for the 503. `bus_health.rs` pins the behaviour. If the owner prefers the literal rule, the change is the `(_, 0, Some(true))` arm of `report_health`; the proposed CONTRACTS.md wording is "healthy again once the oldest unpublished publishable row is no longer stuck, or none waits".

## Risks / Trade-offs

- [Removing the direct publication changes a specified capability] -> the capability is replaced by the watch path and its spec requirements are removed in this change; no consumer relied on the direct subject because no runtime caller existed.
- [A tracked repository whose metadata step failed keeps an unevaluated watch] -> the next observation queues the request; no analysis of old state is lost.
- [Provider errors drop the rate-limit headers] -> `ProviderError` carries no headers, so the ledger learns only from successful and 304 responses; a 403 or 429 does not feed it. Recommendation, outside this change: carry the parsed rate-limit headers in the error variants so the ledger can honour `Retry-After` from a refusal.
- [A change of only the star count, topics or push time is the same source revision] -> the source identity covers the analysis attributes and the README, so such a body only advances `fetched_at`; this is how `apply_fresh_source` behaved before this change.
