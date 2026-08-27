## Context

Repository metadata refresh is already conditional and durable, and the Catalog has a transactional
outbox. The former placeholder watch table has no owner, action, checkpoint, or result state.
Knowledge owns the request budget, provider invocation, requeue decisions, and analysis result.

The workspace `repository-analysis-intake` spec requires an immutable input reference and a
deduplication key derived from the stable repository identity, immutable metadata revision, README
state, and requested contract. Catalog currently does not preserve README bytes, so this first
producer honestly sends `readme: absent/not_preserved`.

## Decision

`repository_watches` represents an enabled user-owned `metadata_changed` policy for one repository.
Registration records the current metadata hash as the baseline, so creating a watch does not analyse
old state. Each changed hash creates at most one `repository_analysis_requests` row per watch.

Request rows are initially `queued`, which is an operator-visible pending state. A singleton dispatch
cursor assigns each row a durable due time; the dispatcher moves no more than one due row to
`pending` and inserts its typed payload into the transactional outbox. This shapes Catalog egress
without making a budget or admission decision for Knowledge.

The request carries only bounded metadata and the explicit missing-README state. It hashes that
canonical input with SHA-256 and makes the digest both its contract idempotency key and durable
deduplication value. Completion/failure ingestion matches the owner, Catalog repository ID, GitHub
numeric ID, request ID, and source revision before changing a pending row. Completion stores only
Knowledge's opaque `EntityRef`.

## Consequences

- Metadata refresh never calls Knowledge or an LLM; a delayed/failed Knowledge request does not roll
  back the metadata projection.
- A consumer can render `queued` and `pending` as "still indexing" and terminal state with its
  opaque result reference.
- README preservation and release-specific provider observation remain separate changes; they can
  create a new immutable revision and reuse this intake path.
