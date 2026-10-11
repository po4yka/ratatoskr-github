## MODIFIED Requirements

### Requirement: A watch is explicit user-owned metadata-delta policy

The Catalog SHALL register at most one enabled-or-paused metadata-delta analysis watch per user and
repository. Registration SHALL take an explicit baseline: `Current` establishes the repository's
current metadata revision as the initial evaluation checkpoint, and `Unevaluated` stores no
checkpoint so the first evaluation queues an analysis request.

#### Scenario: Registering a watch does not analyse historic metadata

- **WHEN** a user registers a metadata-delta analysis watch with the `Current` baseline for a repository with existing metadata
- **THEN** the watch stores that revision as its checkpoint and no analysis request is created

#### Scenario: An unevaluated watch analyses the first revision

- **WHEN** a user registers a watch with the `Unevaluated` baseline and the repository is then evaluated
- **THEN** exactly one analysis request is queued for that watch
