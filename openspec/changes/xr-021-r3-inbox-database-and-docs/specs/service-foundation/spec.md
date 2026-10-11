## ADDED Requirements

### Requirement: The bus connection uses the service's own inbox prefix

The service SHALL open its NATS connection with the inbox prefix `_INBOX_github`, and the reviewed stanza of its identity SHALL allow subscription to `_INBOX_github.>` and nothing else.

#### Scenario: A JetStream request completes under the private prefix

- **WHEN** the service connects to a broker whose only subscribe permission for its identity is `_INBOX_github.>`
- **THEN** the three durables are verified, a publish receives its acknowledgement and a fetched result is delivered

### Requirement: The operator example names the shared database

The shipped example SHALL name role `ratatoskr_github` on database `ratatoskr` at port 5432, and the repository SHALL ship the provisioning file that creates that role.

#### Scenario: The example and the role file agree

- **WHEN** the example is read
- **THEN** its database URL is `postgres://ratatoskr_github:CHANGE-ME@127.0.0.1:5432/ratatoskr` and `deploy/postgres/role.sql` creates `ratatoskr_github` and grants `connect, create` on `ratatoskr`
