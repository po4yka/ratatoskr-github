## MODIFIED Requirements

### Requirement: Changed source evidence updates projection and appends a revision

The catalog SHALL conditionally acquire the bounded README representation after a fresh metadata response, SHALL preserve permitted README bytes through an immutable `BlobRef`, and SHALL calculate a SHA-256 combined source identity from normalized metadata and the README state.  It SHALL update the projection when that source identity differs, SHALL append exactly one source revision per distinct identity, and SHALL atomically append one corresponding `knowledge.repository_analysis.requested.v1` outbox command carrying the published typed repository-analysis request. README bytes and credentials MUST NOT be carried in the command.

#### Scenario: Changed metadata or README produces a new revision

- **WHEN** fresh normalized metadata or the immutable README state differs from the current source revision
- **THEN** the projection reflects the metadata values, the revision history grows by exactly one entry carrying the source evidence, and one corresponding repository-analysis request is committed

#### Scenario: Unchanged metadata and README do not duplicate revisions

- **WHEN** a refreshed 200 response carries metadata and README evidence identical to the current source revision, or the README conditional request returns `304 Not Modified`
- **THEN** the projection stays correct, the revision count does not grow, and no repository-analysis request is appended
