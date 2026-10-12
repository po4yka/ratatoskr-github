## Context

See proposal.md. The behaviour is defined by XR-021 CONTRACTS.md R4-09. `validate_provider_base_url` in `crates/catalog/src/config.rs` already accepts an HTTPS origin or a numeric loopback HTTP origin and defaults to `https://api.github.com`; nothing about validation changes.

## Decisions

### D1. The gateway constructor takes the configuration, not a URL

`provider_gateway(config)` replaces `provider_gateway(config, base_url)`. A signature that cannot receive a literal origin makes the defect unrepresentable: a future command cannot pass `https://api.github.com` by hand, and there is no second place to keep in step with the key.

### D2. The tests run the real binary against a loopback provider

`operator_commands.rs` starts the binary with `RATATOSKR__PROVIDER__BASE_URL` set to a wiremock origin and a disposable database. `reconnect-pat` is asserted by the mock's verified `GET /user` with the bearer PAT and by the account becoming `connected`. `shadow-legacy` is asserted by the mock's verified `GET /user/starred` and `POST /graphql` carrying the stored PAT. The shadow test connects its account through the library so that it does not depend on the reconnect command. A unit test of `provider_gateway` would have to read the private base URL out of a `Debug` string; the process test observes the same property through the wire and also covers the command wiring.

### D3. The example documents the key

The example carries the key commented out with its default, because an uncommented line would make the shipped example differ from the default only in appearance. `deployment_profile.rs` asserts the example names the key and that an example which leaves it unset loads the GitHub default.
