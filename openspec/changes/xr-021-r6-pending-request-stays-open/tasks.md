## 1. A dispatched request stays pending indefinitely (R6-01 rule 4)

- [ ] 1.1 Characterization, passes at once: add `crates/catalog/tests/watch_pending_open_ended.rs::a_dispatched_request_stays_pending_indefinitely`; after one hour, 24 hours and 30 days the dispatch answers `NotDue`, the evaluation answers `Unchanged`, the request is `pending` with the same `outbox_message_id`, and the outbox holds one analysis message.
- [ ] 1.2 Characterization, passes at once: add `::a_later_completion_resolves_the_waiting_request_once`; the completion answers `Resolved`, then `Duplicate`, the status is `completed` and `repository_analysis_links` holds the result reference.

## 2. A metadata change during the wait (R6-01 rule 4)

- [ ] 2.1 Characterization, passes at once: add `::a_metadata_change_during_the_wait_queues_a_second_request`; a second `queued` request with a different idempotency digest appears, the first stays `pending`, a fact for the second does not resolve the first (`Ignored`) and the first still resolves with its own fact.

## 3. A retryable failed fact is terminal (R6-01 rule 4, R5-15)

- [ ] 3.1 Characterization, passes at once: add `::a_retryable_failed_fact_is_terminal_until_the_metadata_changes`; the request ends `failed` with `retryable = true`, the checkpoint equals the stored metadata hash, a following evaluation answers `Unchanged` and no new request exists.

## 4. Documentation

- [ ] 4.1 Add the no-deadline, no-re-dispatch sentence to `docs/DOMAIN.md` (repository analysis request) and `docs/ARCHITECTURE.md` (watch loop); this task has no failing test because it is documentation.

## 5. Close

- [ ] 5.1 Run the repository gate and `openspec validate --all --strict`, archive the change, and read the hosted CI of the pushed commit; this task has no failing test because it is verification.
