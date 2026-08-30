## 1. Classified current schema and envelopes

- [x] 1.1 RED — add the schema test for six exact subjects and complete outbox/inbox recovery fields; run the locked targeted PostgreSQL 17 test through `build-gate --` and observe the current-schema assertion fail.
- [x] 1.2 GREEN — edit `schema.sql` in place, update test support/readers, and rerun the schema test.
- [x] 1.3 RED — add analysis final-event-envelope assertions in `readme_observations` and `watch_analysis_flow`; observe raw payload/unclassified subject failure.
- [x] 1.4 GREEN — construct canonical analysis event envelopes transactionally and rerun both tests.
- [ ] 1.5 RED — add the final Vault command-envelope assertion in `backup_policy_flow`; observe the raw policy failure.
- [x] 1.6 GREEN — construct the canonical policy command transactionally and update feedback fixtures/readers.

## 2. Recoverable outbox publisher

- [ ] 2.1 RED — add bounded claim, ordering, lease recovery, backoff, and dead-letter tests in `outbox_claims`; observe missing state/operations.
- [x] 2.2 GREEN — implement database claim/confirm/fail/requeue operations with stable redacted codes.
- [ ] 2.3 RED — add real-broker publication tests for ack-before-mark, identical restart replay, `Nats-Msg-Id`, and no topology creation.
- [x] 2.4 GREEN — implement the bounded JetStream publisher using stored subjects/bytes and lease recovery.
- [ ] 2.5 RED — add exact-identity dead-letter requeue and refusal tests in `operator_commands`.
- [x] 2.6 GREEN — implement the narrow operator command without editing bytes or creating rows.

## 3. Resumable inbox and fixed consumers

- [ ] 3.1 RED — add sync claim recovery, terminal duplicate, and incomplete-snapshot absence-safety tests.
- [x] 3.2 GREEN — implement received/processing/retryable/terminal states and account-owned credential loading per delivery.
- [ ] 3.3 RED — add real-broker tests for all four durables, ack-after-commit, retryable NAK, terminal rejection, duplicate, and foreign isolation.
- [x] 3.4 GREEN — implement four bounded fixed-durable consumers and subject-specific adapters without topology creation.
- [x] 3.5 RED — add complete finite redacted serving bus configuration tests.
- [x] 3.6 GREEN — implement role-aware configuration validation.

## 4. Due workers, supervision, readiness, and drain

- [ ] 4.1 RED — add tests proving due analysis and dirty policy progress without HTTP and survive transient iteration failure.
- [x] 4.2 GREEN — implement bounded due-analysis and policy-reconciliation loops over database state.
- [ ] 4.3 RED — add listener-only, topology drift, bus-loss/recovery, dead-letter, and worker-exit readiness tests.
- [x] 4.4 GREEN — supervise seven workers and expose bounded low-cardinality health/lag/retry/duplicate/rejection/dead-letter telemetry.
- [ ] 4.5 RED — add shutdown tests around commit/ack, leases, claim stop, listener drain, bus close, and bounded exit.
- [x] 4.6 GREEN — implement ordered shared cancellation and bounded joins before database close.

## 5. Deployment and verification

- [x] 5.1 RED — add deployment-profile assertions for arm64, identity, ports, protected inputs, database role, NVMe logging, restrictions, `Type=exec`, and `TimeoutStopSec=130s`.
- [x] 5.2 GREEN — add the unit, redacted environment example, logrotate rule, deployment docs, README/DEVELOPMENT boundary, and release boot/readiness/SIGTERM smoke.
- [x] 5.3 Run the complete locked `DEVELOPMENT.md` gate, strict OpenSpec validation, diff/secret review, commit only GHB-017 paths, and publish the authorized branch with exact local/remote SHA verification.
