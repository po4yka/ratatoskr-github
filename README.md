# Ratatoskr GitHub

`ratatoskr-github` is the GitHub Catalog bounded context for Ratatoskr. It records what repositories a user has starred or chosen to track, preserves GitHub metadata and list membership, coordinates repository analysis, and publishes the desired backup state consumed by Git Vault.

> **Status:** implementation plan items 1 and 3 through 8 are complete: a Rust service runs locally with typed strict configuration, structured telemetry, operator health routes (`/live`, `/ready`, `/metrics`, `/version`), stable repository identity and star/list synchronization, audited repository modes and idempotent provider mutations, and a durable desired-backup-policy publication cursor. Catalog derives a complete `DesiredBackupPolicy` v1 with stable repository references, cadence/priority/size hints and exclusions, publishes only changed versions through `cmd.vault.target.desired.v1`, and records `evt.vault.backup_policy.acknowledged.v1` idempotently for operator visibility. Account credentials, public APIs, and live fleet-bus handlers remain planned.

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
- incremental scans bounded by a watermark, full-snapshot reconciliation, and observed unstars;
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

## Current foundation schema

The service owns a `github_catalog.*` PostgreSQL schema. Its tables encode the
identities and constraints already decided by the bounded context:

```text
github_accounts
repositories
repository_aliases (live/superseded status, redirect history)
repository_metadata
repository_metadata_revisions
star_observations
current_star_state
star_lists
star_list_memberships
repository_watches
backup_policies
sync_runs
sync_checkpoints
outbox_events
inbox_events
```

Repository identity, aliases, metadata projection with conditional requests,
per-token rate-limit accounting, and bounded revision history are implemented.
Credential storage, rate-limit persistence, analysis references, and the behavior behind the remaining placeholder tables remain planned.

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

- fetch starred repositories ordered by `starred_at` descending (`sort=created&direction=desc`);
- ingest exactly the items strictly newer than the account's persisted high-water mark, upserting identity and star state without touching anything else;
- stop once an item at or below the watermark proves the rest of the listing was already covered, or when the provider reports exhaustion;
- advance the watermark to the oldest ingested timestamp only after durable success; a completed full snapshot re-anchors it to its newest observation;
- never infer an unstar from an incomplete listing;
- treat a missing or unparsable `starred_at`, or any increase in the `starred_at` sequence across pages - including across a resumed run, whose ordering boundary travels in the checkpoint - as a gap: the run fails with the reason recorded and a full rescan is required;
- defer to a full snapshot when no baseline exists yet.

### Full snapshot

- periodically enumerate the complete starred-repository set;
- record the successful snapshot boundary;
- only after the complete traversal, mark previously starred but absent repositories as no longer starred;
- preserve the time as `observed_unstarred_at`, because the exact upstream removal time may be unknown;
- record each drift repair explicitly - `unstar_after_drift` for locally starred but absent, `restore_after_miss` for locally unstarred but listed again - inside the same atomic swap transaction, keyed so repeating reconciliation on converged state records nothing and changes nothing.

The central invariant is:

> Absence from a partial scan proves nothing. Only a successful full snapshot can establish removal.

This protects the catalog from false unstars caused by pagination errors, rate limits, transient authorization failures, or interrupted jobs.

### Scheduled synchronization

Synchronization runs on a schedule owned by the platform scheduler. The scheduler publishes this service's sync commands to `cmd.github.sync.requested.v1` using the platform command grammar (see platform ADR-0005 and the scheduler architecture notes); the catalog validates the envelope strictly, claims the command durably in `inbox_events` keyed by the command identity so at-least-once redelivery performs no second effect, dispatches the payload's requested mode (`incremental` by default, `full` for periodic reconciliation), and escalates an ordering gap into an immediate full rescan.

This service implements no registration API. Schedules are registered through the mechanism platform documents: an operator inserts disabled rows into platform's schedule table following platform's published statement form (see `ratatoskr-platform/deploy/README.md`, "Registering a schedule"), then enables them explicitly. This repository's two schedules:

```sql
-- Frequent incremental sync (every 5 minutes), created disabled.
insert into operations.schedules
    (owner_user_id, subject, payload, schedule_expression, enabled)
values
    (<user uuid>, 'github.sync.requested.v1', '{"account": "<github-login>"}',
     '0 */5 * * * *', false);

-- Periodic full reconciliation (daily at 04:30), created disabled.
insert into operations.schedules
    (owner_user_id, subject, payload, schedule_expression, enabled)
values
    (<user uuid>, 'github.sync.requested.v1', '{"account": "<github-login>", "mode": "full"}',
     '0 30 4 * * *', false);

-- Enable both after verification.
update operations.schedules set enabled = true
where subject = 'github.sync.requested.v1' and owner_user_id = <user uuid>;
```

Column names follow platform's documented schema; if platform's statement form changes, platform's documentation wins over this example.

## Native star lists

GitHub star lists are provider-owned organization, separate from Ratatoskr collections and tags.

The service mirrors:

```text
star_lists
star_list_memberships
```

Lists are read through GitHub GraphQL only - REST v3 offers no list endpoints - via `User.lists` with each list's item connection requested inline. Enumeration pages stage durably under cursor checkpoints, and one atomic transaction promotes a completed enumeration into list authority: renames propagate, staged pairs become members, absent pairs become evidenced removals, and lists that vanished upstream are tombstoned with an inferred observation time instead of deleted.

The same invariant governs lists as stars:

> Absence from a partial enumeration proves nothing. Only a successful complete enumeration can establish membership removal or a removed list.

A list whose membership exceeds one provider page is a truncated enumeration: the run fails naming the truncated list and authority stays untouched. The provider supplies no per-item added-at timestamp, so membership timing records observation times only. Membership diffs are recorded as append-only observations bound to the completing run, so repeating reconciliation on converged state adds confirmations but no second removal evidence.

List reconciliation is independent from the star snapshot: every handled sync command refreshes both authorities and reports each outcome separately, so a list-sync failure never invalidates an otherwise successful star snapshot and vice versa. Star state and list state are independent dimensions - starred but unlisted, listed but unstarred, listed but never star-observed, and every other combination are representable and truthful; a list snapshot never creates, alters, or removes any star state.

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

Catalog uses `ratatoskr-backup-contracts` at immutable commit `0d6ddfb475fd47a153a03a69222a5a27cc48e067`. A durable trailing debounce coalesces mode/star-governance changes, then atomically writes an immutable policy version and `cmd.vault.target.desired.v1` outbox row. Vault feedback arrives as `evt.vault.backup_policy.acknowledged.v1`; accepted means only that Vault accepted the requested policy version, not that a mirror, retention action, or restore succeeded.

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

Policy changes publish commands such as:

```text
cmd.vault.target.desired.v1
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

A fine-grained token with minimal repository access is preferred where it satisfies the use case. OAuth credentials remain behind the same account-connection boundary.

### OAuth grant-revocation configuration

GitHub Catalog can revoke an OAuth application's grant during account erasure
only when this service has both of the following deployment settings:

```text
RATATOSKR__GITHUB_OAUTH__CLIENT_ID=<GitHub OAuth app client ID>
RATATOSKR__GITHUB_OAUTH__CLIENT_SECRET=<service secret reference>
```

Set both values or neither: partial configuration fails startup. The client ID
is a non-secret application identifier; the secret reference must resolve only
inside GitHub Catalog to `RATATOSKR__GITHUB_OAUTH__CLIENT_SECRET`. Do not place
either setting in browser, Platform, event, telemetry, or user-token
configuration. On an OAuth credential issued by another app, a PAT, a missing
OAuth configuration, or a provider error, local erasure still proceeds and the
acknowledgement truthfully reports incomplete external grant revocation.

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

The implemented foundation exports only:

```text
github_catalog_process_info
```

Sync, snapshot, rate-limit, mutation, and analysis metrics remain planned with the behavior that
would emit them.

## Non-goals

- Bare mirrors, bundles, LFS, disk paths, retention, or restores.
- General Git hosting.
- LLM execution or vector search.
- Ratatoskr collection ownership.
- Automatic provider mutations without explicit user consent.
- Treating repository names as stable identities.
- Inferring exact unstar timestamps when GitHub does not provide them.

## Implementation plan

The authoritative sequence is [`docs/IMPLEMENTATION_PLAN.md`](docs/IMPLEMENTATION_PLAN.md). Items 1
and 3 (service foundation; repository identity and metadata), item 4 (full star snapshots with
atomic authority, checkpoints, and evidenced unstars), item 5 (watermark-governed incremental
scans with gap-forced rescans, recorded drift repairs, and platform-scheduler command consumption),
item 6 (native star-list snapshots over GraphQL with atomic list authority, evidenced
membership observations, tombstones, and truncation refusal), and item 7 (repository modes with
validated audited transitions, plus consent-carrying idempotent star/list mutations under an
authorization context with truthful partial-success reporting) are implemented. Items 2 and 8
through 10 remain planned.

## Workspace integration

The planned `ratatoskr-workspace` topology will pin Catalog with compatible contracts, Vault,
Knowledge, Telegram, Platform, and client commits. No workspace pin or GitHub-to-Vault integration
profile exists yet. Cross-repository changes are coordinated through changesets; this repository
remains independently buildable and testable.

## Project status

The process foundation (configuration, telemetry, operator health, owned schema), repository identity with metadata, full snapshots, incremental scans with scheduled reconciliation via consumed sync commands, native star-list snapshots chained independently onto every commanded sync, repository modes with audited validated transitions, and authorized idempotent star/list mutations with truthful partial-success outcomes are implemented and gated by CI. Account connections (credential storage), public APIs, watches, and event publication do not exist yet; those sections above describe the intended GitHub Catalog architecture. Mutation authorization reads granted scopes recorded on the account; credential flows populate them in item 2.
