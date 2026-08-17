# Developing Ratatoskr GitHub

> Status: Proposed  
> Last reviewed: 2026-08-17

Architecture bootstrap: the Rust service, provider client, OAuth/PAT flows, migrations, and sync workers are not implemented.

## Intended toolchain

Rust/Tokio, Reqwest/Rustls, SQLx/PostgreSQL, GraphQL where required, NATS JetStream, typed encrypted credentials, WireMock/provider fixtures, tracing, and testcontainers.

## Workflow

1. Identify provider capability, required scope, mutation/consent, rate-limit cost, and authority semantics.
2. Preserve GitHub numeric IDs and treat names as mutable aliases.
3. Never infer removal from an incomplete scan.
4. Add pagination, checkpoint, redelivery, partial-success, and rate-limit tests.
5. Delegate Git execution to Vault and analysis to Knowledge through contracts.

The first scaffold PR must specify exact build/test/migration/local-server commands. Default tests use recorded/synthetic fixtures, never personal tokens.
