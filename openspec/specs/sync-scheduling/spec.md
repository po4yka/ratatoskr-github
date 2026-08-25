# sync-scheduling Specification

## Purpose
Lets this service run its own synchronization on a schedule without owning a scheduler: the platform scheduler publishes this service's sync commands, the catalog validates and consumes them idempotently under the platform command grammar, dispatches the requested scan mode, and escalates to a forced full rescan when an incremental pass detects an ordering gap. Schedule registration stays with the documented operator mechanism; nothing here registers schedules programmatically.

## Requirements

### Requirement: Sync command envelopes are validated before any effect

The catalog SHALL accept a synchronization command only when it carries the platform command envelope for `github.sync.requested.v1` with a parseable command identity, a tenant of the form `user:<uuid>`, and a payload object naming an existing connected account by its owner reference with an optional mode of `incremental` or `full` (defaulting to incremental); any other shape SHALL be rejected with no rows written and no scan started.

#### Scenario: A well-formed scheduled command starts the requested scan

- **WHEN** a command envelope with type `github.sync.requested.v1`, a fresh command identity, tenant `user:<uuid>`, and payload account matching a connected account's owner reference arrives
- **THEN** the requested mode runs for that account's data and an inbox row records the consumption

#### Scenario: A malformed or foreign command changes nothing

- **WHEN** a command arrives with a different type, an unparseable identity or tenant, a missing or unknown account reference, or a payload mode outside the vocabulary
- **THEN** the call fails naming the violation, no inbox row exists, and no sync run was created

### Requirement: Consumption is durable and idempotent

The catalog SHALL record each accepted command in the owned inbox keyed by the command identity inside the acceptance path, and a redelivered command whose identity is already recorded SHALL be reported as a duplicate with no second scan, no second inbox row, and no state change.

#### Scenario: Redelivery of the same command performs no second effect

- **WHEN** the identical command envelope is delivered twice in sequence
- **THEN** exactly one sync run exists for it, one inbox row records the command, and the second delivery reports a duplicate outcome

### Requirement: Commanded gaps chain into a forced full rescan

When a commanded incremental scan ends with a detected ordering gap, the catalog SHALL run a full snapshot for the same account as part of handling that command and SHALL report both facts - the gap outcome of the incremental run and the completion of the forced full rescan - so the schedule converges state instead of stalling until the next periodic full pass.

#### Scenario: A gap during a commanded incremental triggers the full rescan immediately

- **WHEN** the incremental scan dispatched by a sync command detects an ordering gap
- **THEN** the handling reports the failed incremental run and a completed full-mode run for the same account, with star authority reflecting the full snapshot

### Requirement: Schedules are registered through the documented operator mechanism

The catalog SHALL NOT implement schedule registration. The deployment documentation SHALL carry the operator registration statements, following platform's documented mechanism, that create this service's two schedules - a frequent incremental sync and a less frequent full reconciliation - as disabled rows targeting `github.sync.requested.v1` with explicit enabling.

#### Scenario: The documentation shows both schedules as operator inserts

- **WHEN** an operator follows this repository's deployment documentation
- **THEN** they find registration statements inserting a frequent incremental and a periodic full reconciliation schedule into platform's schedule table, created disabled and enabled explicitly, matching platform's published grammar and column rules

### Requirement: Commanded sync refreshes star lists independently

After dispatching the requested star mode for a handled sync command, the catalog SHALL attempt a star-list snapshot for the same account and SHALL report both outcomes; a list-snapshot failure, pause, or completion SHALL NOT alter the star-mode outcome or its recorded effects, and the star-mode result SHALL NOT suppress the list snapshot.

#### Scenario: A commanded full sync also snapshots lists

- **WHEN** a well-formed command requesting mode `full` is handled
- **THEN** the handling reports the completed full-mode star run and a separate completed `star_lists` run for the same account

#### Scenario: A list failure never invalidates the star outcome

- **WHEN** the star-mode dispatch completes successfully but the chained list snapshot fails
- **THEN** the report carries the successful star outcome unchanged alongside the failed list outcome, and all star rows from the star run remain exactly as written
