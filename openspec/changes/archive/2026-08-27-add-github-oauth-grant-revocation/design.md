## Context

See [proposal.md](proposal.md) for the motivation. The current catalog stores a
single encrypted token per account and labels its loader as PAT-only. It has a
strict redacting configuration boundary and a disposable database generated
directly from `schema.sql`. Workspace change
`coordinate-account-controls-lifecycle` requires a GitHub owner acknowledgement
using the published erasure contract.

## Goals / Non-Goals

**Goals:**

- Persist minimal non-secret credential provenance for PAT versus a configured
  OAuth application.
- Add a redacting, all-or-nothing OAuth-app configuration and a narrow provider
  adapter for deleting a GitHub application grant.
- Consume and acknowledge the account-erasure command with truthful verified or
  incomplete external-revocation outcomes.

**Non-Goals:**

- Implement OAuth browser authorization, Device Flow exchange, token refresh,
  general GitHub provider synchronization, or any Platform/Web endpoint.
- Revoke a PAT or a credential from another OAuth app through this app's
  provider credential.
- Create a migration: the current `schema.sql` is changed in place.

## Decisions

### Keep provider authentication data in Catalog only

Add an `oauth` credential provenance alongside `pat`, with the configured app
client ID recorded only as non-secret provenance. The existing encrypted token
storage remains the sole ciphertext location. This lets erasure prove that a
token belongs to the configured app before using it in a grant revoke. A
token-prefix heuristic is rejected: prefixes identify token families, not the
issuing OAuth application.

### Configure an all-or-nothing OAuth app boundary

Add `RATATOSKR__GITHUB_OAUTH__CLIENT_ID` and
`RATATOSKR__GITHUB_OAUTH__CLIENT_SECRET`; validate the pair and redact the
secret in serialization and `Debug`. The public GitHub API base remains a
fixed default, rather than a deployment-provided URL, so a production token
cannot be redirected to an arbitrary host. A client-secret reference is
resolved by the deployment into the process environment; secret-store
integration remains outside this service.

### Revoke the grant, not merely one token

For a matching OAuth credential, the adapter calls GitHub's
`DELETE /applications/{client_id}/grant` with HTTP Basic client ID/secret,
`Accept: application/vnd.github+json`, an explicit GitHub API version, and a
JSON `access_token` body. A `204` is verified external revocation. This
endpoint is chosen over `/token` because deletion of the grant removes all
tokens associated with that application and user. Provider requests are
exercised with WireMock only; neither a live grant nor a credential is needed
for tests.

### Delete local custody even if the provider is unavailable

The erasure handler attempts a matching grant revoke while the token is still
decryptable. It then deletes all Catalog data keyed by the internal owner and
emits `Verified` only after the provider confirmed deletion. Any no-config,
ineligible-token, transport, or non-success provider case emits
`IncompleteExternalGrantRevocation` after the same local deletion. Retaining a
secret solely for retries violates the requested account erasure.

### Update the contract source deliberately

Advance all `ratatoskr-contracts` Git dependency pins to
`55a96859363c45d7f3c4bc65db527363bfb947ea`, add the published operation
contract crate, regenerate `Cargo.lock` with Cargo, and keep the existing
source dependency convention. This is required to create an actual typed owner
acknowledgement rather than an internal-only cleanup path.

## Risks / Trade-offs

- [OAuth app secret is supplied incorrectly] → reject partial configuration,
  redact diagnostics, and make the deployment setup name the exact two fields.
- [A provider outage happens after the user requested erasure] → delete all
  Ratatoskr-owned state and report the external part incomplete; never claim a
  complete provider revoke.
- [A token from another app is submitted] → require persisted matching app
  provenance and skip the provider call otherwise.
- [New contract code changes crate surface] → compile the exact published
  contract pin and cover command-to-acknowledgement behavior with a test.

## Migration Plan

1. Deploy the code with OAuth configuration absent; existing PAT behaviour
   remains local-only and truthfully incomplete during account erasure.
2. Inject the OAuth app ID and secret into GitHub Catalog's deployment secret
   configuration, then register only OAuth credentials verified for that app.
3. If rollback is needed, stop accepting new erasure commands before reverting
   code. Completed local deletion cannot be restored, and incomplete outcomes
   remain the record of the already-attempted provider revoke.
