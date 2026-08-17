# GitHub Catalog testing strategy

Required tests:

- OAuth/PAT scope, encryption, refresh/revoke, and account binding.
- Repository rename/transfer/fork/archive/deletion with stable numeric identity.
- Multi-page incremental and full snapshots, interruption, duplicate pages, checkpoints, and false-removal prevention.
- Star-list reconciliation and membership authority.
- `metadata`/`track`/`star` workflows and partial-success matrices.
- Idempotent provider mutations, retries, rate-limit/conditional request behavior.
- Desired Vault policy and Knowledge request contracts.
- SQL migrations, outbox/inbox redelivery, authorization, and secret-redaction.
- Legacy import/shadow comparison with synthetic data.

Default tests use WireMock/fixtures and no personal GitHub token. An opt-in sandbox suite may use a dedicated test account.
