## Purpose

Publishes the catalog's explicit preservation intent as a versioned policy for Vault and retains Vault's independently reported decision without claiming that preservation succeeded.

## ADDED Requirements

### Requirement: Catalog derives a complete mirror policy from governed catalog state
The Catalog SHALL derive every emitted `DesiredBackupPolicy` using the shared `ratatoskr-backup-contracts` v1 type. It SHALL include each `auto` or `tracked` repository whose desired level requires a Git mirror, omit `ignored`, `none`, and `metadata_only` entries, and use the catalog-owned cadence, priority, optional byte-size hint, and explicit exclusions. The policy SHALL use catalog repository identifiers, not mutable GitHub aliases, and SHALL not contain credentials, clone URLs, retention decisions, or claimed actual backup state.

#### Scenario: A governed repository becomes one typed policy entry
- **WHEN** an `auto` or `tracked` catalog repository has a mirror-requiring desired policy with a cadence, priority, size hint, and exclusions
- **THEN** the derived document contains exactly one matching stable repository entry with those values

#### Scenario: Non-mirror catalog state does not widen Vault scope
- **WHEN** the catalog contains ignored, metadata-only, none, or unclassified repositories alongside a governed mirror repository
- **THEN** the document contains only the governed mirror repository

### Requirement: Publication versions advance only for a changed desired state
The Catalog SHALL serialize policy publication and assign a strictly increasing positive document version only when the complete derived desired state differs from the last published state. It SHALL persist the policy and its outbox record atomically, so a visible version always has exactly one corresponding `cmd.vault.target.desired.v1` request and a duplicate worker attempt cannot create another version.

#### Scenario: A changed policy receives the next version
- **WHEN** a first derived desired state is published and a later catalog change changes its entries
- **THEN** the later publication has a larger positive version and one new desired-policy request

#### Scenario: An unchanged policy is idempotent
- **WHEN** reconciliation runs again without a change to its derived desired state
- **THEN** it creates neither a new version nor another desired-policy request

### Requirement: Catalog coalesces policy reconciliation after catalog changes
The Catalog SHALL record catalog changes that can affect desired backup scope and defer publication until the configured debounce window has elapsed. Several changes during one window SHALL yield at most one reconciliation run, derived from the state at execution time; the worker SHALL be safe to retry after a committed outcome.

#### Scenario: Burst changes publish once after the trailing debounce window
- **WHEN** two policy-affecting catalog changes occur before the debounce window expires
- **THEN** no policy request exists before the trailing deadline and one request exists after it

### Requirement: Vault acknowledgment feedback remains auditable and idempotent
The Catalog SHALL consume `evt.vault.backup_policy.acknowledged.v1` through its inbox using the shared typed acknowledgment payload. It SHALL retain the acknowledged version, outcome, prior Vault-applied version, and machine-actionable reasons for operator visibility; duplicate deliveries SHALL not duplicate feedback. An accepted acknowledgment SHALL be recorded as Vault's decision only, never as evidence that a backup, retention action, or restore completed.

#### Scenario: A rejection remains visible with its reason
- **WHEN** Vault acknowledges a policy version as rejected with a valid rejection reason
- **THEN** the feedback projection exposes that version, rejected outcome, previous applied version, and reason exactly once

#### Scenario: A redelivered acknowledgment is harmless
- **WHEN** the same valid acknowledgment message is delivered twice
- **THEN** the second delivery reports a duplicate and leaves one feedback record
