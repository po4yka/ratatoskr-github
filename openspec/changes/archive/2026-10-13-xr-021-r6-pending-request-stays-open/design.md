## Context

See proposal.md. The Knowledge side of XR-021 CONTRACTS.md R6-01 redelivers an `OperatorAction` or `BudgetExhausted` failure at every delivery and settles nothing, so from this repository the request is simply `pending` for as long as the outage lasts. `evaluate_metadata_watches` already advances `last_evaluated_content_hash` when it queues a request, so a request that ends `failed` is not re-queued until the metadata hash changes again.

## Decisions

### D1. Pin the behaviour, do not add code

A request timeout, a stale-pending detector or a re-queue sweep would each be a new state machine with a retry policy and a second terminal path. R6-01 decides that waiting is lossless and visible, so the right change here is a test that fails if anyone later adds an age-out, plus a documentation line. A stale-pending detector and the re-queue sweep for a retryable `failed` fact stay out of scope (R5-15).

### D2. Time is an explicit parameter

`dispatch_due_repository_analysis` and `evaluate_metadata_watches` take `now`, so the tests advance one hour, 24 hours and 30 days by passing `fixed_now()` plus an offset. No test reads the wall clock or sleeps.

### D3. Characterization tests

The tests describe behaviour that already holds, so none of them can be RED. Each task line says so. If one fails it is a defect of this repository and the writer stops and reports it instead of editing production code.
