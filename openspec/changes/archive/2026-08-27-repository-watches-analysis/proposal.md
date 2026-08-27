# Repository watches and Knowledge analysis requests

## Why

GitHub Catalog records metadata revisions but has no user-owned watch policy, durable analysis
request tracking, or completion linkage. A metadata change therefore cannot safely ask Knowledge to
analyse a repository or show whether that request remains pending.

The workspace store specification `repository-analysis-intake` defines the shared request and
terminal-fact contract. Knowledge now accepts that contract durably; this change implements the
Catalog producer and result projection.

## What changes

- Replace the placeholder watch definition with user-owned metadata-delta watches and their last
  evaluated checkpoint.
- On an observed metadata delta, construct a bounded immutable repository-analysis request, queue it
  exactly once, and pace outbox dispatch through a durable Catalog cursor.
- Track queued, pending, completed, and failed request state; consume matching Knowledge terminal
  facts idempotently and store only the opaque result reference.
- Add PostgreSQL integration tests for registration, delta detection, deduplication, dispatch, and
  pending-state resolution.

## Impact

- Affected code: `crates/catalog`, `schema.sql`, catalog documentation, and the local OpenSpec.
- Affected cross-repository behaviour: uses the published `repository-analysis-intake` contract; no
  contract shape is changed.
- No Git execution, LLM call, budget decision, README fetch, or notification delivery is added.
