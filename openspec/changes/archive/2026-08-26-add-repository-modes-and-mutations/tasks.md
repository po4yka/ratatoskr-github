## 1. Schema foundation

- [x] 1.1 Failing test first: extend `crates/catalog/tests/schema.rs` with `mutation_audit_and_mode_columns_carry_the_decided_rules` asserting that `repositories.mode` carries the auto/tracked/ignored check, `github_accounts.granted_scopes` defaults to an empty array, and `github_catalog.mutation_audit` exists with its operation-kind, source, outcome checks and the partial unique index over successful idempotency keys - run it and confirm the assertion fails because the column, column default, and table do not exist yet (schema-definition task pair 1 of 2)
- [x] 1.2 Edit `schema.sql` in place: add `repositories.mode`, `github_accounts.granted_scopes`, and the `mutation_audit` table per design D7 until the new schema test passes (`cargo nextest run --locked -p ratatoskr-github-catalog --test schema`)

## 2. Authorization context

- [x] 2.1 Failing test first: in new `crates/catalog/tests/mutation_flow.rs` add `mutation_for_unconnected_account_is_refused_without_provider_call` calling the mutation executor entry point with a context whose account has no row; assert the outcome is refused-as-unauthorized, wiremock received zero requests, and one audit entry with outcome rejected exists - confirm it fails because the executor module does not exist yet (minimal type stubs allowed to reach a real assertion failure)
- [x] 2.2 Implement `crates/catalog/src/mutations.rs`: `MutationContext`, capability/scope requirement types, executor entry resolving account status and granted scopes before any provider contact, refusal auditing; make the 2.1 test pass
- [x] 2.3 Failing test first: add `star_without_required_scopes_is_refused_and_audited` - account row connected but empty granted scopes; assert refusal naming the missing capability, zero provider requests, audit entry recorded; confirm it fails on the outcome assertion
- [x] 2.4 Extend enforcement with the star capability check accepting any of `repo`/`public_repo`; make 2.3 pass

## 3. Idempotent star and unstar

- [x] 3.1 Failing test first: in `crates/catalog/src/provider_mutations.rs` unit tests pin the GraphQL wire shapes through `ReqwestGithubApi` against wiremock: `star_mutation_sends_documented_add_star_operation_and_reports_provider_confirmation` asserting request body matches the committed shape and response maps viewerHasStarred into applied/already-applied - confirm it fails because the trait method is not implemented
- [x] 3.2 Implement the `MutationApi` trait (`fetch_repository_node_id`, `star_repository`, `unstar_repository`) with legacy-proven GraphQL documents and rate-header capture; make 3.1 pass
- [x] 3.3 Failing test first: in `tests/mutation_flow.rs` add `successful_star_sets_local_star_state_and_records_one_audit_entry` - seeded unclassified repo, connected scoped account; assert provider called once, `current_star_state` starred with mode promoted to auto, audit entry applied; confirm it fails on the local-state assertion
- [x] 3.4 Implement the star execution path (node-id resolution, budgeted provider call, local star observation + projection write in one transaction, audit insert); make 3.3 pass
- [x] 3.5 Failing test first: add `retrying_completed_star_with_same_key_short_circuits_to_already_applied` - resubmit same idempotency key; assert provider still called exactly once total, outcome already-applied, exactly one successful audit entry for the key; confirm it fails (second call currently reaches the provider)
- [x] 3.6 Implement the stored-outcome replay fast path plus partial unique index conflict fallback (design D5); make 3.5 pass
- [x] 3.7 Failing test first: add `starring_already_starred_repository_reports_already_applied_without_touching_timestamps` - provider mock confirms viewerHasStarred true for pre-starred state; assert outcome already-applied and established starred_at unchanged; confirm it fails
- [x] 3.8 Implement already-held detection from provider confirmation; make 3.7 pass
- [x] 3.9 Failing test first: add `failed_star_attempt_does_not_consume_its_idempotency_key` - first attempt gets a 500-classified provider failure (outcome failed, local unchanged), retry succeeds; assert final end state starred and exactly one successful audit entry; confirm it fails on the retry behavior
- [x] 3.10 Wire failed-attempt handling (audited failure without key consumption) so 3.9 passes
- [x] 3.11 Failing test first: add `unstar_follows_the_same_idempotent_contract_as_star` covering removeStar wire shape, truthful outcomes, and replay; confirm it fails, then extend execution to unstar including auto-mode release to unclassified; make it pass

## 4. List membership mutations

- [x] 4.1 Failing test first: in `provider_mutations.rs` tests add `set_lists_mutation_sends_update_user_lists_for_item_with_complete_desired_set` pinning the wire shape and desired-set recording; confirm it fails, then implement the trait method; make it pass
- [x] 4.2 Failing test first: in `tests/mutation_flow.rs` add `adding_a_list_preserves_the_repositorys_other_live_lists` - wiremock serves live membership query returning two lists, then the set mutation; assert written listIds contain all three and audit detail records the desired set; confirm it fails on the computed-set assertion
- [x] 4.3 Implement read-modify-write membership add (live read under budget, desired = live + target, write, audit); make 4.2 pass
- [x] 4.4 Failing test first: add `removing_a_list_leaves_remaining_memberships_in_place` mirroring 4.2 for removal; confirm it fails, then extend to removal; make it pass
- [x] 4.5 Failing test first: add `list_write_requires_user_scope_and_is_refused_audited_otherwise` enforcing the `user` scope capability for both directions; confirm it fails, implement; make it pass

## 5. Batch partial success

- [x] 5.1 Failing test first: add `one_failing_operation_strands_nothing_in_a_batch` - three operations, second's provider call fails; assert ordered outcomes applied/failed/applied, first and third effects stand, three audit entries with distinct keys; confirm it fails because batch entry point does not exist
- [x] 5.2 Implement independent sequential execution returning per-operation outcomes in submission order; make 5.1 pass
- [x] 5.3 Failing test first: add `resubmitting_batch_retries_only_incomplete_operations` - resubmit after second op recovered; assert first/third come back already-applied with no additional provider calls for them and no duplicate successful audit entries; confirm it fails, then verify D5 replay covers it (implementation only if the test exposes a gap)

## 6. Mode transitions as audited operations

- [x] 6.1 Failing test first: in new `crates/catalog/tests/mode_flow.rs` add `explicit_track_request_records_transition_and_sets_tracked` - assert mode tracked and one audit entry carrying principal, source, from/to modes; confirm it fails because `set_repository_mode` does not exist
- [x] 6.2 Implement `crates/catalog/src/modes.rs`: transition validation matrix (design D2) with audited application; make 6.1 pass
- [x] 6.3 Failing test first: add `direct_auto_request_is_refused_without_side_effects` asserting refusal outcome, unchanged mode, no transition record; confirm it fails, then enforce the rule; make it pass
- [x] 6.4 Failing test first: add `ignoring_a_starred_repository_is_refused_without_state_change` and `starring_an_ignored_repository_is_refused_without_provider_call`; confirm they fail, then enforce both directions of the ignore/star conflict; make them pass
- [x] 6.5 Failing test first: add `re_requesting_current_mode_succeeds_as_no_op_with_single_record` and `retrying_mode_request_with_same_key_yields_one_record`; confirm they fail, then implement no-op confirmation semantics and key replay; make them pass
- [x] 6.6 Failing test first: in `tests/incremental_flow.rs` add `first_star_observation_promotes_unclassified_to_auto_but_never_overrides_explicit_modes` - incremental scan ingests a new starred repo seeded unclassified while a second seeded ignored repo also appears; assert first becomes auto, second stays ignored; confirm it fails, then hook promotion into the star upsert path; make it pass
- [x] 6.7 Failing test first: in `tests/reconciliation_flow.rs` add `snapshot_authority_respects_tracked_and_ignored_classifications`; confirm it fails if the swap path can clobber modes, adjust only if red
- [x] 6.8 Failing test first: add `evidenced_unstar_releases_auto_governance_to_unclassified_and_keeps_tracked` driving unstar evidence through the reconciliation drift-repair path; confirm it fails, then apply the release rule at the evidenced-unstar write; make it pass

## 7. Exports, docs alignment

- [x] 7.1 Export the new public surface from `crates/catalog/src/lib.rs` (context, outcomes, errors, entry points) and confirm the workspace compiles (`cargo build --workspace --locked`); no failing test possible first - compile success is the verification
- [x] 7.2 Align README.md status paragraph, DEVELOPMENT.md opening status sentence, and docs/DOMAIN.md mode section with implemented reality; documentation task - verified by review, no failing test applies

## 8. Gate and archive

- [x] 8.1 Run the full DEVELOPMENT.md gate list until green (fetch/deny/fmt/clippy/build/test/doc/release/file-size ratchet)
- [x] 8.2 Tick every task, `openspec validate --archived` clean after archiving the change
