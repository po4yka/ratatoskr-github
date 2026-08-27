## 1. Watch policy and durable request state

- [x] 1.1 RED: add `watch_registration_baselines_existing_metadata` in
  `crates/catalog/tests/watch_analysis_flow.rs`; it must fail because the Catalog cannot register a
  user-owned metadata-delta watch.
- [x] 1.2 GREEN: implement watch registration and current-schema checkpoint persistence.
- [x] 1.3 RED: add `metadata_delta_queues_and_dispatches_one_analysis_request`; it failed because
  a changed metadata revision creates no queued request/outbox payload.
- [x] 1.4 GREEN: construct the shared immutable request, deduplicate it, and pace dispatch through
  the Catalog cursor.

## 2. Terminal linkage

- [x] 2.1 RED: add `matching_completion_resolves_the_pending_request_once`; it failed because
  there is no completion projection.
- [x] 2.2 GREEN: consume matching completion/failure facts idempotently and retain only opaque result
  linkage.

## 3. Documentation and gate

- [x] 3.1 Update catalog interfaces, domain/data model, and implementation-plan status.
- [x] 3.2 Run the full `DEVELOPMENT.md` gate using PostgreSQL 17.
