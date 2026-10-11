## 1. Per-service inbox prefix (R3-01)

- [x] 1.1 RED: add `services/catalog/tests/authorized_bus.rs::the_service_completes_jetstream_requests_under_its_inbox_prefix` and change `deploy/nats/identity.conf` to `subscribe: { allow: ["_INBOX_github.>"] }`; the test fails because `Bus::connect` times out waiting for the consumer-info reply on the shared inbox.
- [x] 1.2 GREEN: add `INBOX_PREFIX` beside the durable constants in `bus.rs` and apply it with `custom_inbox_prefix` on both connect branches.

## 2. The example names the shared database (R3-11)

- [x] 2.1 RED: add `services/catalog/tests/deployment_profile.rs::the_example_names_the_shared_database_and_the_role_file_exists`; it fails because the example names role `github` on port 5435 and `deploy/postgres/role.sql` does not exist.
- [x] 2.2 GREEN: edit the example and its header comment, add `deploy/postgres/role.sql`, add the PostgreSQL paragraph to `README.md`, and mark the development URLs as development.

## 3. Documentation from the code (R3-19)

- [ ] 3.1 Rewrite the `README.md` status paragraph and the no-pin sentences from `services/catalog/src` and the workspace; this task has no failing test because it is documentation.
