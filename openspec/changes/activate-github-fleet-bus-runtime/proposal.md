## Why

GitHub Catalog commits cross-service intent and already implements handlers and due functions, but the serving process never connects to the fleet bus, publishes the outbox, consumes a durable, dispatches due analysis, or reconciles dirty policy. It can report ready while all fleet workflows remain stalled, and a scheduled-sync claim can become a permanent false duplicate after a transient provider or process failure.

## What Changes

- **BREAKING**: store exact `cmd.`/`evt.` transport subjects and complete canonical wire envelopes in the current inbox/outbox schema; update all callers together with no compatibility path.
- Add ordered leased outbox publication with broker-ack-before-mark, bounded retry/dead-letter state, stable `Nats-Msg-Id`, and exact-identity operator requeue.
- Add resumable inbox state and four fixed-durable consumers whose acknowledgements follow committed terminal outcomes.
- Add validated redacted bus/worker configuration, due workers, supervision, truthful readiness, bounded telemetry, and ordered shutdown.
- Add the production-shaped arm64 systemd role, environment example, logrotate rule, deployment documentation, and fixture seams required by workspace `GHB-017`.

## Capabilities

### New Capabilities

- `fleet-bus-runtime`: GitHub Catalog's classified transactional wire records, recoverable publisher/consumers, due workers, readiness, drain, telemetry, and deployment role.

### Modified Capabilities

None.

## Impact

This is the second repository in workspace changeset `GHB-017`, after Platform provisions the fixed topology. It affects the catalog schema and persistence, wire adapters, service configuration/runtime, deployment files, tests, README, and DEVELOPMENT. It does not change shared payload types, call live GitHub in tests, implement Knowledge/Vault consumers, or operate the deployment host.
