## 1. A track action originates the first analysis request (R2-08 a and b)

- [x] 1.1 RED: add `crates/catalog/tests/watches.rs::an_unevaluated_baseline_queues_the_first_analysis`, `services/catalog/tests/repository_api.rs::track_queues_one_analysis_request_with_the_readme` and `services/catalog/tests/bus.rs::a_tracked_repository_publishes_one_requested_event`; they fail on the missing request, the missing README and the missing event (a signature-only `WatchBaseline` stub precedes them so the failure is an assertion).
- [x] 1.2 GREEN: add `WatchBaseline`, factor `observe::record_fresh_repository`, call it from `execute_catalog_action`, register an `Unevaluated` watch on `track`, remove the direct publication and `repository_analysis_publications`, and stop producing `not_preserved` at runtime.

## 2. Periodic observer for registered watches (R2-08 c)

- [x] 2.1 RED: add `services/catalog/tests/watch_observer.rs` and the watch configuration tests in `crates/catalog/tests/config.rs`; they fail because there is no observer and no key.
- [x] 2.2 GREEN: add `RATATOSKR__WATCH__{OBSERVE_INTERVAL_SECONDS,BATCH}`, the selection query and the observer pass in the supervised relay loop.

## 3. Bounded provider calls (R2-08 d)

- [x] 3.1 RED: add `provider_http.rs::a_stalled_provider_times_out`, `account_erasure_stalled.rs::erasure_with_a_stalled_revocation_is_bounded_and_incomplete` (a stalled revocation is a provider error, which erasure already reports as incomplete revocation after local erasure) and `config.rs::provider_timeout_defaults_and_bounds`; they fail because no timeout exists.
- [x] 3.2 GREEN: add the connect and total timeouts, `with_timeout`, `RATATOSKR__PROVIDER__TIMEOUT_MS`, and pass it from `main.rs`.

## 4. Revocation confirmation (R2-08 e)

- [x] 4.1 RED: add the five `revoke_*` cases in `account_erasure.rs`; `revoke_404_with_a_live_token_is_incomplete` and the probe-failure case fail because every 404 is success.
- [x] 4.2 GREEN: probe `GET /user` after a 404 and update the code comment and the README sentence.

## 5. Relay health reaches readiness (R2-03, R2-04 rule 2)

- [x] 5.1 RED: add `services/catalog/tests/bus_health.rs`; it fails because readiness stays 200 and an oversize row is retried forever.
- [x] 5.2 GREEN: add `Lifecycle::set_publisher_healthy`, the `bus_publish` check, `outbox_events.undeliverable_at` and the counters.

## 6. Documentation and operator example (R2-10)

- [x] 6.1 RED: add `services/catalog/tests/deployment_profile.rs::the_shipped_example_loads_and_enables_the_bus_and_the_reader_route`; it fails because the example is absent.
- [x] 6.2 GREEN: ship `deploy/systemd/github.conf.example` and rewrite README, DEVELOPMENT.md and AGENTS.md where they say watches or event publication do not exist.

## 7. Observer rotation (review fixes, R2-08 c)

- [x] 7.1 RED: add `watch_observer.rs::failing_repositories_do_not_starve_a_healthy_one` (batch repositories that answer 404 are older than a healthy changed one) and `the_first_pass_runs_shortly_after_boot_not_a_full_interval_later`; they fail because selection rotates only on success and the first pass waits a full interval.
- [x] 7.2 GREEN: add `repository_metadata.observe_attempted_at` (`schema.sql` in place), order the selection by it, record it after every attempt except a withheld budget, and start the first pass after at most five seconds.

## 8. Evaluation converges from stored state (review fix, R2-08 b and c)

- [x] 8.1 RED: add `watch_observer.rs::a_not_modified_pass_queues_the_request_a_crashed_evaluation_left_behind` (a source revision committed without its evaluation, then an observer pass answered with 304); it fails because the 304 path never evaluates watches.
- [x] 8.2 GREEN: `evaluate_metadata_watches` reads the attributes and README revision from the stored projection and its matching raw revision instead of a caller's body (a deliberate break of its signature; every call site is updated), and `observe_repository` calls it after a not-modified answer.
- [x] 8.3 RED: add `observe_flow.rs::the_readme_request_is_gated_by_the_budget_the_repository_response_reported`; it fails because the README request is made without `ledger.acquire`.
- [x] 8.4 GREEN: `record_fresh_repository` acquires before the README request and returns `ObserveError::RateLimited`; `observe_repository` reports it as `ObserveOutcome::RateLimited` and the action route as a provider failure.

## 9. One source-revision path (review fix, AGENTS.md no duplicate code paths)

- [x] 9.1 RED: add `schema.rs::a_metadata_projection_requires_its_readme_revision`; it fails because a projection without README evidence is accepted.
- [x] 9.2 GREEN: make `repository_metadata.readme_revision` not null (`schema.sql` in place), delete `apply_fresh_body` and the `NotPreserved` fallback in `watches.rs`, and rewrite the metadata, watch and bus tests on `apply_fresh_source` through `test_support::record_body`.

## 10. Final gate

- [ ] 10.1 Run the full local gate and `openspec validate --all --strict`.
