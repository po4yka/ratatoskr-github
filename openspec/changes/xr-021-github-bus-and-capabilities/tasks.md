## 1. Contracts pin

- [x] 1.1 Move every `ratatoskr-*` dependency to contracts commit `ad16855c4e7f3d52cd118274faa3b8f3ab4da576` and refresh `Cargo.lock`; configuration, so there is no RED; the existing suite stays green.

## 2. Capability probe needs no user claim (S06 D2)

- [x] 2.1 RED: add `capabilities_are_served_without_the_user_claim` and `preview_still_requires_the_user_claim`; the first fails with 401 today.
- [x] 2.2 GREEN: the capabilities handler takes no header extractor and calls no `authenticated_user`.

## 3. Outbox stores complete envelopes (S09)

- [x] 3.1 RED: add `dispatched_analysis_request_is_a_complete_event_envelope` and `published_policy_is_a_vault_apply_command_envelope`; they fail on the bare payload and unprefixed subject.
- [x] 3.2 GREEN: build the envelopes, edit the outbox and inbox subject CHECKs in `schema.sql`, add the relay columns, and use the `evt.` spellings for the inbox subjects.

## 4. Bus relay, consumers and dispatch loops (S02, S04)

- [x] 4.1 RED: add `services/catalog/tests/bus.rs` against a real JetStream broker; it fails because `serve()` has no bus.
- [x] 4.2 GREEN: add `BusConfig`, `services/catalog/src/bus.rs`, the relay, the three consumers, the dispatch ticker and the supervised lifecycle.

## 5. Authorized README byte endpoint (S09)

- [x] 5.1 RED: add `services/catalog/tests/readme_blob_route.rs`; every case answers 404 today.
- [x] 5.2 GREEN: add the route, the constant-time bearer check and `RATATOSKR__INTERNAL__READER_SERVICE_SECRET`.

## 6. Authorized broker and identity fragment (S03)

- [x] 6.1 RED: add `services/catalog/tests/authorized_bus.rs` which reads `deploy/nats/identity.conf`; it fails because the fragment is absent.
- [x] 6.2 GREEN: add `deploy/nats/identity.conf` and document `/etc/ratatoskr/github.nkey`.

## 7. Documentation and final gate

- [x] 7.1 Update `AGENTS.md` current phase, `README.md` and `docs/INTERFACES.md`; documentation has no RED.
- [x] 7.2 Run the full local gate and `openspec validate --all --strict`.
