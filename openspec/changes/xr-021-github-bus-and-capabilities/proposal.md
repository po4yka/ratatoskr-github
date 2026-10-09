## Why

Changeset XR-021 found that GitHub could not take part in the fleet bus at all: its capability probe demanded a user claim that Edge's anonymous probe never carries, its outbox held bare payloads under unprefixed subjects, the process had no NATS client, and Knowledge had no way to fetch README bytes. The cross-repository behaviour is defined in XR-021 CONTRACTS.md sections S01, S02, S03, S04, S06 (D2) and S09; this change cites it and defines only what is internal to this repository.

## What Changes

- Move every `ratatoskr-*` contracts dependency to contracts commit `ad16855c4e7f3d52cd118274faa3b8f3ab4da576` (S00).
- `GET /v1/capabilities` stops requiring the `x-ratatoskr-user-id` claim; the document is unchanged and `POST /v1/gh/repositories/preview` still requires it (S06 D2).
- The outbox stores complete `EventEnvelope` / `CommandEnvelope` JSON under class-prefixed subjects `evt.knowledge.repository_analysis.requested.v1` and `cmd.vault.backup_policy.apply_requested.v1`; the inbox subjects become `evt.knowledge.repository_analysis.completed.v1`, `evt.knowledge.repository_analysis.failed.v1` and `evt.vault.backup_policy.acknowledged.v1`. The ad hoc subject `cmd.vault.target.desired.v1` is removed (deliberate break, S09).
- A bus module gives the service its first NATS client: outbox relay with ack-gated `published_at` and backoff, three supervised pull consumers over Edge-provisioned durables, dispatch loops, and a Lifecycle that goes not-ready and makes `serve()` return an error when any bus task exits (S02, S04).
- `GET /internal/v1/readme-blobs/{sha256_hex}` serves stored README bytes to Knowledge behind a constant-time bearer secret; the route is not mounted when the secret is unset (S09).
- `deploy/nats/identity.conf` carries the GITHUB stanza of the reviewed ACL, proven by an authorization-enabled broker test (S03).

## Capabilities

### New Capabilities

- `github-bus-and-capabilities`: GitHub publishes and consumes only complete envelopes on its two published and three consumed subjects, serves an anonymous capability probe, and serves README bytes to Knowledge.

### Modified Capabilities

None. `openspec/specs/` is empty by design.

## Impact

- Affects `schema.sql` (outbox columns and subject CHECKs edited in place; no migration), `crates/catalog` (`config`, `watches`, `backup_policy`), `services/catalog` (`bus`, `repository_api`, `lib`, `main`), `deploy/nats`, README and docs.
- Adds the `async-nats` dependency to the workspace and `services/catalog`.
- Out of scope: account-erasure NATS transport, GitHub sync command intake, `vault.target.state_changed.v1`, a re-queue sweep for dropped analysis requests, schedule registration.
