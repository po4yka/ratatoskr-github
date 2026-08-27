# GitHub Catalog testing strategy

Required tests:

- OAuth/PAT scope, encryption, refresh/revoke, and account binding.
- Repository rename/transfer/fork/archive/deletion with stable numeric identity.
- Multi-page incremental and full snapshots, interruption, duplicate pages, checkpoints, and false-removal prevention.
- Star-list reconciliation and membership authority.
- `metadata`/`track`/`star` workflows and partial-success matrices.
- Idempotent provider mutations, retries, rate-limit/conditional request behavior.
- Desired Vault policy and Knowledge request contracts.
- Current-schema application, outbox/inbox redelivery, authorization, and secret-redaction.
- Legacy import/shadow comparison with synthetic data.

Default tests use WireMock/fixtures and no personal GitHub token. An opt-in sandbox suite may use a dedicated test account.

`services/catalog/tests/live_repository_api.rs` is the repository API smoke:
it creates a disposable database from the current schema, starts the real
service binary on reserved loopback ports, calls readiness/capabilities/preview,
and drives a confirmed star against WireMock. Its injected post-provider policy
failure must produce a partial result and zero compensating unstar requests.

## Test-first

A change is planned before it is built, and the plan is a task list in which behaviour arrives in
pairs: one task adds a failing test, the next makes it pass. `openspec/config.yaml` carries that
rule, which is what puts it into every planning and implementation request rather than only into this
document.

The loop:

1. Write the test the scenario names. Run it. Confirm it fails, and read the failure — a test that
   fails because it does not compile has proved nothing about the behaviour.
2. Write the smallest change that makes it pass. Run it again.
3. Refactor only once it is green, adding no test and changing no behaviour.

Two checks stand behind this, and neither of them can see the order:

- `openspec validate --archived`, in `.github/workflows/openspec.yml`, fails when a change was
  archived with a task left unticked.
- A step in `.github/workflows/fleet.yml` fails when this repository holds a manifest and a `ci.yml`
  that never runs a test.

`ratatoskr-workspace/docs/QUALITY_GATES.md` records why the order itself is not checkable, rather
than leaving the gap to be discovered.
