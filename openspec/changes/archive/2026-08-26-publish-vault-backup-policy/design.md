## Context

See [proposal.md](proposal.md) for motivation and [the capability spec](specs/vault-backup-policy-publication/spec.md) for observable behavior. The shared v1 policy and acknowledgment types are already committed in `ratatoskr-contracts`; Vault has its desired-state reconciler but neither service yet has the live fleet-bus adapter.

## Goals / Non-Goals

**Goals:**

- Make the catalog policy document deterministic from committed catalog state, and atomically durable with its outgoing request.
- Coalesce policy-affecting changes without losing the latest state or emitting a stale document.
- Preserve Vault's typed feedback as a local operator projection under at-least-once delivery.

**Non-Goals:**

- Git, mirror, snapshot, restore, retention, or offsite execution.
- A scheduler, NATS client, or Vault-side message handler; the existing outbox/inbox are the durable hand-off seam.
- Extending the already published backup-policy contract or adding a second contract version.

## Decisions

### D1: Use the published shared contract crate at an immutable commit

`ratatoskr-backup-contracts`, identifiers, and envelope primitives are pulled from the same full SHA used by the workspace until package publishing exists. The policy request uses the fleet command subject `cmd.vault.target.desired.v1`; the acknowledgment uses the event subject `evt.vault.backup_policy.acknowledged.v1`. This follows the fleet rule that desired state requests work while acknowledgments are facts.

Alternative considered: serialize a catalog-local JSON struct. Rejected because its semantics would drift from Vault's feedback contract and it would bypass contract fixtures.

### D2: Derive mirror enrollment from repository governance and policy rows

`auto` and `tracked` repositories are eligible. `ignored`, unclassified, `none`, and `metadata_only` are absent. `git_mirror`, `git_mirror_with_lfs`, and `complete_archive` all require a Git mirror, so they each produce the policy's mirror entry; deeper material collection stays a Vault concern. Per-repository cadence and priority default to `daily` and `standard`; byte-size hints are nullable and exclusions are explicit JSON data validated before constructing the contract type.

Alternative considered: publish every row including a `none` entry. Rejected because this contract specifies the desired mirror set, and withdrawal/retention handling belongs to Vault's distinct desired-state lifecycle.

### D3: Keep one catalog-wide publication cursor and a canonical state fingerprint

The schema stores policy-version history plus a singleton reconciliation cursor. Under a row lock, the worker derives sorted entries, fingerprints the stable content excluding generated timestamp/version, and only allocates the next version when that content changes. It writes the typed JSON and an outbox row in the same transaction. The row lock serializes concurrent workers; the outbox idempotency row makes committed success observable without executing any external side effect in the transaction.

Alternative considered: derive a version from time or use an auto-increment sequence. Rejected because time is not a strict order and a sequence can allocate a version for an unchanged or rolled-back publication.

### D4: Model debounce as durable dirty generations

Every local transaction that changes repository mode or star-driven governance marks the singleton dirty and moves its trailing deadline. The worker accepts an injected instant for deterministic tests, locks the cursor, and publishes only once the deadline has passed. It derives after acquiring that lock rather than caching a payload at trigger time, so a burst always emits the latest committed state.

Alternative considered: an in-memory timer. Rejected because restart loses the pending request and it cannot coordinate multiple service instances.

### D5: Record feedback through the inbox in the same transaction as its projection

Acknowledgment handling deserializes the shared payload at the boundary, inserts the envelope message ID into the existing inbox, and writes one feedback row only when that insert wins. The projection stores structured reasons and the prior Vault-applied version; it does not turn acceptance into health.

Alternative considered: retain only a log entry. Rejected because logs are neither queryable operator state nor an idempotency boundary.

## Risks / Trade-offs

- [Vault has not yet wired the shared bus contract into its reconciler] → This change produces a durable command and can record its defined feedback, but live end-to-end broker proof remains a coordinated Vault follow-up.
- [A malformed persisted exclusion could prevent publication] → Validate every row while deriving; leave the dirty generation pending and return a classified catalog error rather than emit a partial policy.
- [Burst activity delays publication] → The explicit trailing debounce favors one complete current document over chatty stale emissions; operators can inspect the pending generation/deadline.

## Migration Plan

1. Deploy the in-place current schema and this Catalog worker before enabling any outbox publisher route for the new command subject.
2. Enable the publisher and then Vault's matching consumer/acknowledgment producer in the coordinated fleet rollout; replaying the outbox is safe.
3. To roll back Catalog, stop claiming the new command subject. Existing policy records and feedback remain audit evidence; no Vault retention or deletion action is requested by rollback.
