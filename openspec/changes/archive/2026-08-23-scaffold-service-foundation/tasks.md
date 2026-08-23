## 1. Workspace scaffold

- [x] 1.1 Create root `Cargo.toml` (two members: `crates/catalog`, `services/catalog`), `rust-toolchain.toml` pinning 1.97.0, `clippy.toml`, `deny.toml`, `rustfmt.toml`, `.editorconfig`, member manifests with workspace lints and exact-pinned dependencies, and empty lib/bin skeletons that compile. Verification: `cargo check --workspace --locked` succeeds. This task cannot start from a failing test: it creates configuration and manifest files only.
- [x] 1.2 Add `compose.yaml` (postgres:17 on 127.0.0.1:5435) and start it; verify `pg_isready -h 127.0.0.1 -p 5435` succeeds. This task cannot start from a failing test: local infrastructure provisioning.

## 2. Configuration strictness

- [x] 2.1 Add `crates/catalog/tests/config.rs` with tests for: defaults are finite and loopback; serialization omits the database URL; an unknown key (`RATATOSKR__LIMITS__MYSTERY`) is refused without echoing its value; a non-positive timeout value is refused without echoing its value; a malformed database URL is refused; a recognized override changes exactly its own field. Confirm they fail against a permissive skeleton loader that ignores unknown keys and echoes nothing about validation. Verification: `cargo test -p ratatoskr-github-catalog --test config --locked` fails on the stated assertions.
- [x] 2.2 Implement the strict closed-key loader in `crates/catalog/src/config.rs` until the suite passes. Verification: same command green.

## 3. Telemetry bootstrap

- [x] 3.1 Add `crates/catalog/tests/telemetry.rs`: initialization succeeds once and a second call in the same process returns the typed already-installed error. Confirm it fails while `init_telemetry` is unimplemented. Verification: `cargo test -p ratatoskr-github-catalog --test telemetry --locked` fails to provide the passing behavior.
- [x] 3.2 Implement `crates/catalog/src/telemetry.rs` (JSON subscriber via `try_init`, typed `TelemetryError`). Verification: same command green.

## 4. Owned schema and disposable-database harness

- [x] 4.1 Add `crates/catalog/src/database.rs` (`Database::connect`, `apply_schema` under advisory lock, typed `PersistenceError`) and the `test-support` feature exposing `TestDatabase`; add `crates/catalog/tests/schema.rs` asserting: application succeeds into an empty database, applying twice succeeds identically, expected `github_catalog` tables exist, and no table exists outside `github_catalog`/`information_schema`/`pg_catalog`. Start `schema.sql` at the bare `create schema` statement so the run fails because the tables are absent. Verification: `cargo test -p ratatoskr-github-catalog --test schema --locked` fails on missing tables.
- [x] 4.2 Write the first-version `schema.sql`: `github_catalog` placeholder tables (`github_accounts`, `repositories`, `repository_aliases`, `star_observations`, `current_star_state`, `star_lists`, `star_list_memberships`, `repository_watches`, `backup_policies`, `sync_runs`, `sync_checkpoints`, `outbox_events`, `inbox_events`) with primary keys, identity/status checks, and unique constraints per README rules. Verification: same command green.

## 5. Operator routes

- [x] 5.1 Add `services/catalog/tests/admin.rs`: `/live` stays successful across starting/ready/draining; `/ready` fails while starting and draining, succeeds between; `/metrics` returns a Prometheus text body; `/version` returns the package version; every response carries `Cache-Control: no-store`. Confirm it fails against a router that serves only constant success responses. Verification: `cargo test -p ratatoskr-github-catalog-service --test admin --locked` fails on the readiness transitions.
- [x] 5.2 Implement the `Lifecycle` state machine and `admin_router` in `services/catalog/src` until green. Verification: same command green.

## 6. Process boot

- [x] 6.1 Add `services/catalog/tests/boot.rs`: spawn the real binary against a disposable database with env configuration; assert check-config exits successfully binding no port; assert `/ready` reaches 200; assert `/live`, `/metrics`, `/version` return 200 and an unknown path 404 while serving; send SIGTERM and assert exit within the shutdown bound. Confirm it fails because the process does not serve yet. Verification: `cargo test -p ratatoskr-github-catalog-service --test boot --locked` fails with "readiness did not arrive" or equivalent.
- [x] 6.2 Implement `services/catalog/src/main.rs`: config load with `check-config` mode, telemetry init, database connect and schema application, bound listener, `mark_ready`, graceful SIGTERM drain within the configured bound. Verification: same command green.

## 7. Gates and documentation

- [x] 7.1 Add `.github/workflows/ci.yml` (postgres service container, pinned actions, fetch/deny/fmt/clippy/build/test/doc-test/release/file-ratchet steps plus the ci-versus-DEVELOPMENT.md drift guard) and the matching fenced gate block under "### Rust - the CI gate" in DEVELOPMENT.md; update DEVELOPMENT.md status text from architecture-bootstrap to the running foundation. This task cannot start from a failing test: CI and documentation artifacts; their consistency check is the drift guard step itself.
- [x] 7.2 Update README.md status blockquote to describe the implemented foundation (service runs locally, health endpoints, owned schema) while keeping unimplemented areas marked planned. This task cannot start from a failing test: documentation.

## 8. Full gate

- [x] 8.1 Run the complete gate list from DEVELOPMENT.md against a clean tree and record the results. Verification: every command exits zero.
