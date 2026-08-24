## Purpose

Keeps the per-repository metadata projection fresh against GitHub at minimal request cost by honoring conditional requests, and preserves what was observed as a bounded revision history.

## ADDED Requirements

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

### Requirement: Changed payload updates projection and appends a revision

The catalog SHALL update the projection when an observed body differs from the current revision and SHALL append exactly one new raw revision per distinct observed content.

#### Scenario: Changed metadata produces a new revision

- **WHEN** a refreshed body differs in any projected field from the current revision
- **THEN** the projection reflects the new values and the revision history grows by exactly one entry carrying the new raw payload

#### Scenario: Unchanged body does not duplicate revisions

- **WHEN** a refreshed 200 response carries content identical to the current revision
- **THEN** the projection stays correct and the revision count does not grow

### Requirement: Bounded revision history

The catalog SHALL retain at most a fixed number of most recent raw metadata revisions per repository, discarding only older entries beyond that bound.

#### Scenario: History never exceeds its bound

- **WHEN** more distinct bodies than the configured bound have been observed for one repository
- **THEN** exactly the most recent bound number of revisions remain, ordered from oldest to newest
