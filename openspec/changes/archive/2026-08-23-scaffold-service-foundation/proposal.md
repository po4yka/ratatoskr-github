## Why

The repository is in architecture bootstrap: it holds intent documents but no executable boundary, so every later milestone (credentials, synchronization, mutations) has nothing to build on. Implementation plan item 1 establishes the process foundation — typed configuration, structured telemetry, typed errors, operator health routes, the first `github_catalog` schema definition, and a test harness with a green gate — so subsequent changes start from running code instead of prose.

## What Changes

- Add a Rust workspace with two members: the `ratatoskr-github-catalog` domain library and the `ratatoskr-github-catalog-service` deployable binary.
- Add finite typed configuration loaded only from `RATATOSKR__`-prefixed environment entries, rejecting unknown keys and never echoing supplied values.
- Add one-time structured (JSON) telemetry initialization and typed telemetry errors.
- Add typed error hierarchies for configuration, persistence, and telemetry failures.
- Add loopback operator routes: `/live`, `/ready`, `/metrics`, `/version`, with readiness following startup and drain.
- Add the first-version editable `schema.sql` defining the `github_catalog` schema with placeholder tables; no migration tooling.
- Add a disposable-database test harness, strict lint configuration (`clippy.toml`, workspace lints), dependency policy (`deny.toml`), the pinned toolchain, and `.github/workflows/ci.yml` whose command list matches the fenced gate block in DEVELOPMENT.md.
- Keep credentials, GitHub API access, synchronization, star observation, list reconciliation, backup-policy publication, watches, analysis requests, NATS, and any second API version outside this change.

## Capabilities

### New Capabilities

- `service-foundation`: Process configuration strictness, operator health routes, telemetry bootstrap, owned-schema application, and the gates that hold them in place.

### Modified Capabilities

None.

## Impact

This creates the first Rust code, the first PostgreSQL schema definition, tests, CI, compose-based local database setup, and updated README/DEVELOPMENT status in `ratatoskr-github`. It introduces pinned external dependencies (tokio, axum, sqlx, tracing, figment-free hand-rolled loader style used by the fleet's minimal services) and requires no GitHub credential in default tests or CI.
