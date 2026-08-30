## Context

The workspace `github-fleet-bus-runtime` specification and `GHB-017` changeset define the cross-repository subject inventory, fixed durables, deployment order, and proof boundary. GitHub already has transactional domain operations, outbox/inbox tables, typed handlers, durable checkpoints, and due functions; only HTTP listeners are wired into the process.

## Goals / Non-Goals

**Goals:** persist final classified wire bytes atomically; publish at least once with per-key ordering and recoverable leases; couple inbound acknowledgement to durable state; resume unfinished sync claims; continuously run due work; supervise every bus worker; make readiness and shutdown truthful; ship a bounded systemd role.

**Non-Goals:** new payload versions, live provider acceptance, Knowledge/Vault business execution, topology creation, production credentials, host/firewall mutation, Git execution, or LLM work.

## Decisions

Outbox rows hold the exact subject, final envelope JSON, message identity, ordering key/sequence, lease, attempt/backoff, publish, and dead-letter state. Inbox rows hold stream/consumer/delivery coordinates, received/processing/retryable/consumed/rejected state, lease, attempt, terminal outcome, and redacted error code. The database remains authoritative; one bounded publisher, four fixed consumers, and two due workers run under shared cancellation and supervision. Broker acknowledgement occurs only after commit, publisher marking only after JetStream persistence acknowledgement, and startup verifies rather than creates topology. Serving requires the complete bus and credential-encryption configuration; one-shot operator roles validate only their dependencies.

## Risks / Trade-offs

- The schema cutover invalidates old development databases and readers; project policy requires recreation and one current path.
- A dead-letter blocks later rows only for its ordering key; exact-identity requeue restores progress while unrelated keys continue.
- Bus dependency makes readiness stricter; this is required because a listener-only process cannot perform its declared role.
- Provider reads may repeat after retries; durable checkpoints and complete-snapshot authority prevent false absence.

## Migration Plan

Land after Platform's additive topology. Recreate disposable databases, install protected seed/encryption inputs and the new unit, verify topology/readiness, then enable schedules. Rollback disables schedules and stops the new unit first, restores the old binary/configuration, and retains database rows and durable cursors.
