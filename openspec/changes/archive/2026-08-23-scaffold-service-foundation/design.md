## Context

The tree holds intent documents only. The fleet already answers how a minimal Ratatoskr Rust service is shaped: `ratatoskr-knowledge` bootstrapped with two workspace members, a hand-rolled strict environment loader, one JSON telemetry entry point, an axum operator router, and one editable `schema.sql` applied in a single transaction under an advisory lock. `ratatoskr-workspace/docs/QUALITY_GATES.md` fixes the numbers (850-line file ratchet; clippy thresholds at the measured worst case). The development status forbids migrations and second versions, so the schema lands as one file that edits in place.

## Goals / Non-Goals

Goals:

- A running local process with operator health routes and a disposable-database test harness.
- Fleet-identical gate, lint, dependency-policy, and toolchain configuration.
- A first-version `github_catalog` schema whose placeholder tables still encode known identity rules.

Non-Goals:

- Credentials, OAuth/PAT flows, encryption keys, or any GitHub API call.
- Synchronization, star observations, list reconciliation, backup policy publication, watches, analysis requests.
- NATS, outbox/inbox workers, OTLP export, Prometheus recording beyond a stub body.
- Any `v2` surface or migration tooling.

## Decisions

- **Two-member workspace** (`crates/catalog`, `services/catalog`) instead of extractor-style crate-per-concern: this mirrors how `ratatoskr-knowledge` bootstrapped; splitting concerns into separate crates is cheap to do later once account/sync/policy code exists and the seams are real rather than speculative. Crate names carry the bounded context (`ratatoskr-github-catalog`, service binary `ratatoskr-github-catalog`) because `ratatoskr-github` alone would collide conceptually with provider-facing names planned for later adapters.
- **Hand-rolled strict loader over figment**: the closed-key whitelist with per-entry validation gives value-free diagnostics and a `deny_unknown_fields` equivalent for free, matches knowledge, and keeps the dependency tree small. Environment scanning uses `std::env::vars_os`; direct `std::env::var`/`var_os` reads are banned by `clippy.toml` except at site-level `#[expect]`s in test-only database location helpers.
- **JSON-only telemetry without OTLP**: one `tracing_subscriber` JSON layer installed via `try_init`, typed `TelemetryError` on double install. Exporters arrive when there is a consumer; adding them now would be configuration for an unstarted milestone.
- **Schema placeholders with real constraints**: tables are skeletal (identity column, timestamp, status vocabulary) but each carries its primary key plus the checks that are already decided — stable GitHub numeric ID uniqueness on repositories, alias uniqueness, star-state evidence naming (`observed_unstarred_at`), outbox/inbox subject vocabularies from README, backup-policy state vocabulary. Placeholder bodies will be edited in place by later changes; nothing here may imply migration history.
- **Advisory-lock constant**: `0x7261_7461_736b_7203` — the fleet's `0x7261_7461_736b_72..` prefix ("ratatoskr" hex) with sequence number 03 (01 platform, 02 extractor, 04 knowledge).
- **Local database port 5435**: platform's compose occupies 5432, extractor's 5434; github's `compose.yaml` binds `127.0.0.1:5435` so two stacks can run side by side. CI uses its own service container on 5432 with `GITHUB_CATALOG_TEST_DATABASE_URL`.
- **Exact-pinned dependencies** (`=`) like extractor/knowledge, committed lock file, every CI command `--locked`.

## Risks / Trade-offs

- [Placeholder schema invites premature detail] → Keep columns to identity/status/timestamps only; later plan items own their columns and edit this file.
- [Boot test depends on a reachable PostgreSQL] → The harness creates and drops a uniquely named database; compose.yaml documents the one command needed locally, CI provisions its own container.
- [Strict unknown-key rejection can break operators mid-rollout] → Deliberate: silent typo tolerance hides misconfiguration; the error names the key to remove.

## Migration Plan

None. Development status: no data survives a schema change; the schema definition is created once and edited thereafter.

## Open Questions

None.
