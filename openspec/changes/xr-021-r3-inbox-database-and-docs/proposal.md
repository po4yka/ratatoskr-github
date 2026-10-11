## Why

Changeset XR-021 round 3 (CONTRACTS.md sections R3-01, R3-11 and R3-19) found three defects in this repository. Every non-Edge NATS identity may subscribe to the shared `_INBOX.>` namespace, so a compromised service could read the JetStream deliveries of every other service; this service connects without a private inbox prefix. The operator example names the development compose database (role `github`, database `github`, port 5435) although the target runs one PostgreSQL database, `ratatoskr`, on port 5432, and nothing creates the role the example names. The README status text still says credentials, watches and event publication do not exist.

## What Changes

- **Deliberate break**: the service connects with `custom_inbox_prefix("_INBOX_github")` and its stanza in `deploy/nats/identity.conf` allows `subscribe` only on `_INBOX_github.>`. A service built before this change cannot complete JetStream requests under the new ACL (the reply is never delivered and the call times out), and the new build cannot complete them under the old ACL either, because NATS compares whole subject tokens and `_INBOX_github` does not match `_INBOX.>`: reload NATS and deploy the services of one snapshot together. Nothing is deployed, so the break is named and not staged (R3-01).
- **Deliberate break**: `deploy/systemd/github.conf.example` names `postgres://ratatoskr_github:CHANGE-ME@127.0.0.1:5432/ratatoskr`, role `ratatoskr_github` on the shared database `ratatoskr`, instead of role `github`, database `github`, port 5435. `deploy/postgres/role.sql` creates the role. The development compose database and the development defaults keep port 5435 and say "development" next to them (R3-11).
- `README.md` states what the code does and what the workspace runs, from the code and not from the older sentence (R3-19).

## Capabilities

### Modified Capabilities

- `service-foundation`: the bus connection uses the service's own inbox prefix, and the shipped example targets the shared database with a checked-in role file.

## Impact

`services/catalog/src/bus.rs`, `deploy/nats/identity.conf`, `services/catalog/tests/authorized_bus.rs`, `deploy/systemd/github.conf.example`, `deploy/postgres/role.sql`, `services/catalog/tests/deployment_profile.rs`, `README.md`, `DEVELOPMENT.md`. Cross-repository behaviour is defined in XR-021 CONTRACTS.md R3-01, R3-11 and R3-19, which this change cites and does not restate.
