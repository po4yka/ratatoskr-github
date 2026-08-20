# Ratatoskr GitHub

`ratatoskr-github` is the GitHub Catalog bounded context for Ratatoskr. It records what repositories a user has starred or chosen to track, preserves GitHub metadata and list membership, coordinates repository analysis, and publishes the desired backup state consumed by Git Vault.

> **Status:** architecture bootstrap. OAuth, synchronization, persistence, APIs, and event handlers described below are planned and are not implemented yet.

> [!IMPORTANT]
> **Ratatoskr is in development.** No database holds data that has to survive a schema change.
> While this status holds, these two rules replace what the documents below plan:
>
> - the API and the database keep their first version. There is no `v2` and no later major
>   version.
> - the database has no migrations. One schema definition exists, and a schema change edits it in
>   place.
>
> Only the repository owner changes this status.

## Role in Ratatoskr

This service answers questions such as:

- Which GitHub accounts are connected?
- Which repositories are currently starred?
- When was a star first observed?
- Which native GitHub star lists contain a repository?
- Which repositories are explicitly tracked without being starred?
- Which README or metadata version was last seen?
- Which repository changes should trigger analysis or notifications?
- What backup policy should Git Vault converge toward?

It does **not** execute `git clone`, manage local disk paths, create bundles, retain LFS objects, or verify restores. Those responsibilities belong to `ratatoskr-vault`.

## Core responsibilities

- GitHub account connection through fine-grained PAT or OAuth where appropriate;
- encrypted credential lifecycle and scope auditing;
- repository identity and mutable aliases;
- starred-repository synchronization;
- full-snapshot reconciliation and observed unstars;
- native GitHub star-list synchronization;
- repository metadata, languages, topics, README metadata, and content hashes;
- manual repository tracking;
- explicit star and list mutations with user consent;
- repository watch policies;
- desired backup policies;
- requests for versioned repository analysis in `ratatoskr-knowledge`;
- provider rate-limit and reauthorization state;
- audit and operation events.

## Repository identity

GitHub's numeric repository ID is the stable upstream identity. `owner/name`, canonical URL, and clone URL are mutable aliases and must not serve as the primary key.

A repository may be:

- manually indexed for metadata;
- explicitly tracked for backup;
- starred through GitHub;
- both manually tracked and starred;
- renamed or transferred;
- archived, deleted, made private, or no longer accessible.

The local model converges these observations on the stable GitHub ID rather than creating duplicate records.

## Planned data model

The service owns a `github_catalog.*` PostgreSQL schema. Expected tables include:

```text
github_accounts
github_credentials
repositories
repository_aliases
star_observations
current_star_state
star_lists
star_list_memberships
repository_watches
backup_policies
sync_runs
sync_checkpoints
rate_limit_state
analysis_references
outbox_events
inbox_events
```

Typical repository metadata includes:

- GitHub ID;
- owner and name aliases;
- canonical and clone URLs;
- description and homepage;
- default branch;
- primary and detailed language data;
- topics;
- star, fork, and watcher counts;
- license identifier;
- archived/fork/template/private flags;
- creation and push timestamps;
- README ETag, hash, and blob reference;
- metadata content hash;
- first and last observation timestamps.

Large README bodies and raw provider responses are stored in the content-addressed BlobStore, not duplicated in operational tables.

## Star synchronization

GitHub star synchronization uses two complementary modes.

### Incremental scan

- fetch starred repositories ordered by `starred_at` descending;
- stop after reaching a persisted high-water mark;
- upsert new and changed repositories;
- refresh metadata without unnecessary analysis;
- never infer an unstar from an incomplete listing.

### Full snapshot

- periodically enumerate the complete starred-repository set;
- record the successful snapshot boundary;
- only after the complete traversal, mark previously starred but absent repositories as no longer starred;
- preserve the time as `observed_unstarred_at`, because the exact upstream removal time may be unknown.

The central invariant is:

> Absence from a partial scan proves nothing. Only a successful full snapshot can establish removal.

This protects the catalog from false unstars caused by pagination errors, rate limits, transient authorization failures, or interrupted jobs.

## Native star lists

GitHub star lists are provider-owned organization, separate from Ratatoskr collections and tags.

The service mirrors:

```text
star_lists
star_list_memberships
```

List reconciliation is independent from the REST starred-repository scan and may use GitHub GraphQL. A list-sync failure must not invalidate an otherwise successful star snapshot.

Native list membership is treated as derived upstream state. Local Ratatoskr collections remain owned by the appropriate product context and must not be overwritten by GitHub reconciliation.

## Adding repositories

The API and Telegram flows preserve three distinct modes:

| Mode | Local catalog | GitHub star | Git backup |
|---|---:|---:|---:|
| `metadata` | Yes | No | No |
| `track` | Yes | No | Yes |
| `star` | Yes | Yes | Optional by policy |

A pasted GitHub URL defaults to the safe `metadata` path. External writes require an explicit command or confirmed UI action.

The operation runs in increasing order of user-visible commitment:

1. fetch and store metadata;
2. optionally star the repository;
3. optionally update native list membership;
4. publish desired backup state.

A later failure does not roll back an earlier successful external action. Responses report truthful partial success with warnings rather than pretending the entire workflow was atomic.

## Backup policy

GitHub Catalog stores desired state; Git Vault owns actual storage state.

Planned policy levels:

```text
none
metadata_only
git_mirror
git_mirror_with_lfs
complete_archive
```

Additional policy attributes may include:

```text
pinned
retention_policy
include_wiki
include_releases
include_issues
offsite_required
```

Policy changes publish events such as:

```text
vault.target.desired.v1
```

Catalog never inspects Vault's filesystem or writes its database. Vault reports convergence and verification through contracts.

## Repository analysis

Repository analysis belongs to `ratatoskr-knowledge`.

When metadata or README content changes, Catalog may publish:

```text
knowledge.repository_analysis.requested.v1
```

Knowledge returns a versioned result containing purpose, technology stack, architectural summary, concepts, use cases, target audience, maturity, dependencies, confidence, and hallucination-risk fields. Catalog stores only the accepted analysis reference and user-facing projection required for its API.

Changes to star lists or backup policy do not by themselves change repository content and must not trigger expensive reanalysis.

## Repository watches

A watch policy may monitor:

- README content hash;
- new releases;
- newly opened issues;
- repository archival or deletion;
- default-branch or visibility changes;
- selected activity thresholds.

The first observation establishes a baseline without notifying. Later monotonic or hash changes emit deduplicated events. Watch synchronization remains bounded by account rate limits and per-repository policy.

## Credentials and authorization

Credentials are owned exclusively by this service.

Requirements:

- encrypted access tokens with versioned key rotation;
- no token values in logs, traces, API responses, or events;
- precise recording of granted scopes and account identity;
- support for reauthorization and explicit revoke;
- separate consent for mutations;
- provider error classification;
- rate-limit reset persistence;
- bounded concurrency and conditional requests.

A fine-grained token with minimal repository access is preferred where it satisfies the use case. OAuth device or web flows may be added behind the same account-connection boundary.

Telegram, Platform, Knowledge, and Vault never receive the plaintext GitHub token.

## Commands and events

Expected contracts include:

```text
github.account.connected.v1
github.account.reauth_required.v1
github.sync.requested.v1
github.sync.completed.v1
github.repository.observed.v1
github.repository.changed.v1
github.repository.star_requested.v1
github.repository.starred.v1
github.repository.unstar_observed.v1
github.star_lists.reconciled.v1
github.backup_policy.changed.v1
```

All consumers are idempotent under at-least-once delivery. Commands that can mutate GitHub require a user principal, explicit authority, and idempotency key.

## API surface

The public client reaches this service through `ratatoskr-platform`. Planned use cases include:

- connect, inspect, and revoke a GitHub account;
- list and filter catalog repositories;
- inspect repository details and analysis status;
- add a repository in `metadata`, `track`, or `star` mode;
- manage native star lists where authorized;
- configure watch and backup policies;
- trigger a manual sync;
- inspect sync, rate-limit, and reauthorization status.

Internal provider details are not exposed as stable public contracts.

## Security invariants

1. GitHub credentials remain inside this bounded context.
2. A pasted repository URL never implies consent to star or change provider state.
3. Every mutation is explicit, authenticated, idempotent, and audited.
4. Partial listings never establish removal.
5. Manual `pinned` backup intent is never erased by later star-state changes.
6. Catalog does not execute Git or access Vault storage.
7. Private repository metadata is protected by the same tenant and access boundary as its credential.
8. Provider response data is treated as untrusted input.

## Observability

Core metrics include:

```text
github_sync_duration
github_sync_repositories_seen
github_full_snapshot_age
github_false_removal_guard_skips
github_rate_limit_remaining
github_rate_limit_waits
github_conditional_not_modified
github_metadata_changes
github_analysis_requests
github_mutation_failures
github_reauth_required
```

Every sync run records mode, cursor/high-water mark, pages completed, completeness, warnings, and resulting state transitions.

## Non-goals

- Bare mirrors, bundles, LFS, disk paths, retention, or restores.
- General Git hosting.
- LLM execution or vector search.
- Ratatoskr collection ownership.
- Automatic provider mutations without explicit user consent.
- Treating repository names as stable identities.
- Inferring exact unstar timestamps when GitHub does not provide them.

## Initial milestones

1. Establish account, credential, repository, and sync schemas.
2. Implement minimal PAT validation and encrypted storage.
3. Implement incremental and full starred-repository snapshots.
4. Add metadata and README conditional retrieval.
5. Add native star-list reconciliation.
6. Add manual `metadata`, `track`, and `star` workflows.
7. Publish desired backup policy to Git Vault.
8. Integrate repository analysis with Knowledge.
9. Add watches, rate-limit diagnostics, and shadow comparison with the legacy system.

## Workspace integration

`ratatoskr-workspace` pins Catalog with compatible contracts, Vault, Knowledge, Telegram, Platform, and client commits. Cross-repository changes are coordinated through changesets; this repository remains independently buildable and testable using recorded GitHub fixtures and mock servers.

## Project status

This README defines the intended GitHub Catalog architecture. It does not claim that account connections, synchronization, mutations, or database models already exist.
