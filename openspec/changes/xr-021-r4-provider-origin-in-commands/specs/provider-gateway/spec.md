## ADDED Requirements

### Requirement: Every provider call uses the configured origin

The server, `reconnect-pat` and `shadow-legacy` SHALL reach the provider through the origin in `RATATOSKR__PROVIDER__BASE_URL`, which is `https://api.github.com` when unset, and no command SHALL name a provider origin of its own.

#### Scenario: A replacement PAT is verified against the configured origin

- **WHEN** `reconnect-pat` runs with `RATATOSKR__PROVIDER__BASE_URL` set to a loopback HTTP origin and a PAT on standard input
- **THEN** that origin receives `GET /user` carrying the PAT as a bearer credential and the account becomes `connected`

#### Scenario: The legacy shadow snapshots use the configured origin

- **WHEN** `shadow-legacy` runs with `RATATOSKR__PROVIDER__BASE_URL` set to a loopback HTTP origin for a source with a connected account
- **THEN** that origin receives the star listing and the star-list query carrying the stored PAT, and the command succeeds

#### Scenario: The default origin is GitHub

- **WHEN** the shipped operator example is loaded without an origin
- **THEN** the configured provider origin is `https://api.github.com`
