## Context

See proposal.md. The Catalog owns metadata revisions and reserves the event subject, but it has neither README acquisition nor a BlobStore-backed immutable source projection. Knowledge cannot read Catalog tables and no repository payload contract has been made executable.

## Goals / Non-Goals

**Goals:**

- Acquire README conditionally, store its permitted body by reference, and bind it with SHA-256 metadata evidence into one immutable source revision.
- Commit one fact-as-event with each new combined repository source revision.
- Make failure/redelivery safe before introducing live broker transport.
- Keep source authority, content digest, and private-data boundaries explicit.

**Non-Goals:**

- Broker connection, a background publisher loop, Knowledge implementation, Git clone/mirror behavior, or client surfaces.
- Publishing for unchanged metadata, star/list changes, or policy mutations.

## Decisions

### D1. Contract first, then Catalog dependency update

The event payload will be added to the shared contracts repository/store and published at a pinned immutable revision before Catalog constructs it. A generic JSON object is rejected because Knowledge needs independently validated source identity, digest, and blob-reference semantics.

### D2. Combined SHA-256 source identity is the outbox deduplication key

The event identifies the immutable metadata digest and optional README digest/blob reference. A unique constraint on that combined identity makes retries safe and keeps old facts available for forensic replay. MD5 is replaced because the cross-service content identity requires a canonical SHA-256 digest.

### D3. README bodies are acquired conditionally and kept out of events

The provider adapter adds a bounded conditional README fetch. A successful permitted body is sent to the Catalog BlobStore seam and only its typed reference/digest enters durable rows and the event. A 304 preserves the current evidence; absence/unavailability is explicit. Sending README bytes in the event is rejected for privacy and size reasons.

### D4. Write the fact in the evidence transaction

The evidence transaction persists the combined source revision and outbox row before commit. Provider and BlobStore operations occur outside the transaction; transport remains an outbox consumer after commit.

## Risks / Trade-offs

- [The contract must change in another repository] → publish and pin it before applying this change; reject untyped payloads.
- [README blob storage fails after provider retrieval] → no source revision or outbox row commits; a bounded retry reuses conditional state.
- [Bounded revision retention can remove local metadata payloads] → event and blob references preserve the immutable analysis source under its retention policy.
- [No broker worker exists yet] → this change proves durable publication through the outbox; transport activation remains separately observable work.

## Migration Plan

1. Publish the cross-repository payload contract and pin its revision.
2. Add provider/BlobStore seams and edit the current schema in place for README evidence and source identity.
3. Deploy Catalog; only subsequently observed metadata/README revisions create events.
4. If rollback is required, stop consuming the subject; committed source revisions and facts remain replayable.
