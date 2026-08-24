# Proposal: add repository identity, mutable aliases, metadata, and conditional requests

## Why

Implementation plan item 3 needs the catalog to know repositories by a stable identity rather than by their mutable names and URLs. The legacy monolith keyed everything on `owner/name`, so every rename or transfer produced duplicate records; the catalog must instead converge observations on one logical repository and keep aliases as redirecting history. Metadata refreshes against GitHub must also stop paying full response costs on every scan, and all provider traffic must share one per-token rate-limit budget so no operation can exhaust an account unnoticed.

## What Changes

- Introduce repository upsert keyed by GitHub's numeric provider ID with a generated stable internal identifier (`repository_id`, UUIDv7).
- Record `owner/name`, `html_url`, and `clone_url` aliases with redirect semantics: when rename/transfer evidence arrives from API responses (301 `Location` or a body whose `full_name` differs from the requested alias), the new alias is activated for the same logical repository and the superseded alias still resolves to it.
- Make live alias values globally unique while preserving historical alias rows, so a name later taken by a different repository cannot silently hijack an old identity.
- Add a metadata projection (`description`, primary language, stargazer count, topics, default branch, `pushed_at`) refreshed through conditional requests: stored ETags are sent as `If-None-Match`, `304 Not Modified` is handled cheaply without rewriting state, and raw metadata revisions are retained as bounded per-repository history.
- Add per-token rate-limit accounting shared across operations: ledger updated from provider headers (`x-ratelimit-*`, `Retry-After`), enforcing a reserve floor and refusing requests until cooldown/reset expires.
- Extend the editable first-version schema in place: alias status/supersession columns plus a partial unique index, and new `github_catalog.repository_metadata` / `github_catalog.repository_metadata_revisions` tables.
- Add a GitHub REST provider seam in the domain crate with recorded synthetic HTTP fixtures served through WireMock-style tests.

Out of scope: star snapshots and unstar semantics (item 4), incremental scans (item 5), analysis requests, credential storage itself.

## Capabilities

### New Capabilities

- `repository-identity`: stable repository identity, alias records, resolution of current and superseded aliases, and rename/transfer redirect behavior.
- `repository-metadata`: metadata projection freshness via conditional requests, 304 short-circuit reuse, revision append on change, and bounded revision history.
- `provider-gateway`: GitHub REST access behind a testable seam — conditional request mechanics, rename observation from responses, per-token rate-limit budget enforcement, and recorded-fixture parsing contracts.

### Modified Capabilities

(none)

## Impact

- `schema.sql`: in-place edits to `github_catalog.repository_aliases`; two new tables. The disposable-database schema tests are updated to the extended table set.
- `crates/catalog`: new modules for identity persistence, metadata persistence, provider gateway, rate-limit ledger, and the metadata observe flow wiring them together; new pinned dependencies (`reqwest` rustls/json, `wiremock` as dev-dependency).
- No service HTTP surface changes; `services/catalog` binary is untouched.
- No migrations (development status); no version bump; no cross-repository contract changes — provider types stay inside this adapter boundary.
