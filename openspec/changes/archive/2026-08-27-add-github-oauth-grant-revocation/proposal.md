## Why

Account erasure can remove Ratatoskr's encrypted GitHub credential, but the
service currently only models a PAT and has no way to revoke an OAuth grant at
GitHub. A local delete alone must not be presented as provider-side revocation.

## What Changes

- Add an OAuth credential mode and provider-owned OAuth-app configuration to
  GitHub Catalog without exposing the client secret outside the service.
- On a matched OAuth credential erasure, revoke the user's GitHub application
  grant before deleting local credential custody and report a typed outcome.
- Treat PATs and credentials from another OAuth app as locally erasable but
  externally incomplete; do not submit them to the configured app endpoint.
- Consume the published account-erasure contract at
  `ratatoskr-contracts` commit `55a96859363c45d7f3c4bc65db527363bfb947ea`
  and implement the GitHub owner acknowledgement required by workspace change
  `coordinate-account-controls-lifecycle`.

## Capabilities

### New Capabilities

- `github-oauth-grant-revocation`: Securely revokes a configured GitHub OAuth
  application grant during account erasure and reports its actual outcome.

### Modified Capabilities

<!-- None. -->

## Impact

`crates/catalog` gains OAuth credential metadata, configuration validation and
a GitHub REST adapter; `schema.sql` changes in place under the development
schema rule. The Git dependency pin advances to the published erasure contract.
Deployments configure the OAuth app client ID and secret only in this service;
no browser, Platform, event, or log receives either the secret or user token.
