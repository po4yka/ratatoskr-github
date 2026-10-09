## Context

S12 of XR-021 CONTRACTS.md fixes the predicate, the collection rules and the ledger. This design records the local shape only.

## Decisions

- `account_erasure_state.rs` owns `OWNER_KEYED_PAYLOAD`, the `OWNER_REFERENCES` list (tables that keep a repository alive) and `CATALOG_CHILDREN` (tables that hang off a repository and go with it), plus `delete_owner_state`. A census test over `pg_constraint` fails when a foreign key into `repositories` is in neither list.
- Candidates are captured into a temp table (`on commit drop`) inside the erasure transaction before any owner row is deleted; after the owner deletes, a candidate is collected only when no `OWNER_REFERENCES` row remains.
- The ledger insert and the owner deletion are one transaction. `insert ... on conflict (operation_id) do nothing` affecting zero rows rolls back and returns the stored row, so two concurrent redeliveries converge on one outcome.
- The owner digest is the SHA-256 hex of the owner ref so the ledger holds no tenant identifier.
- Already-revoked status of `DELETE /applications/{client_id}/grant` is confirmed against GitHub's REST documentation before the revoke change (task 5.1) and the documented status is used.
