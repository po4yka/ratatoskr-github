## Purpose

GitHub Catalog takes part in the fleet bus through complete envelopes on a closed set of subjects, answers Edge's anonymous capability probe, and serves README bytes to Knowledge. Cross-repository wire behaviour is defined in XR-021 CONTRACTS.md; this spec covers what is observable inside this repository.

## ADDED Requirements

### Requirement: The capability probe is anonymous

`GET /v1/capabilities` SHALL answer 200 with the capability document without any `x-ratatoskr-user-id` claim, and every other domain route SHALL keep requiring the claim.

#### Scenario: Probe without a claim

- **WHEN** `GET /v1/capabilities` arrives with no claim header
- **THEN** the response is 200 and the document is unchanged

#### Scenario: Preview still requires the claim

- **WHEN** `POST /v1/gh/repositories/preview` arrives with no claim header
- **THEN** the response is 401

### Requirement: Outbox rows are complete class-prefixed envelopes

Every outbox row destined for the bus SHALL store the complete canonical envelope under a subject beginning with `evt.` or `cmd.`, and the envelope id SHALL equal the row id.

#### Scenario: Analysis request envelope

- **WHEN** a queued repository analysis request is dispatched
- **THEN** the outbox row subject is `evt.knowledge.repository_analysis.requested.v1` and its payload parses as an `EventEnvelope` carrying `RepositoryAnalysisRequested` with `event_id` equal to the row id

#### Scenario: Backup policy envelope

- **WHEN** a due backup policy is published
- **THEN** the outbox row subject is `cmd.vault.backup_policy.apply_requested.v1` and its payload parses as a `CommandEnvelope` carrying `VaultBackupPolicyApplyRequested` with `command_id` equal to the row id

### Requirement: The relay publishes ack-gated with backoff

The relay SHALL set `published_at` only after the JetStream `PubAck` resolves, and a failed publish SHALL record an attempt, a safe error class and a `next_attempt_at` so later rows are not starved.

#### Scenario: Broker loss stops the service

- **WHEN** the broker connection is lost
- **THEN** readiness becomes false and the serve future returns an error

### Requirement: Knowledge can fetch README bytes with a shared secret

`GET /internal/v1/readme-blobs/{sha256_hex}` SHALL return the stored bytes with `X-Content-SHA256` when the bearer matches, 401 without or with a wrong bearer, 404 for an unknown digest, and the route SHALL be absent when the secret is unset.

#### Scenario: Authorized fetch

- **WHEN** a request carries the configured bearer for a stored digest
- **THEN** the response is 200 with the stored bytes and the digest header
