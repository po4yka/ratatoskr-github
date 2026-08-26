# Design: add-repository-modes-and-mutations

## Context

Read paths are complete: identity, metadata, snapshots, incremental scans, and list authority all work against a disposable-database test harness that mounts `ReqwestGithubApi` on wiremock servers. No write path exists, no audit table exists, and `outbox_events` has no producer. The retired monolith (`~/GitRep/ratatoskr-repositories/ratatoskr`) performed these exact writes in production through GitHub GraphQL: documented `addStar`/`removeStar` and undocumented-but-public-schema `updateUserListsForItem` (see `app/adapters/github/github_graphql_client.py`, lines 115-179), requiring the `user` OAuth scope for list writes and satisfied by `repo`/`public_repo` for stars.

Development status rules apply: schema changes edit `schema.sql` in place; no v2; product is Ratatoskr.

## Goals / Non-Goals

**Goals:**

- A mode vocabulary on repository rows with validated transitions and an audit record per accepted transition.
- One authorization-context type every mutation requires; scope and connection enforcement before any provider call.
- Idempotent star/unstar/list-membership execution whose retries converge to one end state and one successful audit row.
- Per-operation truthful outcomes for batches, including partial success.

**Non-Goals:**

- Confirmation UX (telegram/web own it); this service consumes the resulting context.
- Outbox event publication (no consumers exist yet; subjects stay reserved in the schema whitelist).
- Metrics emission (no emitter infrastructure exists; introducing the first emitter is its own decision).
- Backup-policy or watch coupling (items 8-9).
- Creating, renaming, or deleting lists upstream (item 6 explicitly deferred write-back of list identity itself).

## Decisions

### D1: Mode lives as a nullable column on `github_catalog.repositories`

`mode text check (mode in ('auto','tracked','ignored'))`, null = unclassified. Alternative: a per-account mode table. Rejected because the established single-catalog modeling already keys intent per repository globally (`backup_policies.repository_id` is unique), and modes gate exactly those catalog-level concerns.

### D2: Transition matrix enforced in one domain function

Explicit requests accept only `tracked` | `ignored`; `auto` is reachable solely through star effects. Unstar maps `auto` back to unclassified; `tracked` survives both directions; `ignored` requires unstarred state; synchronization promotes only unclassified to `auto`. Alternatives considered: richer states (per-account modes, pending confirmations) - rejected as speculative until item 8/9 need them; free-form transitions with post-hoc audit - rejected because acceptance demands transition validation.

### D3: Provider mutations via a separate `MutationApi` trait in a new module

`provider.rs` holds 725 of 850 allowed lines. The four operations (`fetch_repository_node_id`, `star_repository`, `unstar_repository`, `set_repository_lists`) go to `provider_mutations.rs` as their own AFIT trait implemented by `ReqwestGithubApi`, mirroring how tests mount wiremock responses today. Wire shapes copy legacy's proven GraphQL documents:

- `mutation($starrableId: ID!) { addStar(input: {starrableId: $starrableId}) { starrable { ... on Repository { databaseId viewerHasStarred } } } }`
- `removeStar` symmetric.
- `mutation($itemId: ID!, $listIds: [ID!]!) { updateUserListsForItem(...) { lists { id } } }`

Starring needs the repository's GraphQL node id, resolved at mutation time via `repository(owner:, name:) { id }` (legacy `_REPO_NODE_ID_QUERY`), since the catalog stores REST database ids only.

### D4: List membership writes are read-modify-write against live provider state

`updateUserListsForItem` replaces the item's whole set. Before writing, the executor queries the repository node's live `lists(first: 100)` membership, computes desired = live ± target, writes the full set, and records the computed set in the audit detail. Alternative: derive desired from local `star_list_memberships` authority - rejected because local state can lag upstream renames/filings made outside Ratatoskr, and silently dropping such memberships repeats the exact hazard legacy documented ("an opportunity to silently move it somewhere else"). The extra read is one budgeted request per operation.

### D5: Idempotency = partial unique index over successful audit rows

`mutation_audit.idempotency_key` carries `create unique index ... where outcome in ('applied', 'already_applied')`. Executor flow per operation: select by key; hit returns the stored outcome without touching the provider (fast-path replay); miss executes; insert after success - a racing duplicate loses on the unique index and reads back the winner's row. Failed attempts never occupy the key, so retry-after-failure re-executes (safe: provider operations are repeat-safe). Alternatives: claim-first inbox pattern - rejected because a crash between claim and execution would strand a pending claim with no outcome; deduplicating only in-memory - rejected as non-durable.

### D6: Authorization context and scope enforcement mirror legacy requirements

`MutationContext { account_id, principal, source, }` plus per-operation idempotency keys. Enforcement order: account resolves and is `connected`, then granted scopes satisfy the capability - star/unstar accepts any of `repo`, `public_repo`; list writes require `user`. `github_accounts.granted_scopes text[] not null default '{}'` is added now; credential flows populate it in item 2, tests seed it directly. Refusals are audited with outcome `rejected` before any provider contact. Alternative: defer all scope checking to item 2 - rejected because the task scope explicitly assigns enforcement to this service.

### D7: Audit as one table serving transitions and mutations

`github_catalog.mutation_audit`: `audit_id`, unique-indexed `idempotency_key`, `account_id`, `repository_id`, nullable `list_id`, `operation_kind` (`star|unstar|list_member_add|list_member_remove|mode_set`), `principal`, `source` (`telegram|web`), `outcome` (`applied|already_applied|rejected|failed`), `detail jsonb` (from/to mode, desired list set, classified failure reason), `created_at`. One table keeps "who/what/when" greppable in one place; alternatives (separate transition-audit table) duplicate the shape for no query benefit today. `account_id` deliberately carries no foreign key: a refused attempt names an account that may not exist, and the trail records claims rather than vouching for them.

### D8: Batch semantics are independent execution plus stored-outcome replay

Each operation runs alone under one transaction for its audit row and any mode/star-state effect; a failure marks only its own outcome. Resubmission short-circuits succeeded keys via D5's fast path. This matches legacy property 3 ("one repository's failure does not end the run") and the README's no-rollback-of-external-actions rule.

### D9: Mutation-established stars carry no provider starred-at and no scan observation

`addStar` returns only `viewerHasStarred`; GitHub supplies no star creation time for self-inflicted stars, and the `star_observations` check demands provider evidence that does not exist here. The projection therefore records `starred_at = statement time` - accurate, since the service performed the action itself - clears `observed_unstarred_at`, and skips `star_observations`: the audit row is the append-only evidence for external actions, while scan observations remain evidence for synchronization listings. Outcome truthfulness is relative to known local state: locally already-starred plus a confirming reply reports `already_applied` with untouched timestamps; anything else reporting a starred confirmation is `applied`. Unstars mirror the same mapping with `observed_unstarred_at = now()`.

### D10: Refusal audits keep their account claim without a foreign key

The audit trail records attempts as claimed, including claims naming accounts that do not exist, so `mutation_audit.account_id` carries no foreign key; the target repository is resolved-or-upserted so every attempt references a real catalog identity.

## Risks / Trade-offs

- [GitHub's list mutations are undocumented and may drift] → Wire shapes pinned by committed wiremock fixtures asserted through the real adapter; unknown GraphQL error classes map to `failed` with zero local change; the reliance is recorded here and in the proposal.
- [Replace-semantics race: external list edit between live-read and write] → Window minimized to two sequential requests; the audit detail records the desired set actually written, making any clobber forensically visible; acceptable for single-user deployment scale.
- [Scope vocabulary differs across classic PATs and future fine-grained/OAuth tokens] → Capability checks accept explicit accepted-scope sets now; item 2 owns normalizing granted scopes from real token introspection.
- [Mode promotion touches hot sync paths] → Promotion is one guarded helper called at existing star-upsert points; existing invariant suites (partial scans never remove, snapshot authority) must stay green, guarding against regressions.
- [Audit grows unbounded] → Append-only by design (evidence rule); volume is human-scale mutations, not sync observations; revisit with retention policy when event publication lands.

## Migration Plan

None required by development status: `schema.sql` edits in place and disposable test databases rebuild from it. Rollback is branch-level (feature branch merged only after the full gate).

## Open Questions

None blocking. Scope-name normalization for fine-grained PATs is deliberately deferred to item 2 without changing specs or tasks here.
