## Purpose

Moves retained GitHub Catalog state from the retired service into Ratatoskr with auditable parity checks and an owner-controlled, reversible activation boundary.

## ADDED Requirements

### Requirement: One-shot, credential-free legacy import
The catalog SHALL accept a temporary, operator-provided legacy PostgreSQL source only for an explicit import invocation, map each legacy repository to the stable GitHub numeric identity where the source provides it, preserve every supplied `starred_at` value, and import account, star, list, and repository state without retaining any dependency on the legacy schema after the invocation finishes. When the source has only an observation time, the catalog SHALL preserve it as an observation time and SHALL NOT fabricate a provider star time. Repeating the same source import SHALL converge to the same catalog state without duplicating identities, observations, list memberships, or policies.

#### Scenario: Repeating a synthetic import is idempotent
- **WHEN** an operator imports the same synthetic legacy source twice
- **THEN** the catalog has the same stable repository identities, account-scoped stars, list memberships, observations, and desired policy after each invocation, with no duplicate records

#### Scenario: Imported star timestamp is preserved
- **WHEN** an imported legacy star carries a provider `starred_at` value
- **THEN** the catalog's current star state and its import evidence retain that supplied value rather than replacing it with the import time

#### Scenario: Unknown provider star time stays unknown
- **WHEN** the legacy source records a starred repository's last observation but no provider `starred_at`
- **THEN** the catalog records the supplied observation time, leaves the provider star time unknown, and requires a complete post-reconnect provider snapshot before cutover readiness

#### Scenario: Identity mapping rejects unresolved legacy records
- **WHEN** a legacy repository lacks a stable GitHub numeric identity or conflicts with a different known provider identity
- **THEN** the import reports that record as unresolved, does not invent an identity from an alias, and refuses cutover readiness until the owner resolves it

### Requirement: Reauthorization is mandatory after import
The importer SHALL never select, copy, decrypt, persist, include in a report, or log legacy encrypted credentials, token values, authorization headers, device codes, or credential ciphertext. Every imported account SHALL enter `reauthorization_required`, and synchronization and provider mutations for that account SHALL remain unavailable until the user reconnects through the supported credential flow.

#### Scenario: Credential-looking legacy fields are excluded
- **WHEN** the legacy integration table contains an encrypted-token field beside connection metadata
- **THEN** the importer reads only its allow-listed metadata columns, creates no credential record, and emits no credential value or ciphertext

#### Scenario: Imported account cannot synchronize before reconnecting
- **WHEN** an account exists only because of a legacy import
- **THEN** a requested synchronization or mutation is refused as not connected until a supported reauthorization completes

### Requirement: Shadow synchronization reports parity without legacy authority
The catalog SHALL provide an explicit shadow run that executes the normal catalog synchronization for a reauthorized account, compares the resulting catalog projection with a temporary legacy source, and emits a deterministic redacted report of repository identities/aliases, star state and timestamps, native lists and memberships, and supported-policy differences. Legacy list names without provider list identities SHALL remain import evidence and SHALL be compared with provider-derived lists rather than synthesized into provider identities. A shadow run SHALL not write to the legacy source, make legacy absence authoritative, or enable production reads or provider writes.

#### Scenario: Shadow run emits an actionable diff
- **WHEN** a reauthorized account's catalog state differs from the temporary legacy source
- **THEN** the shadow result identifies the differing category and stable non-secret references, reports counts and per-record classification, and marks the account ineligible for cutover

#### Scenario: Matching shadow run is clean
- **WHEN** a complete catalog synchronization and comparison find identical supported state
- **THEN** the report records a clean result with its source and catalog observation boundaries and permits owner review for the next cutover stage

### Requirement: Owner-approved staged cutover and rollback
The catalog SHALL keep legacy routing disabled by default and SHALL permit the staged sequence of read activation followed by write activation only when an owner-approved checklist records a clean required shadow result, successful gate evidence, reauthorization status, and an explicit activation approval. The checklist SHALL name a rollback action that restores the prior routing without deleting catalog state, rerunning import, or making a legacy source a continuing dependency.

#### Scenario: Unapproved cutover is refused
- **WHEN** an operator attempts to activate catalog reads or writes without an owner approval that references a clean required shadow report
- **THEN** the activation is refused and existing routing remains unchanged

#### Scenario: Read activation precedes write activation
- **WHEN** an owner approves cutover after the required verification
- **THEN** the checklist activates reads first, requires stability evidence before writes, and records the independent status of both stages

#### Scenario: Rollback preserves import evidence
- **WHEN** the owner invokes the documented rollback after either activation stage
- **THEN** the prior routing is restored and the catalog retains imported state, shadow reports, and audit evidence without querying or modifying legacy tables
