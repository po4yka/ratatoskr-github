## Purpose

Lets a user safely reconnect an imported GitHub account so the catalog can verify identity and scopes without ever recovering a legacy credential.

## ADDED Requirements

### Requirement: PAT re-registration verifies identity before activation
The catalog SHALL accept a replacement GitHub PAT only through a non-command-line secret input, verify it with GitHub before it changes account state, and bind the resulting provider user identity and observed scopes to the requesting current owner identity. A successful re-registration SHALL transition only the matching imported account from `reauthorization_required` to `connected`; an invalid token, an owner mismatch, or an ambiguous imported account SHALL leave all catalog state unchanged.

#### Scenario: Valid PAT reconnects the imported account
- **WHEN** the owner supplies a valid replacement PAT for one imported account
- **THEN** the catalog records the verified GitHub user identity and observed scopes, marks that account connected, and enables its ordinary synchronization

#### Scenario: Invalid PAT leaves account reauthorization-required
- **WHEN** GitHub rejects the supplied replacement PAT
- **THEN** the catalog reports a stable authentication failure, keeps the imported account reauthorization-required, and stores no replacement credential

#### Scenario: PAT is not accepted on the command line
- **WHEN** an operator invokes credential registration with a token-bearing command-line argument
- **THEN** the invocation is rejected before contacting GitHub and the token is not included in diagnostics

### Requirement: Credentials are encrypted, versioned, and redacted
The catalog SHALL encrypt every accepted replacement credential with the active configured key version before persistence, retain only the encryption metadata needed to decrypt it inside this service, and exclude plaintext, ciphertext, authorization headers, and key material from logs, reports, errors, events, fixtures, and serialized configuration.

#### Scenario: Stored credential has no plaintext representation
- **WHEN** a PAT registration succeeds
- **THEN** the persisted credential is encrypted and versioned, and normal account, synchronization, audit, and operator outputs contain neither the token nor its ciphertext

#### Scenario: Key rotation preserves reconnect semantics
- **WHEN** a configured replacement encryption key becomes active
- **THEN** the next successful credential registration stores the active key version while previously registered credentials remain distinguishable by their recorded version

### Requirement: Import uses an explicit current-owner mapping
The legacy importer SHALL require an operator-provided mapping from each imported legacy user identifier to one current Platform owner identity, reject unmapped or duplicate mappings before target writes, and use that current identity for all imported account-scoped state. It SHALL not treat a legacy Telegram user ID, username, or GitHub login as a current owner identity.

#### Scenario: Unmapped legacy user blocks import
- **WHEN** the legacy source contains an account-scoped repository or integration whose legacy user identifier is absent from the owner mapping
- **THEN** the import terminates without catalog writes for that source and reports only the unmapped legacy identifier category

#### Scenario: Imported account begins disconnected from GitHub
- **WHEN** a mapped legacy integration is imported
- **THEN** its account connection record carries the mapped current owner and reauthorization-required state, without a credential record or activated provider access
