## ADDED Requirements

### Requirement: Provider calls are bounded

Every call of the provider adapter SHALL be bounded by a connect timeout of 5 seconds and a total timeout that defaults to 30 seconds and is configurable through `RATATOSKR__PROVIDER__TIMEOUT_MS` (1000 to 120000). A call that exceeds its bound SHALL fail as a transport error.

#### Scenario: A stalled provider times out

- **WHEN** the provider answers later than the configured total timeout
- **THEN** repository fetch, README fetch, grant revocation and PAT authentication each return a transport error within the bound

#### Scenario: Erasure cannot hang

- **WHEN** account erasure reaches a provider that never answers the grant revocation
- **THEN** the erasure returns an error inside the bound and the owner's data is still present
