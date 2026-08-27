# repository-metadata Specification

## Purpose
Keeps the per-repository metadata projection fresh against GitHub at minimal request cost by honoring conditional requests, and preserves what was observed as a bounded revision history.

## Requirements

### Requirement: Metadata projection from provider observations

The catalog SHALL maintain one current metadata projection per repository covering description, primary language, stargazer count, topics, default branch, and last push time.

#### Scenario: First metadata observation creates projection and revision

- **WHEN** a repository body is observed for a repository with no prior metadata
- **THEN** the projection holds the observed field values and exactly one raw revision is recorded for that repository

### Requirement: Conditional short-circuit on not-modified

When the provider reports that stored metadata is unmodified, the catalog SHALL NOT rewrite the projection or append a revision, SHALL treat the previously stored revision as still current, and SHALL record the observation cheaply.

#### Scenario: Not-modified response leaves state untouched

- **WHEN** a refresh carries the repository's stored validator and the provider answers not modified
- **THEN** the projection values are unchanged, no new revision appears, and the stored revision remains the current one

#### Scenario: Stored validator is presented to the provider

- **WHEN** a repository with a stored validator is refreshed
- **THEN** the outgoing request presents that validator so the provider can answer not modified

### Requirement: Changed source evidence updates projection and appends a revision

The catalog SHALL conditionally acquire the bounded README representation after a fresh metadata response, SHALL preserve permitted README bytes through an immutable `BlobRef`, and SHALL calculate a SHA-256 combined source identity from normalized metadata and the README state. It SHALL update the projection when that source identity differs, SHALL append exactly one source revision per distinct identity, and SHALL atomically append one corresponding `knowledge.repository_analysis.requested.v1` outbox command carrying the published typed repository-analysis request. README bytes and credentials MUST NOT be carried in the command.

#### Scenario: Changed metadata or README produces a new revision

- **WHEN** fresh normalized metadata or the immutable README state differs from the current source revision
- **THEN** the projection reflects the metadata values, the revision history grows by exactly one entry carrying the source evidence, and one corresponding repository-analysis request is committed

#### Scenario: Unchanged metadata and README do not duplicate revisions

- **WHEN** a refreshed 200 response carries metadata and README evidence identical to the current source revision, or the README conditional request returns `304 Not Modified`
- **THEN** the projection stays correct, the revision count does not grow, and no repository-analysis request is appended

### Requirement: Bounded revision history

The catalog SHALL retain at most a fixed number of most recent raw metadata revisions per repository, discarding only older entries beyond that bound.

#### Scenario: History never exceeds its bound

- **WHEN** more distinct bodies than the configured bound have been observed for one repository
- **THEN** exactly the most recent bound number of revisions remain, ordered from oldest to newest
