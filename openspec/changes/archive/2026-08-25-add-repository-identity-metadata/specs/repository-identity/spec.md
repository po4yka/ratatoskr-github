## Purpose

Anchors every GitHub repository the catalog knows about to one stable internal identity keyed by GitHub's numeric provider ID, so renames and ownership transfers update aliases instead of creating duplicate logical repositories.

## ADDED Requirements

### Requirement: Stable internal repository identity

The catalog SHALL represent each known GitHub repository exactly once, identified by an internally generated identifier that never changes, and SHALL map it to the provider's numeric repository ID uniquely.

#### Scenario: First observation creates the logical repository

- **WHEN** a repository is upserted for a provider numeric ID that the catalog has not seen
- **THEN** exactly one repository record exists with a fresh internal identifier distinct from the provider numeric ID

#### Scenario: Repeated observation reuses the existing identity

- **WHEN** a repository is upserted again for a provider numeric ID already known
- **THEN** the existing internal identifier is returned unchanged and no second repository record is created

### Requirement: Alias resolution by current name

The catalog SHALL resolve a repository's current `owner/name` alias to its internal repository identity.

#### Scenario: Current alias resolves to its repository

- **WHEN** an alias is looked up by kind and value that was recorded as the live alias of a repository
- **THEN** the lookup resolves to that repository's internal identifier

#### Scenario: Unknown alias resolves to nothing

- **WHEN** an alias value that was never recorded is looked up
- **THEN** the lookup resolves to no repository

### Requirement: Rename evidence redirects aliases

Upon rename evidence observed from provider responses, the catalog SHALL activate the new alias for the same logical repository and SHALL keep the superseded alias resolvable to that same repository.

#### Scenario: Rename moves the live alias while the old name still redirects

- **WHEN** rename evidence arrives for a repository known under an old `owner/name`
- **THEN** the new `owner/name` becomes the live alias of the same internal identity and a lookup of the old `owner/name` still resolves to that same internal identity

#### Scenario: Ownership transfer behaves like a rename

- **WHEN** transfer evidence moves a repository to a different owner while the provider numeric ID stays the same
- **THEN** no second repository record is created and both the pre-transfer and post-transfer aliases resolve to the one internal identity

### Requirement: Live alias uniqueness with historical collision safety

The catalog SHALL permit at most one repository to hold a given live `owner/name` alias at a time, while allowing a superseded name later taken by a different repository to coexist with the earlier repository's historical record.

#### Scenario: Two repositories cannot share one live name

- **WHEN** a second repository attempts to claim an `owner/name` that is currently the live alias of another repository
- **THEN** the claim fails and neither repository's identity changes

#### Scenario: A released name may be claimed by a different repository

- **WHEN** a repository's `owner/name` has been superseded by a rename and a different repository later claims that exact name
- **THEN** the different repository holds it as its live alias and the original repository still resolves through its superseded alias record
