## Purpose

Exposes GitHub Catalog's repository metadata and confirmed mode/mutation behavior through the host-local authenticated domain API while retaining auditable component-level truth.

## ADDED Requirements

### Requirement: The domain API is loopback-only and trusts only Edge identity

The service SHALL bind its first-version domain API to the fleet-assigned loopback listener separately from the operator listener. Repository routes SHALL require a valid Edge-injected `x-ratatoskr-user-id`, SHALL reject missing or malformed identity before provider or database work, and SHALL return only the shared safe error envelope. `/v1/capabilities` SHALL advertise repository preview and action availability without exposing account or credential data.

#### Scenario: Direct unauthenticated access has no effect

- **WHEN** a request reaches a repository route without a valid injected user identifier
- **THEN** it is refused before provider contact and no Catalog state changes

#### Scenario: The service exposes both listener responsibilities

- **WHEN** the configured service starts successfully
- **THEN** the operator listener serves health on its existing address and the loopback domain listener serves capabilities and repository routes on its distinct address

### Requirement: Preview resolves live metadata without mutating catalog intent

The preview route SHALL accept only a canonical `https://github.com/<owner>/<repository>` URL with no credentials, port, query, fragment, or sub-resource path. It SHALL select only a connected account owned by the authenticated user when one is required, fetch normalized provider metadata under the shared rate-limit ledger, and return the shared preview contract. Preview SHALL NOT apply metadata, change repository mode, mark backup policy dirty, or invoke a provider mutation.

#### Scenario: Preview returns requested display fields

- **WHEN** the fake provider resolves a visible repository with description, star count, and language
- **THEN** the response carries its stable identity, current alias, canonical URL, description, stars, language, and mode capabilities with no Catalog write

#### Scenario: A sub-resource URL is not a repository preview

- **WHEN** the route receives a GitHub issues, pull, tree, blob, release, query-bearing, or fragment-bearing URL
- **THEN** it returns a safe invalid-request outcome without provider contact

### Requirement: Actions enforce user/account ownership and explicit evidence

The action route SHALL require a stable preview target, mode, confirmation evidence reference, and idempotency key. A `star` action SHALL additionally require a connected account owned by the injected user with the required scope. The provider SHALL never be contacted for missing confirmation evidence, target mismatch, foreign account, disconnected account, insufficient scope, or an unsupported mode.

#### Scenario: Foreign account cannot star

- **WHEN** a user submits a confirmed star request naming another user's account reference
- **THEN** the action is safely refused before provider contact and reveals no foreign account detail

#### Scenario: Metadata and track do not call the star mutation

- **WHEN** valid confirmed `metadata` and `track` actions execute
- **THEN** neither action invokes the provider-star mutation, while their applicable Catalog and desired-policy components are reported

### Requirement: Action execution reports every component without rollback

The service SHALL execute applicable steps in commitment order and return metadata, provider-star, and desired-backup outcomes separately. A failure SHALL skip only steps whose prerequisites are absent, SHALL preserve every earlier successful outcome, and SHALL never issue a compensating provider mutation. A post-provider persistence failure SHALL still report that the provider confirmed the requested star state while reporting the failed Catalog-dependent component truthfully.

#### Scenario: Provider success survives a later persistence fault

- **WHEN** the fake provider confirms the star and injected persistence failure prevents the local star projection or desired-policy acceptance
- **THEN** the response reports provider-star success plus the exact failed/skipped later components and no unstar call

#### Scenario: Provider refusal blocks dependent policy work

- **WHEN** the provider does not confirm the requested star state
- **THEN** provider star reports failure, dependent desired-policy work reports skipped with a safe reason, and the aggregate is failed or partial according to the metadata outcome

### Requirement: Actions are idempotent under replay and uncertain delivery

The service SHALL persist enough action result evidence by authenticated user and idempotency key to replay the same component result. Reuse of a key with another user, target, mode, or confirmation reference SHALL be refused. A replay SHALL NOT repeat a provider mutation already recorded successful.

#### Scenario: Exact replay returns recorded truth

- **WHEN** the same action request is submitted twice after the first completed
- **THEN** the second response returns the converged component truth and the fake provider records one effective star mutation

### Requirement: A fake provider proves the live API gate

Repository-local acceptance SHALL be runnable with disposable PostgreSQL and a synthetic GitHub provider that controls metadata, scope, mutation, rate-limit, and failure outcomes. The gate SHALL start the real service listeners, observe readiness, and call preview and action routes over HTTP.

#### Scenario: Live smoke exercises partial results

- **WHEN** the service runs against a fake provider configured for star success followed by desired-policy failure
- **THEN** an HTTP client observes readiness, a real preview response, and the expected partial action response through the served API
