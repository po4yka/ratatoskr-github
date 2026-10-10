## Context

Contracts for every hop are fixed in XR-021 CONTRACTS.md (S01 subjects and envelopes, S02 relay and lifecycle rules, S03 ACL, S04 durables, S09 README endpoint and policy lane). This design records only the local shape.

## Decisions

- Envelopes are built where the row is written (`watches.rs`, `backup_policy.rs`), so the outbox row is the exact bytes the relay publishes and the envelope id equals the row id (`Nats-Msg-Id`).
- The outbox gains `attempt_count`, `last_error` (a closed class vocabulary, never provider text) and `next_attempt_at`; the schema CHECK lists the two relayable subjects plus the existing non-bus github lanes. The relay matches the stored subject against its closed list and stops with a hard error on any other, so such a row is never skipped and never retried forever (S02 rule 2).
- Both analysis-request producers (`metadata::apply_fresh_source` and the watch dispatcher) and the policy publisher build their outbox row through one `envelopes` module, so the row is the exact bytes the relay publishes. The retained `repository_analysis_publications.payload` and `backup_policy_publications.document` stay bare contract documents: they are audit and erasure evidence, not bus messages.
- `services/catalog/src/bus.rs` owns every broker interaction. `serve()` supervises the relay, the dispatch ticker and the three consumers; the first task to return flips Lifecycle not-ready and `serve()` returns `Err`, so the process exits non-zero.
- Consumers verify the Edge-provisioned durable (`get_consumer_from_stream`) against filter, ack policy and ack wait and never create one. There is no create path at all: the integration tests play Edge with an admin identity, so no `provision_topology` switch exists to refuse.
- Dispositions follow S02 rule 7: ack after the inbox commit, `Term` for undecodable or wrong-producer input, `Nak` with a 2 s delay for transient failure. The message id handed to the catalog is the envelope event id so redelivery is idempotent.
- The README route reads `github_catalog.repository_readme_blobs` and compares the bearer with `secrecy` plus a constant-time comparison.

## Risks

- A `PubAck` timeout is indistinguishable from a permission denial; error text says to check the NATS server log for a Publish Violation (S02 rule 4).
- The events stream drops the oldest messages when full; a re-queue sweep for dropped analysis requests is a recommended follow-up, not part of this change.
