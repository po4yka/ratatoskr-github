## Context

See proposal.md. The behaviour is defined by XR-021 CONTRACTS.md sections R3-01, R3-11 and R3-19.

## Decisions

### D1. One literal beside the durable constants

`INBOX_PREFIX: &str = "_INBOX_github"` lives in `bus.rs` beside the durable specifications and is applied to the connect options on both branches (with and without an nkey seed), so no connection of this service can reply on the shared namespace. `Bus::connect` is the only place the service opens a NATS connection. The reviewed fragment `deploy/nats/identity.conf` is the byte-equal copy of the stanza in Platform's `ratatoskr.conf`; it changes to `subscribe: { allow: ["_INBOX_github.>"] }` in the same step.

### D2. The test connects through the real function

`authorized_bus.rs` already starts a broker from the fragment. The new test calls `Bus::connect`, which verifies the three durables with JetStream consumer-info requests, then lets the supervised relay publish one outbox row and fetch results, so a PubAck and a fetched message both travel on the private inbox. Under a fragment that allows only `_INBOX_github.>`, a connection without the prefix never receives the consumer-info reply and `connect` times out.

### D3. The example targets the shared database

The shared database is `ratatoskr`, created by Platform's `01-database-and-roles.sql`. `deploy/postgres/role.sql` is run by a PostgreSQL administrator after that file and before the first start of this service. The role is created without a password (set with `\password`), has no elevated attributes, may connect and create in the database, and has no rights on schema `public`. The catalog schema is namespaced as `github_catalog`, so the shared database holds it beside the other services' schemas.

### D4. README from the code

The status paragraph is rewritten from `services/catalog/src` and the workspace. What exists is listed; the fleet-wide account-erasure bus transport is the one named gap, plus consumption of `cmd.github.sync.requested.v1`.
