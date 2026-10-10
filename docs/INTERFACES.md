# GitHub Catalog interfaces

## Inbound

Account connect/refresh/revoke, sync, repository add/mode/star/list/watch/policy commands; provider callbacks; schedule triggers; operation context.

## Host-local repository API

The process binds a domain listener separately from operator health. It accepts
only a valid Edge-injected `x-ratatoskr-user-id` UUID and is configured to a
loopback address (default `127.0.0.1:8092`). It is not a public authentication
surface.

| Method | Route | Effect |
|---|---|---|
| `GET` | `/v1/capabilities` | Advertises repository preview and the three action modes. |
| `POST` | `/v1/gh/repositories/preview` | Reads live name, description, stargazer count, language, stable target, and available actions. Performs no Catalog write. |
| `POST` | `/v1/gh/repositories/actions` | Applies one explicitly confirmed `metadata`, `track`, or `star` action. |

Preview accepts only `https://github.com/<owner>/<repository>` with no query,
fragment, credentials, port, or sub-resource path. An action must echo the
preview's numeric ID, full name, and canonical URL and carry an opaque
`confirmation_evidence_ref` plus `idempotency_key`; star also names the opaque
connected-account reference. Telegram/Platform owns the confirmation UX and
the evidence record; this service validates the received contract, ownership,
connection status, and provider scope before a write.

Valid actions return the shared three-component result: `metadata`,
`provider_star`, and `desired_backup`, plus an aggregate derived by the shared
contract. A provider-confirmed star is never undone because a later local step
failed. Exact owner/key replay returns the stored result without provider work;
conflicting or cross-owner key reuse returns HTTP `409`. Desired-policy
`accepted` means only accepted for publication, never backup completion or
verification.

Malformed/authentication/not-found/rate-limit/dependency failures use the
shared safe error envelope. Responses set `Cache-Control: no-store`, request
bodies are limited to 16 KiB, and no route accepts or returns a GitHub token.

## Consumed sync commands

The platform scheduler publishes this service's sync commands to `cmd.github.sync.requested.v1` under the platform command grammar (platform ADR-0005; scheduler architecture S10/S14): the fixed eight-member envelope (`command_id`, `command_type`, `requested_at`, `operation_id`, `tenant_id`, `correlation_id`, `idempotency_key`, `payload`) whose payload is the schedule row's JSON passed through verbatim. The catalog validates the envelope strictly before any effect, resolves the payload account to a connected local account, claims the delivery durably in `inbox_events` keyed by the command identity, then dispatches the requested mode - `incremental` by default, `full` for periodic reconciliation. Redelivered identities short-circuit as duplicates. An ordering gap during a commanded incremental scan chains into an immediate full rescan within the same handling.

Schedule registration is not an interface of this service: operators register the frequent-incremental and periodic-full schedules through platform's documented mechanism (see README "Scheduled synchronization" for this repository's two registration statements).

Live JetStream consumption of the sync command is a later integration changeset; it is exercised at this domain boundary until then.

## Outbound

Repository/star/list/policy events, operation progress/results, Knowledge analysis requests, and Vault desired-policy commands. Every outbox row stores the complete canonical envelope
(`EventEnvelope` or `CommandEnvelope`) under a class-prefixed subject, and the envelope id is the
row id, which the relay sends as `Nats-Msg-Id`. Two subjects are relayed:

- `evt.knowledge.repository_analysis.requested.v1`, an `EventEnvelope` around
  `RepositoryAnalysisRequested` (producer `ratatoskr-github`, aggregate `repository:<id>`,
  correlation `repository_analysis:<request id>`, tenant the owner), created for an enabled
  metadata-delta watch and for each new combined metadata and README source revision;
- `cmd.vault.backup_policy.apply_requested.v1`, a `CommandEnvelope` around
  `VaultBackupPolicyApplyRequested` (aggregate and correlation `backup_policy:<version>`, no
  tenant because the policy is catalog-wide).

## Inbound facts

Three Edge-provisioned durables on `ratatoskr_events` are verified, never created:
`evt.knowledge.repository_analysis.completed.v1` and `evt.knowledge.repository_analysis.failed.v1`
(producer `ratatoskr-knowledge`, tenant required and equal to the payload owner) settle the
matching request through the idempotent inbox, and `evt.vault.backup_policy.acknowledged.v1`
(producer `ratatoskr-vault`) records Vault's decision once. The inbox message id is the envelope
event id, so a redelivery changes nothing. Input that can never be processed is terminated; a
transient persistence failure is redelivered after two seconds.

## README bytes for Knowledge

`GET /internal/v1/readme-blobs/{sha256_hex}` on the loopback domain listener returns the stored
README bytes with `X-Content-SHA256` for the holder of
`RATATOSKR__INTERNAL__READER_SERVICE_SECRET` as a bearer token, 401 otherwise, and 404 for an
unknown digest. The route is absent while the secret is unset and is not in Edge's prefix table.

## Provider boundary

REST/GraphQL clients expose typed pagination, conditional request, rate-limit, retry-after, auth, and mutation results. Provider payloads remain adapters, not public contracts.

## Rules

Commands carry account/user/operation/idempotency. Full snapshot commits authority only after all pages validate. Mutations expose component results such as local catalog, provider star, list filing, and backup enrollment. Vault receives desired state without GitHub credentials. Knowledge receives README/content references, not tokens. Catalog does not apply Knowledge budgets or requeue decisions; `queued`/`pending` remain visible until a matching terminal fact resolves them.

Current API telemetry is process-level; the live gate proves listener readiness
and safe HTTP outcomes. Request bodies, repository URLs, owner refs, credentials,
provider error bodies, and confirmation references are not ordinary telemetry
labels or log fields.
