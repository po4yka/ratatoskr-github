## Purpose

The GitHub Catalog originates repository-analysis requests at runtime from user-owned metadata watches. Wire behaviour is XR-021 CONTRACTS.md section R2-08.

## ADDED Requirements

### Requirement: Tracking a repository queues its first analysis

A `track` action SHALL register a metadata-delta analysis watch for the acting user on the tracked repository with an unevaluated baseline, so the first evaluation after registration queues one analysis request. The actions `metadata` and `star` SHALL register no watch. Replaying the same idempotency key SHALL add nothing, and a second user tracking the same repository SHALL add a request of their own without adding a second watch for the first user.

#### Scenario: Track queues one request owned by the user

- **WHEN** user U submits a `track` action for a repository the provider answers with a README
- **THEN** exactly one request owned by `user:U` is queued and one `evt.knowledge.repository_analysis.requested.v1` event reaches the outbox when the dispatcher runs

#### Scenario: Replay and a second user

- **WHEN** the same `track` is replayed and then another user tracks the same repository
- **THEN** the replay adds no request and the second user adds exactly one request of their own

#### Scenario: Metadata and star register no watch

- **WHEN** a `metadata` or `star` action completes
- **THEN** no watch and no request exist for that user and repository

### Requirement: The queued request carries real README evidence

A queued request SHALL carry `readme` `Present` with a `BlobRef` that resolves in the Catalog-owned blob table when a README was fetched, and `Absent` with the real reason (`not_found` or `not_authorized`) otherwise. A runtime path SHALL NOT produce `not_preserved`. A change of the README alone SHALL queue a new request for each enabled watch.

#### Scenario: A repository without a README

- **WHEN** the provider answers 404 for the README of a tracked repository
- **THEN** the queued request carries `Absent` with reason `not_found`

### Requirement: A periodic observer evaluates registered watches

A running service with a bus SHALL, every `RATATOSKR__WATCH__OBSERVE_INTERVAL_SECONDS` (default 3600, `0` disables, at most 604800), observe at most `RATATOSKR__WATCH__BATCH` (default 10, 1 to 100) repositories that have an enabled watch, oldest observation first, through the shared rate-limit ledger with unauthenticated provider access. A changed repository SHALL queue a request for every enabled watch of every owner; an unchanged repository SHALL queue none.

#### Scenario: Changed and unchanged repositories

- **WHEN** an observer pass finds one watched repository changed and one answered 304
- **THEN** the changed repository gains a request and the other gains none

#### Scenario: The batch bound and the disabled interval

- **WHEN** more repositories than the batch hold a watch, or the interval is `0`
- **THEN** a pass observes at most the batch, oldest first, or no pass runs

#### Scenario: A withheld budget

- **WHEN** the rate-limit ledger withholds the budget
- **THEN** the pass observes nothing
