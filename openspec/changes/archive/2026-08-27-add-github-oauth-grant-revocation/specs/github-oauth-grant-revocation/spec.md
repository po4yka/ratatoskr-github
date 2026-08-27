## Purpose

Give GitHub Catalog a truthful account-erasure result by revoking a configured
OAuth application grant without releasing provider credentials or app secrets.

## ADDED Requirements

### Requirement: OAuth credentials are distinguished from personal access tokens

GitHub Catalog SHALL retain enough non-secret credential provenance to identify
whether an encrypted credential is a PAT or an OAuth access token issued to its
configured OAuth application. It SHALL not classify an unverified or
other-application token as revocable by its configured OAuth app.

#### Scenario: An OAuth credential belongs to the configured application
- **WHEN** an authenticated OAuth credential is registered with the configured
  GitHub application identity
- **THEN** the service retains it as OAuth provenance eligible for that
  application's erasure-time grant revocation

#### Scenario: A personal access token is registered
- **WHEN** a user registers a PAT
- **THEN** the service retains PAT provenance and never treats it as an OAuth
  grant issued to the configured application

### Requirement: OAuth app configuration is secret-safe and complete

GitHub Catalog SHALL accept an OAuth app client ID and client secret only as
service-local configuration. It SHALL reject a partial OAuth configuration and
SHALL not expose the secret or an access token in a serialized configuration,
debug rendering, acknowledgement, event, or diagnostic.

#### Scenario: A deployment configures an OAuth application
- **WHEN** a valid client ID and client secret are provided together
- **THEN** the service can construct its OAuth-grant revocation capability
without exposing the secret in its observable configuration output

#### Scenario: A deployment provides only one OAuth credential field
- **WHEN** configuration contains a client ID without a secret, or a secret
without a client ID
- **THEN** startup configuration fails with the field name but without the
provided value

### Requirement: Account erasure revokes a matching GitHub OAuth grant

For an account-erasure command with a stored OAuth credential issued to the
configured application, GitHub Catalog SHALL ask GitHub to delete the
application grant before deleting its local credential. It SHALL report
verified external revocation only after a successful provider response and
then remove its user-keyed local state.

#### Scenario: GitHub confirms grant deletion
- **WHEN** GitHub confirms deletion of the matching OAuth application grant
- **THEN** the owner acknowledgement reports verified erasure and the local
credential and user-keyed catalog state are no longer readable

#### Scenario: GitHub grant deletion fails
- **WHEN** the provider call for a matching OAuth grant fails or returns a
non-success response
- **THEN** GitHub Catalog removes local credential custody and user-keyed state
but acknowledges incomplete external grant revocation without exposing token
or client-secret material

### Requirement: Ineligible credentials are not sent to OAuth grant revocation

GitHub Catalog SHALL not send a PAT, an OAuth credential from another
application, or a credential whose configured app provenance is unavailable to
its OAuth grant-revocation endpoint. It SHALL still delete the local credential
and report the external outcome as incomplete.

#### Scenario: Erasure reaches a PAT
- **WHEN** account erasure reaches an account whose credential is a PAT
- **THEN** no OAuth grant-revocation request is made and the acknowledgement
states incomplete external revocation after local state is removed
