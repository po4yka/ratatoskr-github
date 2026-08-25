# sync-scheduling Specification

## Purpose

Lets this service run its own synchronization on a schedule without owning a scheduler: the platform scheduler publishes this service's sync commands, the catalog validates and consumes them idempotently under the platform command grammar, dispatches the requested scan mode, escalates to a forced full rescan when an incremental pass detects an ordering gap, and refreshes native star-list state independently alongside every handled command. Schedule registration stays with the documented operator mechanism; nothing here registers schedules programmatically.

## ADDED Requirements

### Requirement: Commanded sync refreshes star lists independently

After dispatching the requested star mode for a handled sync command, the catalog SHALL attempt a star-list snapshot for the same account and SHALL report both outcomes; a list-snapshot failure, pause, or completion SHALL NOT alter the star-mode outcome or its recorded effects, and the star-mode result SHALL NOT suppress the list snapshot.

#### Scenario: A commanded full sync also snapshots lists

- **WHEN** a well-formed command requesting mode `full` is handled
- **THEN** the handling reports the completed full-mode star run and a separate completed `star_lists` run for the same account

#### Scenario: A list failure never invalidates the star outcome

- **WHEN** the star-mode dispatch completes successfully but the chained list snapshot fails
- **THEN** the report carries the successful star outcome unchanged alongside the failed list outcome, and all star rows from the star run remain exactly as written
