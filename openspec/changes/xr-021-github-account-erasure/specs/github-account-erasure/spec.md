## Purpose

Account erasure removes the owner's data from the GitHub Catalog, collects the shared catalog entries only that owner reached, and answers redelivery with the recorded outcome. Wire behaviour is XR-021 CONTRACTS.md section S12.

## ADDED Requirements

### Requirement: Erasure finds the real payload shapes

Erasure SHALL remove outbox, inbox and publication rows whose payload names the erased owner as `owner`, `tenant_id` or `payload.account`, and every `repository_action_attempts` row of the owner, and SHALL leave other owners' rows and ownerless catalog-wide rows untouched.

#### Scenario: Real producer shapes are erased

- **WHEN** an owner with an analysis request outbox row, completed and failed inbox rows, a stored sync command, a publication and action attempts is erased
- **THEN** none of those rows remain and another owner's rows remain

### Requirement: Only what the owner uniquely reached is collected

Erasure SHALL delete repositories, aliases, metadata, revisions, publications, backup policies and README bytes that no remaining row references, SHALL keep everything another owner references and every repository the owner never touched, and SHALL mark the backup policy dirty when a repository was removed.

#### Scenario: Shared and unrelated repositories remain

- **WHEN** owner A is erased and repository R2 is also starred by owner B and repository R3 was never touched by A
- **THEN** R2, R3 and the bytes R2 references remain while A's unshared repository and its bytes are gone

### Requirement: Acknowledgement is replay-safe

Erasure SHALL record one ledger row atomically with the owner data deletion, SHALL answer a redelivered `operation_id` with the recorded outcome without calling the provider, and SHALL refuse a reused `operation_id` for a different owner.

#### Scenario: Redelivery returns the first answer

- **WHEN** the first erasure answered incomplete and the same operation id arrives again after the credentials are gone
- **THEN** the second answer is incomplete and no provider call is made

#### Scenario: Already-revoked grant converges

- **WHEN** the provider reports the grant as already revoked
- **THEN** the outcome is verified
