## Why

`github.repository.observed.v1` is reserved in the Catalog schema but no committed observation writes an outbox record. Knowledge therefore has no real upstream event with which it can begin repository analysis or prove replay behaviour.

## What Changes

- Add conditional README acquisition, content-addressed blob storage, and SHA-256 metadata/README revision evidence alongside existing metadata observations.
- Add durable, transactional publication of the reserved repository-observation fact when either immutable metadata or README evidence changes.
- Define a bounded, credential-free payload reference suitable for downstream analysis; delivery occurs only after the transaction commits and is deduplicated by the observation identity.
- Provide an idempotent outbox publisher seam and tests for initial publication, redelivery, metadata changes, and README changes.

## Capabilities

### New Capabilities

- `repository-observation-events`: Catalog publishes a durable, replay-safe repository-observation fact containing stable repository, SHA-256 metadata evidence, and an optional immutable README blob reference.

### Modified Capabilities

- `repository-metadata`: A committed metadata or README evidence revision emits exactly one repository-observation fact for the combined immutable source identity.

## Impact

- Affects Catalog provider access, BlobStore boundary, persistence, eventing, current-schema DDL, repository metadata/README tests, and the operator delivery worker.
- Requires a canonical cross-repository event payload contract before implementation; no credentials, README bodies, or provider SDK values may enter the event.
