# Tasks: add full star snapshot with atomic authority and checkpoints

Every behavior task is a pair: the first task adds a test that fails, the second makes it pass. Run each new test before writing implementation and confirm it fails for the stated assertion reason, not for a compile error or typo (a missing type stub may be added minimally to make the test compile). All commands run with `--locked`.

## 1. Schema (no behavior pair possible)

- [x] 1.1 Edit `schema.sql` in place: add `account_id` (FK to `github_accounts`), statistics columns (`pages_processed`, `items_observed`, `additions`, `unstars`), and nullable `failure_reason` to `github_catalog.sync_runs`; add `next_page bigint` to `sync_checkpoints`; add `starred_at timestamptz` to `current_star_state` with a presence check mirroring the removal-evidence check (`starred = false or starred_at is not null`); add the per-run staging table `snapshot_items(sync_run_id, position, provider_repository_id, provider_starred_at)`. Cannot start from a failing test: it edits a schema definition file consumed by disposable databases. Update the table-list expectation in `crates/catalog/tests/schema.rs` to include `snapshot_items`, extend constraint assertions with the new starred-at presence check, and verify via `cargo nextest run --locked -p ratatoskr-github-catalog --test schema`.

## 2. Provider gateway: paginated starred listing

- [x] 2.1 RED: in `crates/catalog/tests/provider_http.rs`, write `starred_listing_serves_pages_with_rate_headers_and_starred_at` - wiremock serves `/user/starred?page=1` and `?page=2` as `application/json` bodies of `[{starred_at, repo:{id, full_name, ...}}]` items; the new gateway call must request pages in ascending order carrying the `star+json` accept header and return normalized items (provider id, owner/name, starred-at) plus rate-limit headers per page. Prediction: fails because the gateway has no starred-listing method.
- [x] 2.2 GREEN: extend `GithubApi` with the paginated listing call and implement it on `ReqwestGithubApi`; response types stay inside the adapter; empty page terminates.

## 3. Enumeration, staging, checkpoints, run accounting

- [x] 3.1 RED: in new `crates/catalog/tests/snapshot_flow.rs`, write `full_snapshot_enumerates_all_pages_records_completed_run_and_statistics` - two non-empty pages plus an empty page against a disposable database; after the call every listed repository exists under stable identity, exactly one `sync_runs` row exists with mode `full`, status `completed`, finish time set, and statistics matching pages processed/items observed; one checkpoint row exists per processed page. Prediction: fails because no snapshot entry point exists.
- [x] 3.2 GREEN: implement the scan flow (`crates/catalog/src/snapshot.rs`): create the run, fetch pages ascending through the gateway, upsert identity, append `snapshot_items` + checkpoint per page in one transaction, complete the run with statistics. Authority tables stay untouched at this stage; the completion transaction records the outcome only.

## 4. Pause and resume

- [x] 4.1 RED: add `budget_refusal_pauses_run_without_touching_authority` - seed the ledger so acquisition is refused partway through; the call returns a paused outcome naming a retry time, the run row remains `running` with no finish time, prior `current_star_state` rows are unchanged, and already-processed pages have their checkpoints. Prediction: fails because the flow treats refusal as an error or keeps scanning. Observed: the first run kept scanning (fixture reset epoch was in the past, so the floor never tripped); after correcting the fixture to a future reset the pause gate held - the gate shipped inside the enumeration cycle, and this test pins it.
- [x] 4.2 GREEN: treat acquisition refusal as a pause outcome that persists nothing beyond what earlier pages durably recorded.
- [x] 4.3 RED: add `interrupted_scan_resumes_from_checkpoint_without_refetching_completed_pages` - pause a run after some pages (wiremock mocks use `up_to_n_times(1)` so any refetch fails verification), then resume; the provider receives only the remaining pages, each page lands exactly once across both calls, and the same run row reaches `completed`. Prediction: fails because resume restarts from page 1 or creates a second run.
- [x] 4.4 GREEN: resume attaches to the newest `running` full run for the account and continues from its latest checkpoint's next page.

## 5. Atomic authority swap

- [x] 5.1 RED: add `authority_swaps_atomically_and_readers_never_see_partial_snapshots` - establish prior authority (repository A starred via direct seed), run a snapshot whose listing contains A and B, and assert mid-run visibility by pausing after page one (A still the only authority row), then completing and asserting A and B appear together as one consistent state. Prediction: fails because completion does not yet promote snapshot results into star authority.
- [x] 5.2 GREEN: implement the swap transaction: additions become starred with their provider starred-at, continuations keep the established starred-at, evidence references the completing run, and the run completes inside the same transaction.
- [x] 5.3 RED: add `absent_repositories_become_evidenced_unstar_observations` - repository C starred under prior authority is missing from a completed snapshot; C's current state becomes unstarred with a non-null unstar observation time, the completing run id as evidence, and one append-only unstar observation row; no row is deleted. Prediction: fails on the unstar branch.
- [x] 5.4 GREEN: implement the absence branch of the swap.
- [x] 5.5 RED: add `continuing_stars_keep_their_established_starred_at` - two consecutive completed snapshots report different provider starred-at values for the same repository; the stored value stays the first one while observations keep both facts. Prediction: fails because the swap overwrites with the newest value.
- [x] 5.6 GREEN: apply the coalesce rule so the earliest established value survives confirmations and a re-star takes the fresh provider value.

## 6. Failure semantics

- [x] 6.1 RED: add `mid_run_provider_failure_preserves_prior_authority_and_records_failure` - a page request fails permanently partway through a scan over existing authority; the call reports failure, the run row is `failed` with finish time and reason, prior authority is unchanged including no new unstars, and staging rows for the dead run are cleared. Prediction: fails because failures currently propagate as errors without terminal run accounting.
- [x] 6.2 GREEN: classify permanent page failures, terminate the run as failed with its reason in one transaction, and leave authority untouched.

## 7. Gate and documentation

- [x] 7.1 Update `README.md` status note plus `docs/DATA_MODEL.md`, `docs/ARCHITECTURE.md`, and `DEVELOPMENT.md` wording where star synchronization is called planned (documentation task; cannot start from a failing test).
- [x] 7.2 Run the full gate from DEVELOPMENT.md (compose DB up): fetch/deny/fmt/clippy/build/test/test-doc/build-release plus the 850-line ratchet; fix findings until green.
- [x] 7.3 Tick every task above, run `openspec validate add-full-star-snapshot --strict`, and verify archived-change readiness (`openspec validate --archived` stays clean after archive).
