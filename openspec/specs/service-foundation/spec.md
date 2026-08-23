# GitHub Catalog service foundation

The process foundation of Ratatoskr GitHub Catalog: strict finite configuration, one-time structured telemetry, operator health routes, application of the owned first-version `github_catalog` schema, and the gates that keep the tree verifiable.

## Requirements

### Requirement: Configuration is loaded only from recognized prefixed environment entries
The service SHALL load configuration exclusively from `RATATOSKR__`-prefixed environment entries against a closed set of keys, SHALL reject an unrecognized or invalid entry instead of ignoring it, and SHALL report every rejection without echoing the supplied value. Defaults SHALL be finite: a loopback admin address with a nonzero port, positive connection, timeout, and shutdown limits.

#### Scenario: Unknown key is refused
- **WHEN** configuration is loaded from an environment containing `RATATOSKR__LIMITS__MYSTERY`
- **THEN** loading fails with an error naming the offending key and the diagnostic does not contain the supplied value

#### Scenario: Invalid value is refused
- **WHEN** configuration is loaded with `RATATOSKR__LIMITS__SHUTDOWN_TIMEOUT_MS` set to a non-positive or non-integer value
- **THEN** loading fails with an error naming the key and the diagnostic does not contain the supplied value

#### Scenario: Database location is validated at load time
- **WHEN** configuration is loaded with `RATATOSKR__STORAGE__DATABASE_URL` set to a string that is not a PostgreSQL connection URL
- **THEN** loading fails with an error naming the key

#### Scenario: Defaults are finite and secret-free on output
- **WHEN** default configuration is inspected and then serialized
- **THEN** every limit is strictly positive, the admin address is loopback with a nonzero port, and the database URL does not appear in the serialization

### Requirement: Telemetry initializes once as structured output
The service SHALL install exactly one process-wide structured telemetry subscriber on startup, SHALL fail with a typed error when initialization is attempted twice, and SHALL emit telemetry records as structured events rather than unstructured prose.

#### Scenario: Second initialization is refused
- **WHEN** telemetry initialization succeeds and is invoked again in the same process
- **THEN** the second call returns a typed telemetry error instead of replacing or panicking

### Requirement: Operator routes report liveness, readiness, metrics, and version
While the process serves, `/live` SHALL return success, `/ready` SHALL return failure until storage startup completes and failure again once draining begins, `/metrics` SHALL return a Prometheus text body, and `/version` SHALL return the running version; every operator response SHALL carry `Cache-Control: no-store`.

#### Scenario: Readiness follows startup and drain
- **WHEN** a freshly built operator router receives requests across starting, ready, and draining lifecycle states
- **THEN** `/live` stays successful throughout, `/ready` fails while starting and draining and succeeds only between them, `/metrics` and `/version` succeed, and each response carries the no-store header

### Requirement: The owned schema applies idempotently and exclusively
Applying the current schema definition to an empty database SHALL create only `github_catalog` objects from one editable definition, SHALL succeed identically when applied again, and SHALL create no objects in any other schema.

#### Scenario: Schema applies twice without foreign objects
- **WHEN** the schema is applied to a fresh disposable database and applied a second time
- **THEN** both applications succeed, the `github_catalog` tables exist, and no table exists outside `github_catalog`, `information_schema`, and `pg_catalog`

### Requirement: The process validates configuration before serving and stops gracefully
The binary SHALL support a mode that validates configuration and exits successfully without binding any port, SHALL reach readiness when started with valid environment configuration and a reachable database, and SHALL stop within a bounded interval after SIGTERM.

#### Scenario: Check-config validates without serving
- **WHEN** the binary runs its check-config mode with valid configuration
- **THEN** it exits successfully and binds no port

#### Scenario: Boot reaches readiness and drains on SIGTERM
- **WHEN** the binary starts against a disposable database and receives SIGTERM after readiness
- **THEN** `/ready` succeeds before the signal, `/live` and `/version` respond successfully while serving, unknown paths fail, and the process exits within the shutdown bound

### Requirement: The gate and the documented commands are one list
The repository SHALL carry `.github/workflows/ci.yml` whose Cargo command list is identical to the fenced gate block in DEVELOPMENT.md, alongside the lint configuration that carries the size limits (`clippy.toml`) and the dependency policy (`deny.toml`).

#### Scenario: CI list matches the documented gate
- **WHEN** the `cargo` invocation lines are extracted from the `gate` job of `.github/workflows/ci.yml` and from the documented fence in DEVELOPMENT.md
- **THEN** the two lists are identical
