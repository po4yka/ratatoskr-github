## Why

GitHub Catalog already implements metadata observation, account-scoped mutations, idempotency, and backup-policy dirtiness, but exposes only operator health routes. Telegram cannot pass its mandatory live-service gate until Catalog exposes those capabilities through the authenticated Edge gateway with a truthful, component-level result.

## What Changes

- Add a separate loopback-only domain API listener on the fleet-assigned port `8092`, preserving the existing operator listener on `9095`.
- Expose `POST /v1/gh/repositories/preview` as a read-only provider-backed preview that trusts only Edge-injected `x-ratatoskr-user-id`, selects only an account owned by that user, and returns the shared bounded contract.
- Expose `POST /v1/gh/repositories/actions` for idempotent `metadata`, `track`, and `star` actions. The request must carry the stable preview target, explicit confirmation evidence reference, and idempotency key.
- Compose existing metadata, mode, provider mutation, and desired-policy primitives without provider credentials leaving this service.
- Return one outcome per attempted/not-applicable/blocked step and retain an earlier provider success even when later Catalog or backup-policy work fails; never compensate a successful provider star automatically.
- Publish `/v1/capabilities`, safe shared error envelopes, bounded bodies/timeouts, low-cardinality telemetry, and a fake-provider/API harness.
- Consume the additive `ratatoskr-github-contracts` revision produced by the coordinated changeset.

## Capabilities

### New Capabilities

- `repository-interaction-api`: Loopback authenticated repository preview and confirmed action API with idempotent, component-level truthful outcomes.

### Modified Capabilities

None.

## Impact

- `services/catalog` runtime/listeners, `crates/catalog` orchestration and provider seams, strict configuration, tests, README/interfaces, and the pinned contracts revision.
- The change affects metadata reads, repository modes, provider star writes, and desired backup-policy acceptance; it does not change incremental/full snapshot authority or native list behavior.
- `ratatoskr-contracts` must merge first. Telegram consumes this API only after a live readiness and fake-provider smoke check succeeds.
- No migration file is added; any schema adjustment edits `schema.sql` in place. Provider mutation rollback remains intentionally manual because truth outranks pretending the operation was atomic.
