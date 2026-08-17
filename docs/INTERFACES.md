# GitHub Catalog interfaces

## Inbound

Account connect/refresh/revoke, sync, repository add/mode/star/list/watch/policy commands; provider callbacks; schedule triggers; operation context.

## Outbound

Repository/star/list/policy events, operation progress/results, Knowledge analysis requests, and Vault desired-target events.

## Provider boundary

REST/GraphQL clients expose typed pagination, conditional request, rate-limit, retry-after, auth, and mutation results. Provider payloads remain adapters, not public contracts.

## Rules

Commands carry account/user/operation/idempotency. Full snapshot commits authority only after all pages validate. Mutations expose component results such as local catalog, provider star, list filing, and backup enrollment. Vault receives desired state without GitHub credentials. Knowledge receives README/content references, not tokens.
