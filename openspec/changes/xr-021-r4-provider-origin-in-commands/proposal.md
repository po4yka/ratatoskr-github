## Why

Changeset XR-021 round 4 (CONTRACTS.md R4-09) found that `RATATOSKR__PROVIDER__BASE_URL`, a documented and validated key that the server honours, is silently ignored by two operator commands. `reconnect-pat` and `shadow-legacy` built their gateway from the literal `https://api.github.com`, so a replacement PAT could never be registered against another origin (a loopback fake in a native smoke, a GitHub Enterprise origin) and the legacy shadow snapshots could not be taken against one.

## What Changes

- **Deliberate break**: `provider_gateway` in `services/catalog/src/main.rs` no longer takes a base URL argument. It builds the gateway from `config.provider.base_url` only, so the server and both commands share one origin and the literal is deleted. An operator who set `RATATOSKR__PROVIDER__BASE_URL` and relied on these two commands still reaching `api.github.com` now reaches the configured origin; the default is unchanged.
- `deploy/systemd/github.conf.example` states the key as a commented line with its default and the loopback-only HTTP rule.

## Capabilities

### Modified Capabilities

- `provider-gateway`: every operator command and the server reach the provider through the one configured origin.

## Impact

`services/catalog/src/main.rs`, `services/catalog/tests/operator_commands.rs`, `services/catalog/tests/deployment_profile.rs`, `deploy/systemd/github.conf.example`, `DEVELOPMENT.md`. Cross-repository behaviour is not involved; the change cites XR-021 CONTRACTS.md R4-09 and does not restate it.
