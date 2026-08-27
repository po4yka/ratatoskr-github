## 1. Contract and credential provenance

- [x] 1.1 Advance the existing `ratatoskr-contracts` Git pins to
  `55a96859363c45d7f3c4bc65db527363bfb947ea`, add the published operation
  contract crate, and regenerate `Cargo.lock`; this setup cannot begin with a
  failing local test because the typed erasure payload is absent at the pinned
  baseline. Verify with `cargo metadata --locked --offline`.
- [x] 1.2 Add `crates/catalog/tests/credentials.rs` test
  `pat_registration_records_pat_provenance` and run it red; its assertion that
  the stored credential records `pat` provenance must fail against the current
  schema.
- [x] 1.3 Add the credential provenance schema and domain registration/loading
  changes that make `pat_registration_records_pat_provenance` pass, while
  retaining encrypted-token secrecy. Verify the focused test passes.
- [x] 1.4 Add `crates/catalog/tests/credentials.rs` test
  `configured_oauth_credential_records_its_issuing_application` and run it
  red; its assertion that a configured OAuth credential records matching app
  provenance must fail.
- [x] 1.5 Implement OAuth credential registration and provenance validation so
  `configured_oauth_credential_records_its_issuing_application` passes, and
  reject app mismatch without exposing the token. Verify the focused test
  passes.

## 2. OAuth configuration and provider revoke

- [x] 2.1 Add `crates/catalog/tests/config.rs` test
  `oauth_app_configuration_is_complete_and_redacted` and run it red; its
  assertions that partial configuration is rejected and the secret is absent
  from serialized and debug output must fail.
- [x] 2.2 Implement paired OAuth client-ID/client-secret configuration and
  redaction so `oauth_app_configuration_is_complete_and_redacted` passes.
  Verify the focused test passes.
- [x] 2.3 Add `crates/catalog/tests/credentials.rs` WireMock test
  `matching_oauth_credential_deletes_github_application_grant` and run it red;
  its assertion that the configured grant endpoint receives Basic app auth,
  the JSON access token body, and reports verified only on `204` must fail.
- [x] 2.4 Implement the narrow GitHub grant-revocation adapter so
  `matching_oauth_credential_deletes_github_application_grant` passes, keeping
  both the client secret and access token out of diagnostics. Verify the
  focused test passes.
- [x] 2.5 Add `crates/catalog/tests/account_erasure.rs` WireMock test
  `pat_erasure_does_not_call_github_oauth_grant_revocation`; it verifies that a
  PAT makes no provider request, still removes local state, and acknowledges
  incomplete external revocation. The handler's already-implemented generic
  ineligible-credential branch made this post-implementation coverage green
  immediately, so no honest red run was available without an intentional
  regression.
- [x] 2.6 Verify ineligible-credential handling with
  `pat_erasure_does_not_call_github_oauth_grant_revocation`; it passes while
  retaining the incomplete outcome and no provider call.

## 3. Account-erasure acknowledgement

- [x] 3.1 Establish the minimal typed account-erasure consumer/acknowledgement
  module needed for an executable owner test; this setup cannot begin with a
  failing test because the baseline has no event-consumer entry point. Verify
  the module accepts only the published contract types.
- [x] 3.2 Add `crates/catalog/tests/account_erasure.rs` test
  `github_owner_erasure_revokes_matching_grant_then_removes_all_owner_state`
  and run it red; its assertion that the acknowledgement is `Verified` only
  after a `204` and that all owner-keyed catalog state is gone must fail.
- [x] 3.3 Implement the erasure handler and typed acknowledgement so
  `github_owner_erasure_revokes_matching_grant_then_removes_all_owner_state`
  passes, including local deletion and
  `IncompleteExternalGrantRevocation` on provider failure. Verify the focused
  success test and `failed_oauth_grant_revocation_reports_incomplete_after_local_erasure`
  pass.

## 4. Deployment evidence

- [x] 4.1 Document the exact service-only deployment keys and that a secret
  reference must resolve to `RATATOSKR__GITHUB_OAUTH__CLIENT_SECRET`; this
  documentation task cannot begin with a failing behavior test. Verify no
  example contains a usable secret or user token. Documented in `README.md`.
- [x] 4.2 Run the full repository gate from `DEVELOPMENT.md`, with each
  compiler-backed command under `build-gate --`, and run
  `openspec validate add-github-oauth-grant-revocation --strict`; record the
  observed results before archival. All commands passed locally.
