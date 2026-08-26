# Proposal: add-repository-modes-and-mutations

## Why

The catalog's read paths are solid - identity, metadata, star snapshots, incremental scans, and list snapshots all work - but the service cannot express user intent about a repository and cannot perform any confirmed write against GitHub. The retired monolith exposed track/star actions behind bot confirmations and filed repositories into star lists; Ratatoskr has no equivalent yet. Implementation plan item 7 calls for exactly this: repository modes, audited mode transitions, and idempotent star/list mutations that report truthful partial-success outcomes.

## What Changes

- Add a per-repository mode vocabulary to the catalog: `auto` (presence governed by star state), `tracked` (explicitly added without starring), `ignored` (explicitly excluded), with unclassified (null) for repositories known but never classified.
- Enforce validated mode transitions: explicit requests may set only `tracked` or `ignored`; `auto` is reached only through star effects (mutation or first sync observation over unclassified); `ignored` requires an unstarred repository; `tracked` is sticky under unstar; synchronization may promote unclassified to `auto` and never overrides an explicit mode.
- Record every accepted mode transition and every mutation attempt as an audit row capturing who, what, when, outcome, and failure reason.
- Introduce an authorization context passed by the calling product flow (telegram/web own confirmation UX): account reference, principal, calling source, and idempotency key. The service enforces account connection status and granted-scope requirements before any provider call and records refusals.
- Execute star/unstar through GitHub's documented, server-side-idempotent GraphQL mutations, and list membership add/remove through the same mutations the legacy deployment used in production (`updateUserListsForItem` - present in the public GraphQL schema though undocumented), computing the complete desired list set from live provider state so a write can never silently move a repository between lists.
- Make every mutation replay-safe: a retried operation with the same idempotency key yields the same end state and exactly one audit record; batched operations execute independently and report per-operation outcomes including partial success.
- Edit the first-version schema in place (development status: no migrations): `repositories.mode`, `github_accounts.granted_scopes`, and a new `github_catalog.mutation_audit` table.
- Explicitly out of scope: confirmation UI (clients own it), analysis triggers (item 9), event publication through the outbox (deferred until consumers exist), watch and backup-policy coupling (items 8-9).

## Capabilities

### New Capabilities

- `repository-modes`: the mode vocabulary, validated transitions, synchronization interaction rules, and audited transition records.
- `provider-mutations`: authorization-context enforcement, idempotent star/unstar/list-membership execution, audit records keyed by idempotency, and per-operation truthfulness including batched partial success.

### Modified Capabilities

- None. Existing capabilities (identity, metadata, snapshots, scheduling) keep their requirements unchanged; mode promotion touches only the unclassified-to-auto edge, which no existing spec constrains.

## Impact

- `schema.sql`: three in-place extensions listed above; `crates/catalog/tests/schema.rs` gains constraint assertions.
- `crates/catalog/src/`: new `modes.rs` (transition validation and application), `mutations.rs` (authorization context, executor, outcomes, audit writing), `provider_mutations.rs` (mutation API trait and GraphQL wire shapes); `lib.rs` re-exports. `commands.rs`/sync flows gain only the unclassified-to-auto promotion hook.
- `services/catalog/`: untouched (no public API surface yet; dispatch arrives with later items).
- Docs: README status paragraph, DEVELOPMENT.md status sentence, docs/DOMAIN.md mode section alignment.
- Provider reliance worth noting: list membership writes depend on GraphQL mutations that GitHub ships in its public schema but does not document; this reproduces legacy production behavior and is flagged in the design.
