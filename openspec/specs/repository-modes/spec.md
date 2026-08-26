# repository-modes Specification

## Purpose
Records whose intent governs each known repository - automatic star-driven presence, explicit tracking without a star, or deliberate exclusion - so that ingestion, backup-policy derivation, and listing can honor what the user actually decided, with every decision validated and audited.

## Requirements

### Requirement: Repository mode vocabulary

The catalog SHALL give every repository a mode drawn from `auto`, `tracked`, `ignored`, or unclassified, where `auto` means catalog presence is governed by star state, `tracked` means explicitly kept regardless of stars, `ignored` means deliberately excluded, and unclassified means known but never classified.

#### Scenario: A first star observation classifies an unclassified repository as auto

- **WHEN** synchronization observes a star for a repository whose mode is unclassified
- **THEN** the repository's mode becomes `auto` afterward

#### Scenario: Metadata-only ingestion leaves the repository unclassified

- **WHEN** a repository enters the catalog through metadata observation without any track, star, or ignore decision
- **THEN** its mode remains unclassified

### Requirement: Explicit mode requests are limited to tracked and ignored

The catalog SHALL accept explicit mode-setting requests only for `tracked` and `ignored`, SHALL treat a request for an unchanged mode as a successful no-op, and SHALL refuse a direct request for `auto` without changing any state.

#### Scenario: A direct request for auto is refused without side effects

- **WHEN** an authorized caller directly requests mode `auto` for a repository
- **THEN** the request is refused with a validation outcome naming the rule, the repository's mode is unchanged, and no transition record claims a change

#### Scenario: Re-requesting the current mode succeeds as a no-op

- **WHEN** a caller requests `tracked` for a repository that is already `tracked`
- **THEN** the request succeeds reporting the mode was already in effect, and the repository's mode stays `tracked`

### Requirement: Ignoring requires an unstarred repository

The catalog SHALL refuse a transition to `ignored` while the acting account currently stars the repository, leaving prior mode and star state entirely unchanged, and SHALL require an explicit unstar before ignoring.

#### Scenario: Ignoring a currently starred repository is refused

- **WHEN** a caller requests `ignored` for a repository the account currently stars
- **THEN** the request is refused naming the conflict, and neither the mode nor the star state changes

### Requirement: Tracked intent survives star changes

The catalog SHALL keep a `tracked` repository `tracked` through both star and unstar effects, so tracking is never silently promoted to `auto` nor cleared by star activity.

#### Scenario: Starring a tracked repository keeps it tracked

- **WHEN** a star mutation succeeds for a repository whose mode is `tracked`
- **THEN** the repository becomes starred and its mode remains `tracked`

#### Scenario: An evidenced unstar keeps a tracked repository tracked

- **WHEN** the star state of a `tracked` repository becomes unstarred
- **THEN** the repository's mode remains `tracked`

### Requirement: Unstarring releases auto governance

The catalog SHALL return an `auto` repository to unclassified when its star state becomes unstarred, since the reason for automatic governance is gone.

#### Scenario: Unstarring an auto repository returns it to unclassified

- **WHEN** the star state of an `auto` repository becomes unstarred
- **THEN** the repository's mode is unclassified afterward

### Requirement: Explicit exclusion outranks starring

The catalog SHALL refuse a star mutation targeting an `ignored` repository while it remains ignored, requiring an explicit return to `tracked` or another allowed classification first, so deliberate exclusion cannot be bypassed through the write path.

#### Scenario: Starring an ignored repository is refused

- **WHEN** a caller submits a star mutation for a repository whose mode is `ignored`
- **THEN** the operation is refused naming the conflict, no provider call is made, and the mode stays `ignored`

### Requirement: Synchronization never overrides an explicit mode

The catalog SHALL let synchronization promote only unclassified repositories to `auto` upon star evidence, and SHALL leave `tracked` and `ignored` modes untouched by any snapshot or scan result.

#### Scenario: An ignored repository stays ignored despite appearing in a completed snapshot

- **WHEN** a completed full snapshot or successful incremental scan observes a star for a repository whose mode is `ignored`
- **THEN** the repository's star state reflects the observation truthfully and its mode remains `ignored`

#### Scenario: A completed snapshot leaves tracked and ignored classifications alone

- **WHEN** a completed enumeration covers repositories whose modes are `tracked` or `ignored`
- **THEN** those modes are exactly as they were before the run

### Requirement: Mode transitions are audited operations

The catalog SHALL record every accepted mode transition - including no-op confirmations - as one audit entry carrying the acting principal, the calling source, the previous and resulting modes, the time, and the operation's idempotency key, and a retried request with the same idempotency key SHALL produce the same end state with exactly one audit entry.

#### Scenario: A validated transition records who changed what

- **WHEN** a caller successfully moves a repository from unclassified to `tracked`
- **THEN** one audit entry exists naming the principal, the source, the previous and resulting modes, and the time

#### Scenario: Retrying a mode request with the same idempotency key adds no second record

- **WHEN** the same mode request is submitted twice with the same idempotency key and the first attempt succeeded
- **THEN** the end state matches the first attempt and exactly one audit entry exists for that key
