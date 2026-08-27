## ADDED Requirements

### Requirement: A watch is explicit user-owned metadata-delta policy

The Catalog SHALL register at most one enabled-or-paused metadata-delta analysis watch per user and
repository. Registration SHALL establish the repository's current metadata revision as the initial
evaluation checkpoint.

#### Scenario: Registering a watch does not analyse historic metadata

- **WHEN** a user registers a metadata-delta analysis watch for a repository with existing metadata
- **THEN** the watch stores that revision as its checkpoint and no analysis request is created

### Requirement: Metadata deltas create one paced analysis request

For each enabled watch, the Catalog SHALL queue one repository-analysis request when a newer metadata
revision is observed. It SHALL deduplicate the same immutable revision, preserve queued/pending state
as visible outstanding work, and dispatch requests through its durable pacing cursor.

#### Scenario: A watched metadata change is dispatched once

- **WHEN** a watched repository's metadata revision changes
- **THEN** the Catalog records one outstanding request and writes one
  `knowledge.repository_analysis.requested.v1` payload when that request becomes due

#### Scenario: Re-evaluating the same revision does not duplicate work

- **WHEN** the Catalog evaluates an enabled watch again without a new metadata revision
- **THEN** it creates no second request or outbox payload

### Requirement: Knowledge terminal facts resolve only their matching pending request

The Catalog SHALL resolve a request only after matching the terminal fact's owner, repository
identities, request ID, and immutable source revision. A matching completion SHALL retain its opaque
result reference; duplicate or mismatched facts SHALL leave state unchanged.

#### Scenario: Matching completion resolves visible pending state

- **WHEN** Knowledge completes a dispatched repository-analysis request with the matching identity
- **THEN** its request state becomes completed and the repository links to the opaque result
  reference exactly once
