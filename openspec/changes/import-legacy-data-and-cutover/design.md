## Context

See [proposal.md](proposal.md) and the two delta specs for the required behaviour. The retired PostgreSQL schema is available locally for inspection, but its 101 MB dump and CSV data are private operator input. Its relevant `public.repositories` columns are `github_id`, `owner`, `name`, `user_id`, `is_starred`, `last_synced_at`, and `list_names`; `public.user_github_integrations` has connection metadata plus `encrypted_token`. It has no provider `starred_at` or native-list node IDs.

The current service has account rows and synchronization logic, but no credential registration. Its existing star projection requires a provider star time, and its native-list projection requires a provider list ID. Neither can be truthfully manufactured from the archive.

## Goals / Non-Goals

**Goals:**

- Provide a bounded, repeatable import from the archived PostgreSQL schema into catalog-owned tables.
- Establish an imported account only after a current-owner mapping and force a fresh PAT re-registration before provider access.
- Keep unknown provider times and native-list IDs honest while preserving the legacy observation evidence needed for shadow comparison.
- Produce deterministic, redacted shadow reports and an owner-gated operational cutover record.

**Non-Goals:**

- Reading, decrypting, copying, or retaining `encrypted_token` or any other legacy credential material.
- A durable foreign-data wrapper, database link, migration, or compatibility layer for legacy tables.
- Importing legacy LLM artifacts, filesystem mirror state, local collections, or the retired product's user identity system.
- Inventing provider star timestamps or provider list identities from `last_synced_at` or a list name.
- Executing external routing changes without the owner's written approval. The current repository has no public read/write router to switch; the checklist is the catalog-side prerequisite and audit artifact.

## Decisions

### D1: Temporary, allow-listed PostgreSQL source

The service binary receives a legacy source connection only from a dedicated, redacted configuration entry; it is not accepted as a command-line value and is never serialized. An explicit `import-legacy` subcommand validates the exact required tables and column types through `information_schema`, then queries a fixed allow-list. The integration query deliberately omits `encrypted_token`.

The operator restores `ratatoskr.dump` into an isolated, read-only source database before invoking the importer. Synthetic tests create a matching minimal source schema in a disposable database; no archive row, dump, or credential ciphertext is committed.

An arbitrary CSV parser was rejected because the accepted archive is PostgreSQL and CSV escaping would widen the untrusted parsing surface. A long-lived FDW/database link was rejected because it would retain the forbidden legacy bridge.

### D2: Explicit owner mapping and current account identity

`--owner-map <path>` names a small JSON mapping from legacy numeric `user_id` to already-existing `user:<uuid>` Platform tenant references. It contains no credential or source connection information. Preflight validates the complete mapping, rejects duplicate or unmapped legacy users, then opens one target transaction per mapped account.

`github_accounts` gains a verified provider user ID, an owner/provider uniqueness constraint, and an import-safe account state. Imported records are `reauthorization_required`; no legacy login, Telegram ID, or username becomes an owner identity. This is preferable to a synthetic `legacy:` tenant, which would violate Platform ownership and create a permanent identity bridge.

### D3: PAT re-registration is a positive, encrypted capability

The initial reconnect surface accepts a fine-grained PAT from standard input only. It calls GitHub's authenticated-user endpoint, captures the verified provider user ID and granted-scope response header, then writes an encrypted, versioned credential and transitions the single matching account to `connected` in one transaction. The token is wrapped in a redacting secret type and never crosses tracing, errors, events, reports, or CLI arguments.

Credential encryption uses authenticated encryption with a configured active key and explicit key version. The key configuration is non-serializable, validated at startup, and never printed. New cryptographic crates are selected only after the locked dependency, source, advisory, license, and code review required by `rust-security`; no home-grown cipher is permitted. OAuth is not added here: PAT reconnection satisfies the immediate import dependency, while an OAuth callback is a separate public-contract change.

### D4: Import evidence remains distinct from provider authority

Import maps `github_id` through the existing stable identity path and records owner/name aliases. A legacy `is_starred` record becomes an imported current-star claim with `last_synced_at` as the source observation time. `provider_starred_at` is nullable for imported unknowns, and a successful full provider snapshot replaces that unknown with provider evidence before cutover can be considered ready.

Legacy `list_names` become catalog-owned import claims keyed by account, repository, and normalized list name, not `star_lists` rows. After re-registration, the normal GraphQL list snapshot supplies stable provider list IDs; shadow comparison joins against the claims by observed list name and membership. This preserves evidence without treating a mutable name as provider identity.

Idempotence is enforced by unique import-source keys and upserts in a target transaction. A failed account transaction leaves its prior catalog state intact and records a redacted failure classification; no partial import becomes cutover-ready.

### D5: Shadow report and cutover are evidence gates

`shadow-legacy` first runs the existing complete catalog synchronization for a reauthorized account, then reads the temporary source and produces a canonical JSON report plus a concise text rendering. The report contains only counts, classifications, owner-independent numeric repository IDs, UUID run IDs, and normalized list names. It never contains source URLs, database connection data, credentials, raw provider bodies, or private repository content.

Clean means: no unresolved identities or owner mappings; every imported account has a completed full star/list snapshot after reconnect; no provider-state/list-membership differences; and no unclassified unknown provider star time. The report is stored as catalog audit evidence and addressed by digest. An owner approval record must bind to that digest and current full-gate evidence.

The repository supplies `docs/cutover/legacy-github-catalog.md`: import, reauthorize, repeat shadow cycles, owner review, read activation, stability window, write activation, and rollback. It does not itself change production routing. Rollback restores the previous external routing while retaining catalog import and report evidence, and it disables new writes before returning reads.

### D6: Current schema only, plus operator telemetry

All tables and constraints are edited in `schema.sql`; no migration framework or migration files are introduced. The importer records bounded import/shadow run statistics and non-sensitive correlation IDs, while operational telemetry uses counts and failure classes rather than repository names, source locations, owners, or token data.

## Risks / Trade-offs

- [Legacy source has no `starred_at` or list node ID] → preserve observation/name claims, require a post-reconnect complete provider snapshot, and fail readiness rather than invent data.
- [Wrong owner mapping attaches private state to another tenant] → validate mapping exhaustively before writes, use Platform tenant syntax, and require owner review before cutover.
- [PAT or encryption key leaks through a diagnostic] → stdin-only token intake, non-serializable/redacting wrappers, narrow stable errors, and regression tests that inspect outputs and persisted rows.
- [A partial source or transient provider failure looks clean] → no source absence authority, no clean report without completed snapshots, and explicit incomplete classifications.
- [Long-lived source access returns] → keep source configuration invocation-scoped; delete neither import evidence nor reports during rollback, but retain no connection data or legacy schema dependency.
- [External routing is not owned here] → treat the owner-approved runbook as a hard prerequisite and do not claim a production cutover until the owner executes the external activation.

## Migration Plan

1. Apply the current schema and deploy the catalog with credential encryption configuration but no legacy source configuration.
2. Restore the archive into an isolated read-only PostgreSQL instance; produce the owner map from current Platform identities.
3. Run the idempotent importer, resolve every reported mapping or identity failure, then repeat the import under the same source label.
4. Have each account holder register a replacement PAT; rejected or pending accounts remain excluded from shadow/cutover.
5. Run at least the documented repeated full star/list shadow cycles and retain their report digests.
6. The owner reviews a clean report and gate evidence, records approval in the checklist, then activates reads externally.
7. After the documented stable read window, the owner separately approves write activation.
8. On any regression, disable catalog writes first, restore prior reads, preserve catalog evidence, and investigate from the report; do not re-import or recreate a legacy bridge.
