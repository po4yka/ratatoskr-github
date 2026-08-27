## Purpose

Publishes immutable repository-observation facts after Catalog commits them, so downstream consumers can begin analysis from durable, replay-safe evidence without access to Catalog tables.

## ADDED Requirements

### Requirement: Committed repository source revisions publish a bounded analysis command

The Catalog SHALL append exactly one `knowledge.repository_analysis.requested.v1` outbox record for each newly committed combined repository source revision. Its payload SHALL conform to published `ratatoskr-github-contracts`, containing canonical repository identity, authorization owner, SHA-256 attributes digest, bounded metadata, and a `ReadmeRevision` whose present form contains only an immutable `BlobRef`; it MUST NOT contain credentials or unbounded raw bodies.

#### Scenario: First repository source revision is published

- **WHEN** a repository's first distinct metadata and README state is committed
- **THEN** the same transaction creates one `knowledge.repository_analysis.requested.v1` outbox record identifying that immutable source revision

#### Scenario: Changed repository source revision is published

- **WHEN** a different metadata body or README state becomes the current source revision
- **THEN** exactly one new analysis command is available for the new source identity and the prior command remains unchanged

### Requirement: Publication is atomic and replay-safe

The Catalog SHALL make the repository source revision and its analysis command durable in one transaction and SHALL deduplicate publication by the immutable source identity.

#### Scenario: Retried observation does not duplicate an event

- **WHEN** the same source revision is applied again after an interrupted caller or redelivery
- **THEN** the outbox contains one analysis command for that revision and no duplicate active command

#### Scenario: Unmodified metadata produces no event

- **WHEN** GitHub reports not modified or the received metadata and README state is identical to the current source revision
- **THEN** no new repository-analysis request outbox record is created
