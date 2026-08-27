## 1. Contract and immutable source boundary

- [x] 1.1 Pin the published `ratatoskr-github-contracts` repository-analysis request contract; this dependency task has no local RED because it consumes the published public artifact.
- [x] 1.2 RED: add `readme_observations::fresh_readme_has_a_durable_content_addressed_store`, asserting a permitted README fixture is persisted as a `BlobRef`; the focused test failed before the README store existed.
- [x] 1.3 GREEN: add bounded conditional README acquisition, a content-addressed store, SHA-256 identity, and current-schema evidence persistence; the focused tests pass.
- [x] 1.4 RED: add `readme_observations::source_revision_creates_one_contract_valid_analysis_outbox_command`, asserting combined metadata and README evidence creates one parseable command without README bytes; the focused test failed before publication existed.
- [x] 1.5 GREEN: add the current-schema outbox uniqueness invariant and transactional command construction for the combined source revision; the focused test passes.

## 2. Idempotency and privacy

- [x] 2.1 RED: extend the fixture flow to deliver identical metadata evidence twice; it initially exposed the absence of a combined source identity.
- [x] 2.2 GREEN: implement combined source-identity idempotency and bounded payload projection; replay creates no duplicate command and no credential or raw README body is stored in the command.

## 3. Verification

- [x] 3.1 Run the exact `DEVELOPMENT.md` gate command list, including the disposable PostgreSQL schema tests and OpenSpec validation; verify the CI command list remains synchronized.
