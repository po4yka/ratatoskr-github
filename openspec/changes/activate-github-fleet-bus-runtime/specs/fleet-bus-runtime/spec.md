## Purpose

Define GitHub Catalog's internal durable message runtime under the workspace-owned fleet-bus contract.

## ADDED Requirements

### Requirement: Classified outbox records are final and recoverable

GitHub SHALL commit each domain change with the exact classified transport subject, complete canonical envelope bytes, stable message identity, ordering key, and sequence. A bounded leased publisher SHALL preserve order within one key, allow unrelated keys to progress, use the identity as `Nats-Msg-Id`, mark success only after JetStream persistence acknowledgement, and retain exhausted rows for exact-identity operator requeue.

#### Scenario: Broker acknowledgement before database mark is replay-safe
- **WHEN** JetStream stores a message and the process terminates before the outbox mark commits
- **THEN** restart republishes identical subject, bytes, and identity and eventually marks the original row published

#### Scenario: One failed key does not block another
- **WHEN** the earliest row for one key repeatedly fails and another key has a due row
- **THEN** the unrelated row progresses while no later row overtakes the failed key

### Requirement: Inbound work is resumable and acknowledged after commit

GitHub SHALL consume only its four fixed subjects, persist transport coordinates and processing state, and acknowledge only after a committed terminal inbox/domain outcome. Consumed/rejected identities are terminal duplicates; received, expired processing, and retryable failure remain resumable. Malformed owned deliveries SHALL become redacted terminal rejections and SHALL NOT poison later messages.

#### Scenario: Provider failure after claim resumes
- **WHEN** a scheduled sync claim fails before a terminal outcome
- **THEN** redelivery resumes from durable synchronization state and does not infer absence from the incomplete pass

#### Scenario: Commit before acknowledgement repeats no effect
- **WHEN** a domain outcome commits and the process terminates before acknowledging
- **THEN** redelivery observes the terminal identity, changes no projection, and is acknowledged

### Requirement: Serving supervises the complete bus boundary

The serving process SHALL supervise one publisher, four consumers, one due-analysis worker, and one policy-reconciliation worker with finite configuration, retries, timeouts, batches, cancellation, and join bounds. Readiness SHALL require database, usable bus connection, exact durable topology, and live workers. One item failure or dead letter SHALL be separately observable without making unrelated shared work unavailable.

#### Scenario: Listener-only service is not ready
- **WHEN** HTTP listeners bind without a usable complete bus supervisor
- **THEN** readiness reports a stable non-ready dependency result

#### Scenario: Shutdown preserves accepted work
- **WHEN** termination interrupts inbound or outbound work
- **THEN** new claims stop, committed outcomes remain deduplicated, uncommitted deliveries remain redeliverable, leases recover, listeners and bus close, and the database closes last within the declared bound

### Requirement: Deployment carries protected finite configuration

GitHub SHALL ship an `aarch64-unknown-linux-gnu` `Type=exec` unit using its dedicated identity, loopback domain port `8092`, workspace-allocated operator port `9469`, owned PostgreSQL role, protected NKey seed and encryption inputs, NVMe log path, explicit filesystem/resource restrictions, and `TimeoutStopSec=130s`. Secrets SHALL NOT appear in checked-in artifacts, arguments, logs, metrics, readiness, or serialized/debug configuration.

#### Scenario: Missing or secret-bearing configuration fails safely
- **WHEN** required serving configuration is missing, unknown, unreadable, or inspected through debug/serialization
- **THEN** startup/check-config returns a stable field/rule error and exposes no secret value
