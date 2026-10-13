## Why

Changeset XR-021 round 6 (CONTRACTS.md R6-01) changes the Knowledge repository handler so that it waits out an operator-fixable provider failure (a rejected credential, an exhausted balance, a withdrawn model) and a spend-budget refusal, instead of settling a retryable `dependency_unavailable` fact after five deliveries. That is lossless only if this repository keeps the request visible and open for as long as Knowledge waits. R6-01 rule 4 asks for a check on this side: nothing here may age, fail or re-dispatch a `pending` request, and a terminal fact must resolve exactly one `pending` request.

## What Changes

- No production code change. The behaviour already holds: `dispatch_due_repository_analysis` selects only `queued` rows, `consume_terminal_fact` updates only `pending` rows that match the full identity, and there is no timeout column or sweep.
- New characterization tests in `crates/catalog/tests/watch_pending_open_ended.rs` pin it against real PostgreSQL: a dispatched request stays `pending` indefinitely, a later completion resolves it once, a metadata change during the wait queues a second request without touching the first, and a retryable `failed` fact is terminal until the metadata changes.
- `docs/DOMAIN.md` and `docs/ARCHITECTURE.md` state that a `pending` request has no deadline and no re-dispatch.

## Capabilities

No capability changes. The accepted `repository-watches` spec names no request timeout, so there is no spec delta; the behaviour pinned here is the one its "Knowledge terminal facts resolve only their matching pending request" requirement already describes.

## Impact

`crates/catalog/tests/watch_pending_open_ended.rs`, `docs/DOMAIN.md`, `docs/ARCHITECTURE.md`. Cross-repository behaviour is not involved here; the change cites XR-021 CONTRACTS.md R6-01 and does not restate it. The re-queue of a retryable `failed` request stays open as R5-15.
