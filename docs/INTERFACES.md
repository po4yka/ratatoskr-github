# GitHub Catalog interfaces

## Inbound

Account connect/refresh/revoke, sync, repository add/mode/star/list/watch/policy commands; provider callbacks; schedule triggers; operation context.

## Consumed sync commands

The platform scheduler publishes this service's sync commands to `cmd.github.sync.requested.v1` under the platform command grammar (platform ADR-0005; scheduler architecture S10/S14): the fixed eight-member envelope (`command_id`, `command_type`, `requested_at`, `operation_id`, `tenant_id`, `correlation_id`, `idempotency_key`, `payload`) whose payload is the schedule row's JSON passed through verbatim. The catalog validates the envelope strictly before any effect, resolves the payload account to a connected local account, claims the delivery durably in `inbox_events` keyed by the command identity, then dispatches the requested mode - `incremental` by default, `full` for periodic reconciliation. Redelivered identities short-circuit as duplicates. An ordering gap during a commanded incremental scan chains into an immediate full rescan within the same handling.

Schedule registration is not an interface of this service: operators register the frequent-incremental and periodic-full schedules through platform's documented mechanism (see README "Scheduled synchronization" for this repository's two registration statements).

Live JetStream subscription is a later integration changeset once credential storage exists; consumption is exercised at this domain boundary until then.

## Outbound

Repository/star/list/policy events, operation progress/results, Knowledge analysis requests, and Vault desired-target events. An enabled metadata-delta watch creates a paced
`knowledge.repository_analysis.requested.v1` outbox payload with bounded attributes and explicit
README absence; Catalog consumes matching `knowledge.repository_analysis.completed.v1` and
`knowledge.repository_analysis.failed.v1` terminal facts through its idempotent inbox.

## Provider boundary

REST/GraphQL clients expose typed pagination, conditional request, rate-limit, retry-after, auth, and mutation results. Provider payloads remain adapters, not public contracts.

## Rules

Commands carry account/user/operation/idempotency. Full snapshot commits authority only after all pages validate. Mutations expose component results such as local catalog, provider star, list filing, and backup enrollment. Vault receives desired state without GitHub credentials. Knowledge receives README/content references, not tokens. Catalog does not apply Knowledge budgets or requeue decisions; `queued`/`pending` remain visible until a matching terminal fact resolves them.
