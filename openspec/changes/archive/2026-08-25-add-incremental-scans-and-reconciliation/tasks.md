# Tasks: add safe incremental scans and scheduled reconciliation

Every behavior task is a pair: the first task adds a test that fails, the second makes it pass. Run each new test before writing implementation and confirm it fails for the stated assertion reason, not for a compile error or typo (a missing type stub may be added minimally to make the test compile). All commands run with `--locked`. DB-backed suites need the compose stack (`docker compose up -d --wait`).

## 1. Schema (no behavior pair possible)

- [x] 1.1 Edit `schema.sql` in place: add `github_catalog.star_watermarks(account_id uuid pk fk github_accounts, high_water_mark timestamptz not null, updated_at timestamptz not null default now())`; add `github_catalog.reconciliation_repairs(sync_run_id uuid fk sync_runs, repository_id uuid fk repositories, action text, recorded_at timestamptz default now(), pk (sync_run_id, repository_id))` with an action check over `('unstar_after_drift', 'restore_after_miss')`; add nullable `boundary_starred_at timestamptz` to `sync_checkpoints`. Update the table-list expectation and constraint assertions in `crates/catalog/tests/schema.rs`, verify with `cargo nextest run --locked -p ratatoskr-github-catalog --test schema`. Cannot start from a failing test: it edits a schema definition file consumed by disposable databases.

## 2. Provider gateway: newest-first starred listing

- [x] 2.1 RED: in `crates/catalog/tests/provider_http.rs`, write `newest_first_listing_requests_sort_created_direction_desc` - wiremock matches `/user/starred` requiring query params `sort=created` and `direction=desc` plus the `star+json` accept header, serves one page of items with rate headers; the new call must return the normalized items. Prediction: fails because the gateway has no newest-first listing method.
- [x] 2.2 GREEN: add `list_starred_newest_first(token, page)` to `GithubApi` and implement it on `ReqwestGithubApi`, sharing reply types and rate-header handling with the existing listing; the unordered `list_starred` and its wire expectations stay untouched.

## 3. Watermark-governed incremental window

- [x] 3.1 RED: in new `crates/catalog/tests/incremental_flow.rs`, write `incremental_scan_ingests_only_items_newer_than_watermark_and_advances_it` - seed account plus watermark T0; page 1 serves items at T3,T2 (> T0), page 2 serves an item at T1 (< T0) then exhaustion mocks; run the incremental scan. Expect: T3/T2 repositories ingested under identity and starred with their provider timestamps, T1's repository absent (never known), exactly one `sync_runs` row with mode `incremental` status `completed`, watermark row advanced to T2, and wiremock verification proving no request past page 2. Prediction: fails because no incremental entry point exists.
- [x] 3.2 GREEN: implement `crates/catalog/src/incremental.rs`: read watermark, create the incremental run, walk pages newest-first through the new gateway method, ingest while strictly newer, stop on proof of coverage, advance the watermark in the transaction that records the final page state; export the outcome type from the crate root. Whole-listing-newer-than-watermark exhaustion advances the watermark to the oldest ingested timestamp (asserted in the same test's fixture shape).

## 4. Incremental requires a full baseline

- [x] 4.1 RED: add `incremental_request_without_baseline_runs_full_snapshot_instead` - no watermark row seeded; the call reports a full-snapshot outcome, exactly one run row exists and its mode is `full`, and no incremental run row was created. Prediction: fails because the flow currently creates an incremental run regardless. Observed: passed on first run - the deferral branch shipped inside task 3.2's implementation because the no-baseline case is structurally inseparable from reading the mark; this test stands as the regression pin for the deferral contract.
- [x] 4.2 GREEN: with no persisted watermark, delegate to the full-snapshot path and surface its outcome; do not open an incremental run. Landed with 3.2 (see 4.1 observation); the pin holds.

## 5. Incremental never infers removals

- [x] 5.1 RED: add `incremental_scan_never_touches_repositories_outside_its_window` - prior authority stars old repository X (timestamp <= watermark) and newer repository Y; the scan sees only Y again plus newcomer Z. Expect X unchanged (still starred, original timestamps, evidence unchanged, no new observation rows for X), Y keeps its established starred-at despite a fresh provider value on the wire, Z becomes starred. Prediction: fails on whichever branch is missing - most concretely the stale-authority preservation assertions. Observed: passed on first run - upsert-only ingestion with timestamp coalesce shipped inside 3.2's window writer; kept as the invariant's regression pin.
- [x] 5.2 GREEN: make ingestion upsert-only: additions insert, continuations coalesce the established timestamp, and nothing outside the fetched window is read or written by the incremental path. Landed with 3.2 (see 5.1 observation); the pin holds.

## 6. Missing provider timestamp is a gap

- [x] 6.1 RED: add `missing_provider_starred_at_fails_run_as_gap_without_side_effects` - a listed item within the window carries `"starred_at": null`; expect a gap-marked outcome, the run failed with a reason naming the ordering gap, staging cleared, watermark untouched, prior authority unchanged, and no ingestion of any item from the offending page. Prediction: fails because the flow currently treats the null as ingestable data. Observed: passed on first run - pre-ingest validation shipped inside 3.2's ordering proof, since ingesting before validating would have made 3.1's own fixtures dishonest; this test pins the abort-without-side-effects contract.
- [x] 6.2 GREEN: detect the anomaly before ingesting the page, terminate the run as failed with the recorded reason in one transaction, clear staging, and return the gap-requiring-rescan outcome. Landed with 3.2 (see 6.1 observation); the pin holds.

## 7. Ordering gaps across resumed boundaries

- [x] 7.1 RED: add `out_of_order_resume_boundary_detects_gap` - pause the run after page 1 (budget refusal mid-scan), resume against a server whose next page leads with an item newer than page 1's oldest boundary; expect the resumed pass to end in the same gap outcome with reason recorded and nothing from the offending page ingested. Prediction: fails because resume does not carry an ordering boundary across processes. Observed: the first failure was my fixture's (`remaining: "4999"` never trips the reserve floor, so the scan ran on and hit a provider failure); after correcting the fixture to the real pause mechanism (`"0"`) the pinned behavior held - boundary persistence and its resume-time enforcement shipped inside 3.2, and without them the impossible page would be ingested instead of gapped. Kept as the cross-process ordering pin.
- [x] 7.2 GREEN: persist `boundary_starred_at` with every checkpoint, restore the monotonicity guard from the latest checkpoint on resume, and route violations through the same gap path. Landed with 3.2 (see 7.1 observation); the pin holds.

## 8. Completed snapshots re-anchor the watermark

- [x] 8.1 RED: add `completed_full_snapshot_sets_watermark_to_newest_observed` - run a full snapshot over known timestamps; after completion the account's watermark equals the newest observed provider starred-at. Prediction: fails because the swap transaction does not touch watermarks. Observed: failed as predicted (no watermark written); GREEN added `reanchor_watermark` to the swap transaction.
- [x] 8.2 GREEN: set the watermark from the enumeration inside the completion transaction; an empty enumeration leaves any prior watermark alone rather than inventing one (asserted alongside by `empty_completed_snapshot_leaves_the_watermark_unset`, whose first run exposed an aggregate-over-zero-rows NULL insert that the guard now filters).

## 9. Drift repairs recorded exactly once

- [x] 9.1 RED: in new `crates/catalog/tests/reconciliation_flow.rs`, write `completed_snapshot_records_drift_repairs_exactly_once` - prior state: A starred but absent from the fresh listing, B unstarred (with prior unstar evidence) but present again, C starred and present. Run one full snapshot: expect repair rows exactly `{(run, A, unstar_after_drift), (run, B, restore_after_miss)}` plus the normal evidenced effects for A and B. Immediately run a second full snapshot over identical fixtures: expect zero repairs, zero additions, zero unstars, and byte-identical current star state. Prediction: fails because no repair rows are written at all (first-phase assertion).
- [x] 9.2 GREEN: record the repair rows inside `apply_authority_and_complete`'s transaction - absence branch emits `unstar_after_drift` for locally-starred-absent ids, promote branch emits `restore_after_miss` for locally-unstarred-present ids - keyed `(sync_run_id, repository_id)` so repetition cannot duplicate; keep the function under the size lint by extracting helpers.

## 10. Sync command consumption: validation, inbox, dispatch

- [x] 10.1 RED: in new `crates/catalog/tests/sync_commands.rs`, write `valid_envelope_dispatches_incremental_scan_and_records_inbox` - build the platform command envelope JSON for `github.sync.requested.v1` (all eight members, tenant `user:<uuid>`, payload `{"account": "<owner_ref>"}`); handle it. Expect an incremental-mode run completed for that account, an `inbox_events` row keyed by the command identity with `consumed_at` set, and a report carrying the scan outcome. Prediction: fails because no command-handling entry point exists. Observed: validation, claim, and dispatch landed together before the first run of this suite, so this test passed once its own fixture was corrected (the builder omitted the `user:` prefix on `tenant_id`); it stands as the consumption-contract pin.
- [x] 10.2 GREEN: implement `crates/catalog/src/commands.rs`: envelope parsing/validation per design D7 (type equality, UUID identity, tenant shape, connected-account owner reference, optional mode vocabulary defaulting to incremental), inbox insert-then-dispatch, mode routing to the incremental or full flow, exported report type.

## 11. Duplicate redelivery is inert

- [x] 11.1 RED: add `duplicate_command_redelivery_performs_no_second_effect` - deliver the identical envelope twice; the second call reports a duplicate, total sync runs stay at one, total inbox rows stay at one, and star state is unchanged by the second delivery. Prediction: fails because the handler re-runs the scan for an already-known identity. Observed: passed after the tenant fixture fix - the claim-before-dispatch short-circuit shipped with 10.2; wiremock's one-fetch caps make any refetch fail loudly, so the inertness is genuinely pinned.
- [x] 11.2 GREEN: treat the inbox primary-key conflict on the command identity as a duplicate short-circuit taken before any dispatch. Landed with 10.2; the pin holds.

## 12. Rejection without side effects

- [x] 12.1 RED: add rejection tests covering, as separate focused functions from one fixture builder: wrong `command_type`, unparseable tenant, unknown account reference, disconnected account status, and payload mode outside the vocabulary; each expects a typed error naming the violation, zero inbox rows, zero sync runs. Observed: four variants passed immediately, but `disconnected_account_reference_is_rejected_without_side_effects` failed against real behavior - the handler resolved accounts without checking their status - and drove the fix; exactly what this pair exists for.
- [x] 12.2 GREEN: raise the typed validation errors before any write; every rejection path touches no table. The disconnected-account check (`status != 'connected'`) was added in response to 12.1's failure.

## 13. Commanded gaps chain into a forced full rescan

- [x] 13.1 RED: add `gap_during_commanded_incremental_chains_full_rescan` - fixtures force an ordering gap during the dispatched incremental scan; handling reports the failed incremental run with its gap reason and a completed full-mode run for the same account, and authority afterwards reflects the full snapshot. Prediction: fails because the handler surfaces the gap outcome without escalating. Observed: passed once fixtures were correct - the escalation shipped with 10.2's dispatch; kept as the convergence pin.
- [x] 13.2 GREEN: on a gap outcome from the commanded incremental, run the full-snapshot path within the same handling and carry both results in the report. Landed with 10.2; the pin holds.

## 14. Documentation and gate

- [x] 14.1 Update `README.md` (status note; incremental/reconciliation subsections; operator registration INSERT statements creating the frequent-incremental and periodic-full schedules disabled then enabled, citing platform's documented mechanism), `docs/DATA_MODEL.md`, `docs/ARCHITECTURE.md`, and `docs/INTERFACES.md` where synchronization is described as full-only or scheduling as absent (documentation task; cannot start from a failing test).
- [x] 14.2 Run the full gate from DEVELOPMENT.md (compose DB up): fetch/deny/fmt/clippy/build/test/test-doc/build-release plus the 850-line ratchet; fix findings until green. Observed: gate green end to end (`cargo deny` reports only informational lock-duplicate/unmatched-source warnings that its policy does not fail on).
- [x] 14.3 Tick every task above, run `openspec validate add-incremental-scans-and-reconciliation --strict`, archive the change, and verify `openspec validate --archived` stays clean.
