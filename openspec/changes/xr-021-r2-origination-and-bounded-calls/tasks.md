## 1. A track action originates the first analysis request (R2-08 a and b)

- [ ] 1.1 RED: add `crates/catalog/tests/watches.rs::an_unevaluated_baseline_queues_the_first_analysis`, `services/catalog/tests/repository_api.rs::track_queues_one_analysis_request_with_the_readme` and `services/catalog/tests/bus.rs::a_tracked_repository_publishes_one_requested_event`; they fail on the missing request, the missing README and the missing event (a signature-only `WatchBaseline` stub precedes them so the failure is an assertion).
- [ ] 1.2 GREEN: add `WatchBaseline`, factor `observe::record_fresh_repository`, call it from `execute_catalog_action`, register an `Unevaluated` watch on `track`, remove the direct publication and `repository_analysis_publications`, and stop producing `not_preserved` at runtime.

## 2. Periodic observer for registered watches (R2-08 c)

- [ ] 2.1 RED: add `services/catalog/tests/watch_observer.rs` and the watch configuration tests in `crates/catalog/tests/config.rs`; they fail because there is no observer and no key.
- [ ] 2.2 GREEN: add `RATATOSKR__WATCH__{OBSERVE_INTERVAL_SECONDS,BATCH}`, the selection query and the observer pass in the supervised relay loop.

## 3. Bounded provider calls (R2-08 d)

- [ ] 3.1 RED: add `provider_http.rs::a_stalled_provider_times_out`, `account_erasure.rs::erasure_with_a_stalled_revocation_fails_without_deleting` and `config.rs::provider_timeout_defaults_and_bounds`; they fail because no timeout exists.
- [ ] 3.2 GREEN: add the connect and total timeouts, `with_timeout`, `RATATOSKR__PROVIDER__TIMEOUT_MS`, and pass it from `main.rs`.

## 4. Revocation confirmation (R2-08 e)

- [ ] 4.1 RED: add the five `revoke_*` cases in `account_erasure.rs`; `revoke_404_with_a_live_token_is_incomplete` and the probe-failure case fail because every 404 is success.
- [ ] 4.2 GREEN: probe `GET /user` after a 404 and update the code comment and the README sentence.

## 5. Relay health reaches readiness (R2-03, R2-04 rule 2)

- [ ] 5.1 RED: add `services/catalog/tests/bus_health.rs`; it fails because readiness stays 200 and an oversize row is retried forever.
- [ ] 5.2 GREEN: add `Lifecycle::set_publisher_healthy`, the `bus_publish` check, `outbox_events.undeliverable_at` and the counters.

## 6. Documentation and operator example (R2-10)

- [ ] 6.1 RED: add `services/catalog/tests/deployment_profile.rs::the_shipped_example_loads_and_enables_the_bus_and_the_reader_route`; it fails because the example is absent.
- [ ] 6.2 GREEN: ship `deploy/systemd/github.conf.example` and rewrite README, DEVELOPMENT.md and AGENTS.md where they say watches or event publication do not exist.

## 7. Final gate

- [ ] 7.1 Run the full local gate and `openspec validate --all --strict`.
