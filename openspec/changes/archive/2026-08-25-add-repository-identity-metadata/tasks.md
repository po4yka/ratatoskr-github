# Tasks: add repository identity, mutable aliases, metadata, and conditional requests

Every behavior task is a pair: the first task adds a test that fails, the second makes it pass. Run each new test before writing implementation and confirm it fails for the stated assertion reason, not for a compile error or typo (a missing type stub may be added minimally to make the test compile). All commands run with `--locked`.

## 1. Schema and dependencies (no behavior pairs possible)

- [x] 1.1 Edit `schema.sql` in place: add `status`/`redirect_to` to `github_catalog.repository_aliases`, replace the alias uniqueness constraint with a partial unique index on `(alias_kind, alias_value) where status = 'active'`, and add `github_catalog.repository_metadata` and `github_catalog.repository_metadata_revisions`. Cannot start from a failing test: it edits a schema definition file consumed by disposable databases. Update the table-list expectation in `crates/catalog/tests/schema.rs` (`owned_schema_applies_twice_without_cross_schema_objects`) to include both new tables, and extend `placeholder_tables_carry_the_decided_identity_rules` with a duplicate-active-alias rejection assertion; verify via `cargo nextest run --locked -p ratatoskr-github-catalog --test schema`.
- [x] 1.2 Add pinned workspace dependencies (`reqwest` rustls+json no-default-features to `crates/catalog`, `wiremock` to its dev-dependencies) and verify with `cargo build --workspace --locked`.

## 2. Repository identity and aliases

- [x] 2.1 RED: in new `crates/catalog/tests/identity.rs`, write `upsert_repository_creates_one_record_per_provider_id` — upserting provider ID 300000001 must insert one row and return an internal id different from the provider id. Prediction: fails because `identity::upsert_repository` does not exist / returns nothing.
- [x] 2.2 GREEN: implement `crates/catalog/src/identity.rs` `upsert_repository` (insert-on-conflict-do-nothing + select) so the test passes.
- [x] 2.3 RED: add `upsert_repository_reuses_identity_for_known_provider_id` — second upsert of the same provider ID must return the identical internal id and leave exactly one repositories row. Prediction: fails on the row-count/id-equality assertion once 2.2 exists.
- [x] 2.4 GREEN: make identity reuse pass (no-op if already satisfied by 2.2's conflict handling; then the test pins the behavior).
- [x] 2.5 RED: add `resolve_alias_finds_recorded_owner_name_and_unknown_resolves_to_nothing` — after recording alias `acme/widgets`, resolving `owner_name` = that value returns the repository id, and resolving an unrecorded value returns none. Prediction: fails because recording/resolution are unimplemented.
- [x] 2.6 GREEN: implement `record_alias` and `resolve_alias` in `identity.rs`.
- [x] 2.7 RED: add `rename_evidence_redirects_old_alias_to_same_repository` — apply rename evidence `acme/widgets` → `acme/gadgets`; assert new alias resolves, old alias still resolves, and only one repositories row exists for the provider ID. Prediction: fails on old-alias resolution.
- [x] 2.8 GREEN: implement `apply_alias_observation` rename transaction (new active alias, old row superseded with `redirect_to`).
- [x] 2.9 RED: add `transfer_keeps_single_identity_across_owners` — same provider ID observed under `old-owner/name` then `new-owner/name`: one repositories row, both aliases resolve. Prediction: fails on the single-row assertion.
- [x] 2.10 GREEN: make transfer reuse the rename path keyed by provider ID (extend code only as the test requires).
- [x] 2.11 RED: add `live_owner_name_is_globally_unique` — a second repository claiming the currently active alias must be rejected while neither identity changes. Prediction: fails because the claim currently succeeds or corrupts state.
- [x] 2.12 GREEN: enforce rejection of conflicting live claims in `record_alias`.
- [x] 2.13 RED: add `released_name_may_be_claimed_while_history_still_redirects` — after repo A renames away from a name, repo B claims it; B holds it actively, A still resolves through its superseded row. Prediction: fails on A resolution or B claim.
- [x] 2.14 GREEN: make collision-safe history pass.

## 3. Provider gateway

- [x] 3.1 RED: new `crates/catalog/tests/provider_http.rs`, `conditional_request_sends_if_none_match_and_short_circuits_on_304` via wiremock - first request returns 200 with an ETag; the second request must carry `if-none-match`, receive 304, and report a not-modified outcome without a payload. Cannot be a unit test with a hand-written fake: a fake restates itself, so status/header mapping is tested across the HTTP boundary instead. Prediction: fails because the gateway cannot yet target an arbitrary base URL or send validators.
- [x] 3.2 GREEN: define `GithubApi` trait, outcome and payload types, and `ReqwestGithubApi` with redirects disabled and injectable base URL: sends `If-None-Match`, maps 200 to a fresh payload plus ETag and 304 to not-modified.
- [x] 3.3 RED: wiremock test `moved_permanently_reports_new_location` - a 301 with `Location` naming `/repos/new-owner/new-name` yields moved evidence naming `new-owner/new-name`, with no follow-up request hitting the server. Prediction: fails on the missing moved outcome.
- [x] 3.4 GREEN: parse the `Location` header into owner/name moved evidence.
- [x] 3.5 RED: wiremock test `mismatched_full_name_reports_rename_evidence_with_payload` - a 200 body whose `full_name` differs from the requested path yields the payload plus rename evidence. Prediction: fails on the missing evidence field.
- [x] 3.6 GREEN: compare the requested alias against body `full_name` and surface evidence.
- [x] 3.7 RED: golden contract test `recorded_fixture_parses_to_expected_projection` in `crates/catalog/tests/fixtures_contract.rs` reading committed `tests/fixtures/repos/widget.json` and asserting normalized fields exactly. Prediction: fails with missing fixture/parser mismatch message.
- [x] 3.8 GREEN: commit synthetic fixture(s) and implement normalization into the shared projection struct.

## 4. Rate-limit budget

- [x] 4.1 RED: unit tests in new `crates/catalog/src/rate_limit.rs` — `budget_refuses_requests_at_reserve_until_reset` (seeded bucket at floor refuses with retry_at in the future; bucket above floor proceeds) and `budget_is_shared_across_operations` (observe() through one ledger handle depletes what another handle sees). Prediction: fails because the ledger does not exist.
- [x] 4.2 GREEN: implement `RateLimitLedger`, `TokenRef`, `acquire`, `observe` with reserve floor and reset handling.
- [x] 4.3 RED: add `retry_after_sets_cooldown_before_numeric_reset` — response carrying `Retry-After` blocks acquisition until cooldown passes even with allowance remaining. Prediction: fails on refusal assertion.
- [x] 4.4 GREEN: ingest `Retry-After` into cooldown state.

## 5. Metadata projection and revisions

- [x] 5.1 RED: in new `crates/catalog/tests/metadata.rs`, `first_metadata_observation_creates_projection_and_revision` — observing a fresh body inserts projection fields verbatim and exactly one revision row. Prediction: fails because persistence functions do not exist.
- [x] 5.2 GREEN: implement `crates/catalog/src/metadata.rs` `apply_fresh_body` (projection upsert + revision append + hash).
- [x] 5.3 RED: add `not_modified_preserves_previous_metadata` — after a fresh observation, applying a not-modified outcome changes no projection value and appends no revision. Prediction: fails on revision-count assertion.
- [x] 5.4 GREEN: implement the cheap not-modified path (touch fetched bookkeeping only).
- [x] 5.5 RED: add `changed_metadata_updates_projection_and_appends_one_revision` — changed stargazer count updates projection and grows history by exactly one. Prediction: fails on projection/history assertions.
- [x] 5.6 GREEN: implement changed-body application keyed on content hash.
- [x] 5.7 RED: add `unchanged_payload_does_not_append_revision` — re-applying an identical body leaves the revision count unchanged. Prediction: fails because a duplicate revision is appended.
- [x] 5.8 GREEN: skip revision append when hash matches current.
- [x] 5.9 RED: add `revision_history_is_bounded_to_ten_most_recent` — after eleven distinct bodies, exactly ten revisions remain ordered oldest-to-newest with the newest matching the last applied body. Prediction: fails with eleven rows retained.
- [x] 5.10 GREEN: prune beyond the bound inside the append transaction.

## 6. Observe flow wiring

- [x] 6.1 RED: integration test `observe_repository_end_to_end_via_wiremock` in `crates/catalog/tests/observe_flow.rs` — fresh 200 creates identity+alias+projection+revision; immediate second observe sends `If-None-Match`, receives 304, and ends `NotModified` with unchanged revision count; rate headers from both responses land in the shared ledger (third acquire sees reduced remaining). Prediction: fails because `observe.rs` does not exist.
- [x] 6.2 GREEN: implement `observe_repository` composing ledger acquire, conditional fetch, rename refetch when moved, identity upsert, and metadata application; structured outcome returned.

## 7. Gate and documentation

- [x] 7.1 Update `README.md` status note and `docs/DATA_MODEL.md`/`docs/ARCHITECTURE.md` wording where they call these areas "planned" (documentation task; cannot start from a failing test).
- [x] 7.2 Run the full gate from DEVELOPMENT.md (compose DB up): fetch/deny/fmt/clippy/build/test/test-doc/build-release plus the 850-line ratchet; fix findings until green.
- [x] 7.3 Tick every task above, run `openspec validate add-repository-identity-metadata --strict`, and verify archived-change readiness (`openspec validate --archived` stays clean after archive).
