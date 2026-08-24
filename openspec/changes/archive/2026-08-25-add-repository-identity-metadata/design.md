# Design: add repository identity, mutable aliases, metadata, and conditional requests

## Context

The foundation provides a two-crate workspace (`crates/catalog` domain library, `services/catalog` binary), an editable first-version `schema.sql`, disposable-database test support on `GITHUB_CATALOG_TEST_DATABASE_URL`, and strict workspace lints (no `unwrap`/`expect`/`panic`, `missing_docs`, 850-line file ratchet). The `repositories` and `repository_aliases` placeholder tables already encode identity intent but no behavior exists. Plan item 2 (credentials) has not landed in this tree, so nothing here may depend on account/credential code.

## Goals / Non-Goals

Goals:

- One logical repository per GitHub numeric ID, addressable through aliases that survive renames.
- Metadata refreshes that cost one cheap round trip when nothing changed.
- One rate-limit budget per token shared by all operations in the process.
- Every behavior pinned by a failing-first test; provider payloads pinned by committed fixtures.

Non-Goals:

- Star observations, snapshots, unstar semantics (item 4); scheduled scans and checkpoints (item 5).
- Persisting rate-limit state to the database (arrives with sync workers that schedule across restarts).
- Credential storage, token decryption, account lifecycle (item 2's surface).
- README/content hashing, analysis requests, public HTTP API changes.

## Decisions

### D1. Identity lives in the domain crate as persistence-backed operations, not a repository "entity object"

`crates/catalog/src/identity.rs` exposes `upsert_repository(provider_repository_id) -> RepositoryIdentity` (returns the internal UUIDv7 `repository_id`, creating the row once), `record_alias(repository_id, kind, value) -> AliasResolution`, `apply_alias_observation(...)` for rename evidence, and `resolve_alias(kind, value) -> Option<Uuid>`. Rationale: the bounded context's invariants (unique provider ID, alias redirect) are database constraints plus transactional upserts; a richer aggregate adds nothing yet. Alternative considered: an in-memory identity map — rejected because durability is the point of identity.

### D2. Redirect semantics via alias status + supersession columns and a partial unique index

`repository_aliases` gains `status text not null default 'active'` checked against `('active', 'superseded', 'released')` and `redirect_to uuid null references github_catalog.repository_aliases (alias_id)` naming the alias that superseded it. The old whole-table-style uniqueness `(repository_id, alias_kind, alias_value)` is replaced by a partial unique index on `(alias_kind, alias_value) where status = 'active'`: exactly one live holder per value globally, while historical rows stay resolvable. Rename application runs in one transaction: insert-or-reactivate the new alias as `active`, mark the old row `superseded` with `redirect_to` pointing at the new alias. Resolution prefers an `active` row and falls back to the most recent non-active row for that value — that fallback IS the redirect. Name reuse by another repository then works without erasing history. Alternative considered: deleting old aliases — rejected, it destroys the redirect and violates "aliases update without creating new logical repositories".

### D3. Metadata projection plus raw revisions, deduplicated by content hash

New tables `github_catalog.repository_metadata` (projection: `description`, `language`, `stargazers_count bigint`, `topics jsonb`, `default_branch`, `pushed_at timestamptz`, plus conditional state `provider_etag`, and `content_hash`, `fetched_at`) and `github_catalog.repository_metadata_revisions` (`revision_id`, `repository_id`, `payload jsonb`, `content_hash`, `observed_at`). A revision is appended only when the SHA-256 content hash differs from the current projection hash; `304` and identical-body `200`s therefore never grow history (AGENTS.md: 304 reuses the known prior body). History bound: keep the 10 most recent revisions per repository, enforced by deleting older rows inside the append transaction. Bound is a module constant, not configuration — configuration stays finite until a caller needs to tune it. Topics stored as `jsonb` array to match the schema's existing JSON usage; sqlx maps it via `serde_json::Value`.

### D4. Provider gateway: reqwest client with redirects disabled behind a seam trait

Add pinned `reqwest` (rustls, json, no default features) to `crates/catalog`. The client sets `redirect(Policy::none())` so renamed repositories surface as `301` instead of being silently followed — rename observation requires seeing the redirect. A trait `GithubApi` (`fetch_repository(owner, name, etag)`) is the seam; `ReqwestGithubApi` implements it over HTTP, tests use hand-written fakes plus wiremock. Provider types (`ProviderRepositoryBody`, validators, outcomes `Fresh`/`NotModified`/`MovedPermanently{owner,name}`) live only in this module boundary. Rename evidence forms: `301` yields `MovedPermanently` parsed from `Location`; a `200` body whose `full_name` differs from the requested alias yields the payload plus observed rename evidence.

### D5. Rate-limit ledger: in-memory, keyed by opaque token reference, reserve floor + cooldowns

`crates/catalog/src/rate_limit.rs`: `RateLimitLedger` (`Arc`-shareable) mapping `TokenRef` → bucket `{ remaining, limit, reset_at, cooldown_until }`. `TokenRef` is a caller-chosen opaque handle (e.g., account UUID or label) — never the secret. `acquire(token)` returns a permit or `RateLimited { retry_at }`; `observe(token, headers)` ingests `x-ratelimit-limit/remaining/reset` and `Retry-After`. Enforcement: refuse when `remaining <= RESERVE_FLOOR` (constant 1) while now < reset_at, or while now < cooldown_until. Time comes from `std::time::Instant` captured internally, so tests construct ledgers with pre-seeded buckets rather than sleeping. Shared across operations by construction: callers pass one `Arc<RatedGateway>` around. Persistence deferred (see Non-Goals).

### D6. Observe flow ties the pieces together

`crates/catalog/src/observe.rs`: `observe_repository(gateway, ledger, database, token_ref, owner, name)` resolves/creates identity via aliases, acquires rate budget, fetches conditionally, applies rename evidence when present (then refetches at the new location), records rate headers, and writes projection/revisions only for fresh bodies. Returns a structured outcome (`Observed { .. } | NotModified | MovedTo(..) | RateLimited { .. }`) — partial-success style, no swallowed errors.

### D7. Fixtures are synthetic recorded-shaped JSON served by wiremock and read directly by golden tests

`crates/catalog/tests/fixtures/repos/*.json` hold redacted synthetic GitHub-shaped bodies (never personal data), each paired expected-normalization asserted by a golden test reading the fixture from disk. Wiremock mounts the same fixtures for HTTP-level conditional-request and redirect tests. No mocking crate beyond wiremock's HTTP server (which replaces a real peer, not our code).

## Risks / Trade-offs

- [In-memory ledger forgets budgets on restart] → Acceptable this item: worst case one extra burst before headers arrive again; persistence lands with sync workers.
- [Partial unique index allows duplicate inactive rows for one value] → Intended (history), resolution orders by recency; active uniqueness is what callers rely on.
- [SHA-256 over normalized payload JSON] → Field-order stable because we serialize our own normalized struct, not raw bytes.
- [wiremock adds dev-dependency weight] → It is the project-declared fixture stack ("WireMock and provider fixtures"); alternatives hand-roll an axum server for less fidelity.
- [Redirect policy none disables following for ALL gateway calls] → Desired: every move must become explicit evidence, never silent following.

## Migration Plan

None — development status: `schema.sql` edits in place, disposable test databases rebuild from it. Rollback is reverting the branch; no deployed data exists.

## Open Questions

(none)
