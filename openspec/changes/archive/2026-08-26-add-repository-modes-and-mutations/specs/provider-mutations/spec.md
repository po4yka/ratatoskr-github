## Purpose

Executes confirmed write operations against GitHub - starring, unstarring, and star-list membership filing - under an authorization context supplied by the calling product flow, so every external change is explicit, replay-safe, scope-checked, and reported truthfully per operation, including partial success across batches.

## ADDED Requirements

### Requirement: Mutations require a complete authorization context

The catalog SHALL accept a mutation only when the caller supplies an authorization context naming the acting account, principal, and calling source with an idempotency key for each operation, SHALL refuse mutations whose account is not connected or lacks the granted scopes the operation requires before contacting the provider, and SHALL record every refusal as an audit entry without provider side effects.

#### Scenario: A mutation for an unconnected account is refused without touching the provider

- **WHEN** a mutation arrives whose account reference does not resolve to a connected account
- **THEN** the operation is refused as unauthorized, the provider receives no request, and an audit entry records the refusal

#### Scenario: Starring without the required scopes is refused

- **WHEN** a star mutation's account holds granted scopes that do not satisfy the star requirement
- **THEN** the operation is refused as unauthorized, the provider receives no request, and the audit entry names the missing capability

### Requirement: Star and unstar execute idempotently

The catalog SHALL execute star and unstar through provider operations that are safe to repeat, SHALL report `applied` or `already-applied` from the provider's own confirmation of the resulting state, and SHALL guarantee that resubmitting a successfully completed mutation with its idempotency key yields the same end state, contacts the provider no additional time for the replayed attempt, and leaves exactly one successful audit entry for that key.

#### Scenario: Retrying a completed star produces the same end state and one record

- **WHEN** a star mutation succeeds and is resubmitted with the same idempotency key
- **THEN** the repository remains starred exactly once over, the response reports already-applied, and the audit log holds exactly one successful entry for that key

#### Scenario: A failed attempt does not consume its idempotency key

- **WHEN** a mutation fails at the provider and is later retried with the same idempotency key
- **THEN** the retry executes against the provider again and, on success, records the single successful audit entry for that key

### Requirement: Truthful star outcomes from provider confirmation

The catalog SHALL derive a star or unstar outcome from what the provider confirms about the resulting state - applied when the provider reports the state newly reached, already-applied when it reports the requested state already held - and SHALL report failure without inventing success when the provider call fails.

#### Scenario: Starring an already-starred repository reports already-applied

- **WHEN** a star mutation reaches the provider for a repository the account already stars
- **THEN** the outcome reports already-applied and the local star state stays starred with unchanged established timestamps

#### Scenario: A provider failure reports failure without local changes

- **WHEN** the provider rejects or cannot complete a star mutation
- **THEN** the outcome reports failed with the classified reason, the local star state is unchanged, and an audit entry records the failure

### Requirement: List membership writes preserve unrelated lists

The catalog SHALL compute the complete desired list set for a repository from the provider's live membership immediately before writing, SHALL apply additions and removals against that set so memberships in other lists survive untouched, and SHALL record the desired set it wrote in the audit entry.

#### Scenario: Adding a list keeps the repository's other live lists intact

- **WHEN** a membership-add files a repository into one list while the provider shows it living in two others
- **THEN** afterward the repository is a member of all three lists on the provider

#### Scenario: Removing a list leaves the remaining memberships in place

- **WHEN** a membership-remove targets one of three lists holding the repository
- **THEN** afterward the repository remains a member of exactly the other two lists on the provider

### Requirement: Batched mutations report partial success faithfully

The catalog SHALL execute the operations of a submitted batch independently, SHALL return one truthful outcome per operation in submission order - applied, already-applied, refused, or failed - SHALL let any operation's failure neither prevent nor undo the others, and SHALL let a resubmitted batch short-circuit previously succeeded operations through their idempotency keys while retrying only incomplete ones.

#### Scenario: One failing operation strands nothing

- **WHEN** a batch of three operations meets a provider failure on the second while the first and third succeed
- **THEN** the returned outcomes are applied, failed, and applied in order, the first and third effects stand, and each operation carries its own audit entry

#### Scenario: Resubmitting the batch retries only what did not succeed

- **WHEN** the same batch is resubmitted after its second operation failed and the provider now accepts it
- **THEN** the first and third outcomes come back already-applied with no new provider calls for them, the second applies, and no additional successful audit entries appear for the first and third keys

### Requirement: Every mutation attempt leaves an audit trail

The catalog SHALL persist an audit entry for every mutation attempt - authorized or refused, succeeding, already-applied, or failing - carrying the operation kind, target repository and list references, principal, calling source, idempotency key, outcome, time, and a classified failure reason when unsuccessful, and the trail SHALL never contain credential material.

#### Scenario: A refused, a successful, and a failed attempt are all explainable afterwards

- **WHEN** the audit log is read following a refused scope violation, a successful star, and a provider failure in the same batch
- **THEN** three entries exist distinguishing refused, applied, and failed outcomes with their reasons, principals, sources, and times, and none contains token material
