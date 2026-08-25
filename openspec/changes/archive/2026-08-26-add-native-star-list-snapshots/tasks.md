# Tasks: add-native-star-list-snapshots

Every behavior task is a pair: the first adds a test that fails, the second makes it pass. Run each new test before implementation and confirm it fails for the stated assertion reason, not a compile error or typo (a minimal type stub may be added so the test compiles). All commands run with `--locked`; DB suites need the compose stack (`docker compose up -d --wait`). GraphQL shapes below follow the live dotcom schema (`docs.github.com/public/fpt/schema.docs.graphql`, fetched 2026-08-25): `User.lists -> UserListConnection`, `UserList{id,name,...}`, `UserListItemsEdge{cursor,node}` with no item timestamps.

## 1. Schema (no behavior pair possible)

- [x] 1.1 Edit `schema.sql` in place: extend `sync_runs.mode` check with `'star_lists'` and add `lists_observed integer not null default 0`, `removals integer not null default 0`; add nullable `graphql_cursor text` to `sync_checkpoints`; add `status text not null default 'active'` (check `'active'`,`'removed'`) and `observed_removed_at timestamptz` plus `evidence_run_id uuid references sync_runs` to `star_lists`; redefine `star_list_memberships` as the current membership projection (`member boolean not null`, `last_observed_at timestamptz not null`, `observed_removed_at timestamptz`, `evidence_run_id uuid references sync_runs`, keep pk, check `(member = true) or (observed_removed_at is not null)`); add flat staging `github_catalog.list_snapshot_items(sync_run_id uuid fk sync_runs, position bigint, provider_list_id text, list_name text, provider_repository_id bigint, primary key (sync_run_id, position))`; add append-only `github_catalog.star_list_membership_observations(observation_id uuid primary key, list_id uuid fk star_lists, repository_id uuid fk repositories, member boolean not null, observed_at timestamptz not null, evidence_run_id uuid references sync_runs)`. Update the table-list expectation and constraint assertions in `crates/catalog/tests/schema.rs`, verify with `cargo nextest run --locked -p ratatoskr-github-catalog --test schema`. Cannot start from a failing test: it edits a schema definition file consumed by disposable databases.

## 2. Provider gateway: GraphQL star-list page

- [x] 2.1 RED: commit synthetic fixture JSON under `crates/catalog/tests/fixtures/lists/user_lists_page.json` shaped like a real GraphQL response (`data.viewer.lists { totalCount pageInfo{hasNextPage endCursor} edges{node{id name slug} items{totalCount pageInfo edges{node{...on Repository{id databaseId nameWithOwner}}}}} } data.rateLimit{cost remaining resetAt}`); in `crates/catalog/tests/provider_http.rs` write `starred_lists_page_posts_graphql_query_and_normalizes_reply` - wiremock matches `POST /graphql` carrying the bearer header, serves the fixture; assert the new gateway call returns normalized list summaries (provider id, name), their repository items (numeric ids), the continuation token, and rate data mapped onto the internal rate-limit shape. Prediction: fails because the trait has no list-enumeration method.
- [x] 2.2 GREEN: add `list_user_lists(token: Option<&str>, cursor: Option<&str>) -> Result<UserListsReply, ProviderError>` to `GithubApi` and implement it on `ReqwestGithubApi` posting the fixed GraphQL document to `{base}/graphql`; all wire types stay in `provider.rs`; GraphQL envelope errors map through existing provider error classification; rate normalization reuses the ledger shape.

## 3. Enumeration and staging walk

- [x] 3.1 RED: in new `crates/catalog/tests/list_snapshot_flow.rs`, write `list_snapshot_enumerates_all_pages_stages_memberships_and_completes_run` - account seeded; page 1 serves two lists (one renamed, one new) holding three distinct repositories total via the fixture-shaped mocks keyed by cursor absence/presence; page 2 serves an empty final page. Expect: repositories exist under identity, exactly one `sync_runs` row with mode `star_lists` status `completed`, staging cleared, run statistics recording pages processed, memberships observed, lists observed, additions, removals. Prediction: fails because no list-snapshot entry point exists.
- [x] 3.2 GREEN: implement `crates/catalog/src/star_lists.rs`: open-or-resume a `star_lists`-mode run, acquire budget per page, fetch through the new gateway method, upsert listed repositories through `upsert_repository` before staging, stage rows into `list_snapshot_items` and persist the `graphql_cursor` checkpoint per page in one transaction, stop at exhaustion and complete the run row; export the outcome type from the crate root.

## 4. Atomic swap records membership diffs as evidenced observations

- [x] 4.1 RED: write `completed_swap_applies_diff_records_observations_and_repeats_inertly` - prior authority: list L1 containing repos A,B (A also in L2); fresh enumeration: L1 contains B,C; L2 unchanged with A. Run one snapshot; expect inside promoted state: A demoted (`member=false`) with non-null `observed_removed_at` and completing-run evidence, C added (`member=true`), B continuing; L1's name updated when renamed; observation rows exactly covering every seen membership plus A's removal bound to the run. Immediately rerun over identical fixtures: zero additions, zero removals, byte-identical projections, only fresh confirmation observation rows. Prediction: fails because nothing promotes staging yet.
- [x] 4.2 GREEN: implement `apply_list_authority_and_complete` in one transaction: count statistics; upsert `star_lists` identity/name from staging; insert completion observations; promote staged pairs clearing `observed_removed_at` and setting evidence; demote locally-member-but-absent pairs with inferred removal time and evidence; clear staging; complete the run row. Extract helpers to stay under size lints.

## 5. Listed unstarred repositories stay truthful members

- [x] 5.1 RED: add `listed_unstarred_repository_remains_member_without_touching_star_state` - repo X holds explicit unstarred state with unstar evidence, repo Y has no star rows at all; both are enumerated inside list L. Expect both become and remain truthful members of L, X keeps its exact prior star state and evidence, Y gains no `current_star_state` row, and no `star_observations` rows appear for either. Prediction: fails or passes only accidentally before the swap exists - run after 4.2 lands green; if already passing, record why and keep as the invariant pin.
- [x] 5.2 GREEN: guarantee the swap reads no star tables and writes none; adjust only if 5.1 found a violation.

## 6. Removed lists are tombstoned with evidence

- [x] 6.1 RED: add `removed_list_tombstones_with_evidence_and_demotes_its_memberships` - prior authority has lists L1 (members A,B) and L2; fresh enumeration contains only L2. Expect L1 kept with status `removed`, non-null `observed_removed_at`, completing-run evidence, and all its memberships demoted with the same evidence; L2 and its members untouched; no rows deleted. Prediction: fails because the swap leaves unknown lists untouched.
- [x] 6.2 GREEN: extend the swap transaction with the tombstone branch updating absent lists and demoting their memberships in the same transaction.

## 7. Truncated membership refuses authority

- [x] 7.1 RED: add `truncated_list_fails_run_naming_it_without_side_effects` - a served list reports items `pageInfo.hasNextPage=true` beyond the inline cap. Expect outcome marking truncation, run failed with reason naming the truncated list, staging cleared, prior list authority unchanged, no observation rows written. Prediction: fails because the walk ignores inner pagination state.
- [x] 7.2 GREEN: detect inner `hasNextPage` during the walk and route through the fail path with the recorded reason before any further page is requested.

## 8. Failure preserves prior list authority

- [x] 8.1 RED: add `mid_scan_provider_failure_preserves_prior_list_authority` - prior authority seeded; page 2 answers HTTP 500. Expect run failed with reason, prior lists/memberships/observations unchanged, staging cleared. Prediction: fails because no list run exists to fail.
- [x] 8.2 GREEN: wire permanent provider failures through the shared fail-run path clearing staging within the same transaction.

## 9. Budget pauses and cursor-based resume

- [x] 9.1 RED: add `budget_refusal_pauses_then_resume_continues_from_recorded_cursor` - page 1 succeeds, next acquisition refuses (exhausted remaining headers); outcome Paused with the run left open and checkpoint recorded; restart against a fresh server whose first served page is page 2; wiremock request verification proves page 1 was never re-fetched and the scan completes. Prediction: fails because resume does not read `graphql_cursor`.
- [x] 9.2 GREEN: restore the continuation token from the latest checkpoint on resume and pass it to the gateway until exhaustion.

## 10. Read surface for current lists and members

- [x] 10.1 RED: add `read_surface_reports_active_lists_and_current_members_only` - after swaps leaving L1 active with members B,C and L2 tombstoned: `current_star_lists` returns exactly L1 with its current name, `current_list_members(L1)` returns exactly B,C, demoted A excluded, tombstoned L2 absent from lists, another account sees nothing. Prediction: fails because no read functions exist.
- [x] 10.2 GREEN: implement `current_star_lists(database, account_id)` and `current_list_members(database, list_id)` in `star_lists.rs` reflecting only promoted authority; export types from the crate root like other flows.

## 11. Commanded sync refreshes lists independently

- [x] 11.1 RED: in `crates/catalog/tests/sync_commands.rs`, add `commanded_full_sync_chains_independent_list_snapshot` - valid full-mode command with working star pages and working list pages: handling reports the completed star run AND a separate completed `star_lists` run. Then `list_failure_never_invalidates_star_outcome` - same command with `/graphql` answering 500 while star pages serve: report carries the successful star outcome unchanged alongside the failed list outcome, star authority rows remain exactly as written by the star run. Prediction: fails because handling performs no list work.
- [x] 11.2 GREEN: after the star-mode dispatch in `commands.rs`, attempt `run_star_list_snapshot` for the same account and carry both outcomes in the report type; neither result alters the other's rows.

## 12. Documentation and gate

- [x] 12.1 Update `README.md` (status paragraph; native star lists section documenting the consistency rules: star and list authorities are independent dimensions - starred-but-unlisted, listed-but-unstarred, and every combination are truthful; GraphQL-only access; truncation rule; tombstones), `docs/DATA_MODEL.md` (new/edited tables), `docs/ARCHITECTURE.md` (list reconciliation semantics), and `DEVELOPMENT.md` (implemented-status sentence). Documentation task; cannot start from a failing test.
- [x] 12.2 Run the full gate from DEVELOPMENT.md (compose DB up): fetch/deny/fmt/clippy/build/test/test-doc/build-release plus the 850-line ratchet; fix findings until green.
- [x] 12.3 Tick every task above, run `openspec validate add-native-star-list-snapshots --strict`, archive the change, and verify `openspec validate --archived` stays clean.
