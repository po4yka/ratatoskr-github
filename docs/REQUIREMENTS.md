# GitHub Catalog requirements

## Goals

1. Connect GitHub accounts with least-privilege credentials.
2. Maintain stable repository identity, metadata, stars, native star lists, and watches.
3. Support explicit `metadata`, `track`, and `star` modes with truthful partial success.
4. Produce desired backup policy for Vault without performing Git operations.
5. Request repository analysis from Knowledge without owning inference.

## Non-goals

Git cloning/mirroring, filesystem retention, embeddings, generic article extraction, and automatic destructive cleanup after unstar.

## Requirements

- GitHub numeric repository ID is authoritative identity; owner/name is alias history.
- Incremental scans add/update observations but never prove removals.
- Only a complete successful snapshot may mark missing stars/list memberships absent.
- Provider writes require explicit consent, idempotency, audit, and safe retries.
- Metadata/list/readme requests respect conditional requests and rate limits.
- Backup policy and actual Vault state remain distinct.

First slice: connected test account -> full star snapshot -> repository catalog -> desired mirror policy event -> operation result.
