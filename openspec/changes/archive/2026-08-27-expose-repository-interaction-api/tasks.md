## 1. Contract Pin and Domain Listener

- [x] 1.1 Pin the merged `ratatoskr-contracts` SHA and update the lockfile; this dependency/configuration step cannot start from a failing behavior test, so verify `cargo metadata --locked --no-deps` resolves the immutable revision and no local path override exists
- [x] 1.2 Add `crates/catalog/tests/config.rs::domain_api_listener_is_loopback_and_provider_test_url_is_bounded`; predict and observe the assertion fail because the API listener/provider URL configuration does not yet exist or accepts a non-loopback listener
- [x] 1.3 Implement strict API listener/provider-base configuration and dual-listener startup/shutdown; verify the named test plus service boot tests pass and operator/domain ports remain distinct

## 2. Authenticated Read-Only Preview

- [x] 2.1 Add `services/catalog/tests/repository_api.rs::preview_returns_bounded_metadata_without_catalog_writes`; predict and observe failure because `/v1/gh/repositories/preview` is absent and the fake provider receives no request
- [x] 2.2 Implement Edge-header authentication, canonical URL parsing, owner-scoped account selection, provider-backed read-only preview, shared errors, and `/v1/capabilities`; verify the named test passes and database assertions prove zero mode/policy/mutation writes
- [x] 2.3 Add `preview_refuses_foreign_private_and_subresource_urls_before_disclosure`; predict and observe failure because foreign/sub-resource inputs are not yet safely collapsed/refused
- [x] 2.4 Implement the ownership/not-found and URL-shape guards; verify the named test and repository API test binary pass

## 3. Confirmation and Mode Gating

- [x] 3.1 Add `action_refuses_missing_confirmation_foreign_account_and_unsupported_mode_without_provider_calls`; predict and observe failure because the action route is absent or does not enforce all pre-provider gates
- [x] 3.2 Implement the action route's principal/target/confirmation/account/scope validation and safe domain outcomes; verify the named test passes with zero fake-provider calls for every refusal
- [x] 3.3 Add `metadata_and_track_never_call_provider_star`; predict and observe failure because the three-mode orchestrator does not yet exist
- [x] 3.4 Implement metadata apply and tracked-mode/desired-policy acceptance by composing Catalog primitives; verify the named test reports component truth and the fake provider records no star mutation

## 4. Star and Partial-Result Truth

- [x] 4.1 Add `provider_star_success_survives_later_persistence_failure_without_unstar`; predict and observe failure because current mutation errors erase the provider-confirmed component or cannot express a partial result
- [x] 4.2 Refine provider-star execution and add the repository-action orchestrator so provider confirmation, Catalog persistence, and desired-policy acceptance remain separate outcomes; verify the named test reports partial truth and zero unstar calls
- [x] 4.3 Add `provider_refusal_skips_dependent_policy_without_fabricated_success`; predict and observe failure because failed/refused/skipped component mapping is incomplete
- [x] 4.4 Implement exhaustive safe reason mapping and aggregate derivation through the shared contract; verify the named test and all mutation/mode tests pass

## 5. Idempotent Result Persistence

- [x] 5.1 Add `services/catalog/tests/repository_api.rs::exact_action_replay_returns_recorded_truth_and_conflicting_reuse_is_refused`; predict and observe failure because action-attempt persistence and request fingerprint checks are absent
- [x] 5.2 Edit the current `schema.sql` in place, implement owner-keyed action attempt/result persistence and replay, and add schema assertions; verify exact replay causes one effective star while conflicting key reuse is refused

## 6. Live API Gate

- [x] 6.1 Add `services/catalog/tests/live_repository_api.rs::real_service_serves_preview_and_partial_action_against_fake_provider`; this composition-only gate started green because the preceding RED/green listener, preview, and partial-action pairs had already completed the real surface before the shared harness was extracted
- [x] 6.2 Complete the disposable PostgreSQL/fake-provider process harness and service wiring; verify the test observes operator readiness, capabilities, preview, and star-success/backup-failure partial action over HTTP

## 7. Documentation, Gate, and Delivery

- [x] 7.1 Update README status, `docs/INTERFACES.md`, deployment/config examples, telemetry/privacy notes, and current-schema documentation; documentation cannot start from a failing behavior test, so verify all documented routes/ports/outcomes match executable tests and no future capability is claimed
- [x] 7.2 Run the exact fenced gate from `DEVELOPMENT.md` through `build-gate`, run `openspec validate expose-repository-interaction-api --type change --strict`, inspect `git diff --check` and the full diff, and rerun the live API smoke
- [x] 7.3 Fetch/rebase on current `origin/main`, rerun the full and live gates, commit only this change, integrate it into GitHub `main`, push `main`, and record the merged SHA and live command/API evidence for Telegram
