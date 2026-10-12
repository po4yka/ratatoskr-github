## 1. The operator commands use the configured origin (R4-09)

- [x] 1.1 RED: add `services/catalog/tests/operator_commands.rs::reconnect_pat_verifies_the_pat_against_the_configured_provider_origin` and `::shadow_legacy_snapshots_through_the_configured_provider_origin`; both fail because the commands connect to `api.github.com`, so `reconnect-pat` exits with "provider credential verification failed" and the wiremock origin receives no request.
- [x] 1.2 GREEN: replace the two literals by one `provider_gateway(config)` that reads `config.provider.base_url`.

## 2. The example documents the key (R4-09)

- [x] 2.1 Add the commented `RATATOSKR__PROVIDER__BASE_URL` line with its default and the loopback-only HTTP rule to `deploy/systemd/github.conf.example`, assert it in `services/catalog/tests/deployment_profile.rs`, and say in `DEVELOPMENT.md` that the commands honour it; this task has no failing test because it is documentation and configuration.
