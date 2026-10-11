## Context

See proposal.md. The behaviour is defined by XR-021 CONTRACTS.md sections R2-08, R2-03, R2-04 and R2-10, which this change cites and does not restate.

## Decisions

### D1. A watch is the only origin of an analysis request

`apply_fresh_source` used to append an analysis command for the observing owner in the same transaction as the source revision, and watches queued a second, paced command. With the action route and the observer both feeding watches, a track would deliver two analyses to Knowledge. The direct publication is removed together with `repository_analysis_publications`; `apply_fresh_source` only persists the immutable source revision (metadata and README evidence) and `evaluate_metadata_watches` queues one request per enabled watch of each owner.

### D2. Registration order for a track

`WatchBaseline::Unevaluated` stores no checkpoint. The route registers the watch before the shared function runs, so the evaluation that follows the metadata step finds the null checkpoint and queues the first request, with the README revision read back from the stored projection. Registration of an `Unevaluated` watch needs no metadata row. If the metadata step fails after registration the watch stays and the next observation queues the request.

### D3. One shared function for the route and the observer

`observe::record_fresh_repository` takes the already fetched fresh body, fetches the README through the same gateway and rate-limit ledger (conditionally, with the stored validator), stores the blob, applies the source revision, optionally registers an `Unevaluated` watch for the tracking user, and evaluates the watches. The observer calls it with unauthenticated access; the route calls it with the caller's credential.

### D4. Request deduplication follows the whole source revision

The unique key becomes `(watch_id, idempotency_digest_hex, requested_contract)`. The idempotency digest already covers owner, repository, attributes and README revision, so a README-only change queues a request and an identical revision still queues none.

### D5. The observer is selected in SQL and runs in the relay loop

A pass selects up to `BATCH` repositories with an enabled watch ordered by `repository_metadata.fetched_at` ascending, observes each through `observe_repository` with a shared `RateLimitLedger` (token label `watch-observer`), and a withheld budget ends the pass. The pass runs inside `relay_loop` when the interval has elapsed since the last pass, so the supervisor already stops the process when it fails. Interval 0 disables it.

### D6. Bounded provider calls

`ReqwestGithubApi::for_base_url` keeps the fixed 5 second connect timeout and the 30 second default total timeout; `with_timeout` rebuilds the client for the configured bound. Every request of the adapter goes through that client, so a stalled provider surfaces as `ProviderError::Transport`.

### D7. Revocation confirmation

`revoke_oauth_grant` returns success on 204. On 404 it sends `GET {base}/user` with the same access token and succeeds only on 401; any other status or a transport failure is a `ProviderError`, which `erase_account` already reports as `incomplete_external_grant_revocation`. GitHub's reference lists only 204 and 422 for the delete, so the probe, not the undocumented 404, is what makes the revocation verified.

### D8. Relay health

`flush_outbox` reports `Lifecycle::set_publisher_healthy(false)` when a pass attempted a publish, at least one failed, and the oldest unpublished publishable row has `attempt_count >= 3` or is older than 300 seconds; a pass that publishes every due row or finds none sets it healthy. A row whose serialized envelope exceeds the connected server's `max_payload` minus 1 KiB is marked `undeliverable_at` at once and never counts. The unhealthy state never stops the relay.

## Risks / Trade-offs

- [Removing the direct publication changes a specified capability] -> the capability is replaced by the watch path and its spec requirements are removed in this change; no consumer relied on the direct subject because no runtime caller existed.
- [A tracked repository whose metadata step failed keeps an unevaluated watch] -> the next observation queues the request; no analysis of old state is lost.
- [`apply_fresh_body` has no runtime caller after this change] -> it stays as the metadata-history primitive its tests cover; recommendation: remove it in a follow-up that rewrites those tests on top of `apply_fresh_source`.
