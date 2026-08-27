## 1. Account and credential foundation

- [x] 1.1 RED: add `crates/catalog/tests/credentials.rs::imported_account_requires_reauthorization_and_has_no_credential` asserting an imported account cannot connect or synchronize and that no credential row exists; run `build-gate -- cargo nextest run --locked -p ratatoskr-github-catalog --test credentials imported_account_requires_reauthorization_and_has_no_credential` and confirm the assertion fails because current schema/logic cannot represent the state.
- [x] 1.2 Edit the current `schema.sql` in place for provider-account binding, encrypted versioned credential storage, and import-safe unknown star/list evidence; implement the account persistence paths until task 1.1 passes.
- [x] 1.3 RED: add `crates/catalog/tests/credentials.rs::valid_pat_reconnects_only_the_matching_imported_account` with a synthetic authenticated-user response, asserting verified provider identity/scopes, encrypted storage, and one connected account; run it through `build-gate -- cargo nextest run --locked -p ratatoskr-github-catalog --test credentials valid_pat_reconnects_only_the_matching_imported_account` and confirm the expected assertion fails.
- [x] 1.4 Implement stdin-only PAT registration, authenticated provider verification, authenticated encryption/key versioning, and atomic account activation until task 1.3 passes.
- [x] 1.5 RED: add `credentials.rs::credential_key_debug_redacts_key_material` and `operator_commands.rs::legacy_commands_reject_secret_bearing_arguments_and_unapproved_activation`, asserting key/token material is absent from debug and command diagnostics; run both through `build-gate` and confirm their red assertions.
- [x] 1.6 Implement secret redaction, stable refusal errors, and non-serializable credential configuration until task 1.5 passes.

## 2. Safe legacy source and import

- [x] 2.1 RED: add `crates/catalog/tests/legacy_import.rs::source_preflight_accepts_only_the_archived_allow_list` using a synthetic PostgreSQL source schema containing `repositories`, `user_github_integrations`, and an encrypted-token column; run `build-gate -- cargo nextest run --locked -p ratatoskr-github-catalog --test legacy_import source_preflight_accepts_only_the_archived_allow_list` and confirm it fails because the source reader does not exist.
- [x] 2.2 Implement the temporary PostgreSQL source preflight and fixed allow-list queries, deliberately excluding credential columns, until task 2.1 passes; add only reviewed, exactly pinned dependencies if the existing graph cannot provide the required functionality.
- [x] 2.3 RED: extend `legacy_import.rs` with `unmapped_or_duplicate_owner_mapping_leaves_the_target_unchanged`, asserting all mappings validate before target writes; run the named test through `build-gate` and confirm its red assertion.
- [x] 2.4 Implement current-owner map parsing/validation and account transaction boundaries until task 2.3 passes.
- [x] 2.5 RED: add `legacy_import.rs::imports_repository_star_observation_and_list_claim_without_fabricating_provider_values`, asserting numeric identity/alias import, `last_synced_at` as observation time, unknown provider star time, and list-name claims rather than synthetic provider list IDs; run through `build-gate` and confirm the expected assertion fails.
- [x] 2.6 Implement account-scoped repository/star/list-claim import and the current-schema constraints until task 2.5 passes.
- [x] 2.7 RED: add `legacy_import.rs::repeating_the_same_fixture_import_is_idempotent`, asserting unchanged counts and no duplicate identity, observation, claim, credential, or policy rows after the second import; run through `build-gate` and confirm the expected assertion fails.
- [x] 2.8 Implement import-source keys/upserts and redacted run evidence until task 2.7 passes.

## 3. Shadow synchronization and reporting

- [x] 3.1 RED: add `crates/catalog/tests/legacy_shadow.rs::shadow_report_classifies_repository_star_and_list_differences` using the synthetic source and a reauthorized catalog account, asserting deterministic redacted mismatch categories and cutover ineligibility; run `build-gate -- cargo nextest run --locked -p ratatoskr-github-catalog --test legacy_shadow shadow_report_classifies_repository_star_and_list_differences` and confirm the expected assertion fails.
- [x] 3.2 Implement shadow execution over the normal complete synchronization path, comparison of imported evidence to provider-backed projections, and canonical JSON/text report rendering until task 3.1 passes.
- [x] 3.3 RED: add `legacy_shadow.rs::clean_post_reconnect_full_snapshot_is_cutover_reviewable`, asserting a clean report requires completed star/list snapshots, no unresolved identity or unknown provider star time, and contains no credential/source data; run through `build-gate` and confirm its red assertion.
- [x] 3.4 Implement clean-report readiness rules, report digest persistence, and redacted telemetry until task 3.3 passes.

## 4. Operator interface and cutover evidence

- [x] 4.1 RED: add `services/catalog/tests/operator_commands.rs::legacy_commands_reject_secret_bearing_arguments_and_unapproved_activation`, asserting source URLs and PATs are not CLI arguments and activation refuses absent/invalid owner approval; run `build-gate -- cargo nextest run --locked -p ratatoskr-github-catalog-service --test operator_commands legacy_commands_reject_secret_bearing_arguments_and_unapproved_activation` and confirm the expected assertion fails.
- [x] 4.2 Implement the bounded operator subcommands (`import-legacy`, `shadow-legacy`, and readiness validation) with stdin-only PAT intake, atomic report output, and stable exit statuses until task 4.1 passes.
- [x] 4.3 Write `docs/cutover/legacy-github-catalog.md` with the exact isolated-source setup, owner-map validation, reauthorization, repeated shadow criteria, read-then-write approval checkpoints, rollback sequence, and evidence retention; documentation has no meaningful RED, so verify links and ensure it never names a secret or embeds a real archive value.
- [x] 4.4 Add a test or static assertion for the checked-in synthetic fixture manifest proving it contains no credential fields/values, then run it through `build-gate` and confirm it passes.

## 5. Verification and owner execution

- [x] 5.1 Run `build-gate -- cargo fmt --all -- --check`, `build-gate -- cargo clippy --workspace --all-targets --locked -- -D warnings`, the full locked workspace tests/doc tests, release build, `cargo deny --locked check`, the Rust file-size ratchet, and `openspec validate import-legacy-data-and-cutover --strict`; record actual outcomes.
- [ ] 5.2 With the real archive restored only into an isolated read-only operator database, execute the documented import/shadow procedure and retain only redacted reports; this task cannot start with a RED because it is an external operator execution, and it is not complete until the owner reviews the report.
- [ ] 5.3 Obtain the owner's explicit approval bound to a clean report digest and executed gate, activate reads externally, observe the documented stability window, then obtain separate write approval; do not tick this task for a plan-only checklist.
- [ ] 5.4 If activation regresses, execute the documented rollback (disable catalog writes, restore prior reads, retain catalog evidence); otherwise record the externally observed cutover result before archiving the change.
