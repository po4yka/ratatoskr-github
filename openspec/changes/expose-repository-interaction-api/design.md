## Context

See `proposal.md` and `specs/repository-interaction-api/spec.md`. `services/catalog` currently binds only the operator listener. Platform has already reserved `/v1/gh` -> `127.0.0.1:8092` and injects an authenticated user UUID. Catalog owns `github_accounts.owner_ref = user:<uuid>`, encrypted PAT loading, provider adapters, metadata persistence, modes, audited idempotent star mutations, and desired-policy dirtiness.

## Goals / Non-Goals

**Goals:**

- Serve the shared preview/action contract on the accepted host-local gateway boundary.
- Reuse existing domain primitives while retaining component truth when later work fails.
- Make the real binary testable against disposable PostgreSQL and a fake GitHub HTTP server.

**Non-Goals:**

- A public internet listener, new service credential, NATS request/reply, OAuth, lists, or Vault verification.
- Changing star synchronization authority or fetching README/analysis as part of preview.

## Decisions

### D1: Add a separate domain listener and router

Strict config gains `api.listen_address`, defaulting to loopback port 8092, and a provider base URL. The binary binds operator and domain listeners before readiness and serves both under one shutdown signal. Provider base URL accepts HTTPS in normal deployment and HTTP only on loopback for deterministic tests. Combining operator and user routes was rejected because operator health must remain non-public and un-authenticated.

### D2: Edge user UUID is converted to the existing owner reference

The API parses `x-ratatoskr-user-id` as UUID and queries accounts by `owner_ref = user:<uuid>`. Missing/foreign account cases collapse to safe contract errors. When exactly one owned connected account can see the repository, preview may expose its opaque account UUID and star capability; zero accounts disables star, and multiple eligible accounts report `account_selection_required` because account-selection UI is outside this slice.

### D3: Preview uses provider fetch without persistence

The preview path parses the canonical repository URL, acquires the existing rate-limit budget, loads only the selected account credential when needed, fetches normalized repository metadata, and maps it directly to the contract. It does not call `observe_repository`, because that function persists metadata, README evidence, and watch/analysis side effects.

### D4: A repository-action orchestrator owns the three outcomes

Action execution fetches/applies metadata, then performs only the mode-applicable provider star, then accepts desired-policy dirtiness when applicable. It adapts existing `set_repository_mode` and `execute_mutation` primitives, but exposes richer step evidence. The mutation path is refined so a provider-confirmed star followed by persistence failure remains distinguishable from provider failure. A boolean success wrapper was rejected because it cannot represent the acceptance criteria.

### D5: Persist request fingerprint and terminal component result

The current schema gains an action-attempt table keyed by owner plus idempotency key, with target/mode/confirmation fingerprint and strict result JSON. Exact replay returns the recorded result; conflicting reuse is refused. Inserts/updates are transactional where local state allows. If storage fails after provider confirmation, the in-flight response still reports provider success and a retry remains state-idempotent at GitHub.

### D6: HTTP errors and domain outcomes are separate

Malformed/authentication/not-found failures use the shared error envelope and HTTP status. A valid action whose provider or policy component fails still returns the action-result contract, because the failure is business truth rather than a malformed transport response.

## Risks / Trade-offs

- [Provider succeeds and all result persistence fails] -> Return observed provider truth in that response; same-key retry rechecks the idempotent provider state and attempts local convergence again.
- [Preview burns rate budget without caching] -> Use conditional/cache data only after a later measured need; correctness requires read-only preview now and the existing ledger bounds calls.
- [Multiple connected accounts cannot star from Telegram] -> Report capability unavailable with `account_selection_required`; richer selection remains in web, avoiding an implicit account choice.
- [Domain listener is exposed beyond the host] -> Validation refuses non-loopback addresses and deployment maps no external port.

## Migration Plan

1. Pin the merged additive contract.
2. Add RED-first contract/router tests, then domain orchestration and current-schema changes.
3. Run targeted tests and the full repository gate through `build-gate`.
4. Start disposable PostgreSQL and fake provider, launch the real binary, and prove readiness plus preview/action responses.
5. Merge/push GitHub before Telegram integration begins.

Rollback disables/removes the `/v1/gh` route and binary listener. Existing action audit/result rows remain harmless development data; no migration reversal and no automatic unstar are performed.
