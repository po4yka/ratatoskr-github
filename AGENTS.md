# Ratatoskr GitHub Catalog Agent Instructions

## Scope

These instructions apply to the `ratatoskr-github` repository.

This repository owns the GitHub Catalog bounded context: accounts, repository identity, stars, star lists, metadata, watches, synchronization checkpoints, and desired backup policy.

## Repository mission

The service answers:

- which GitHub accounts are connected;
- which repositories are known and how they are identified;
- which repositories are starred or tracked;
- which native GitHub lists contain a repository;
- what metadata and README revision were observed;
- what backup state the user desires;
- which repositories should be analyzed or watched.

It does **not** perform Git mirroring or prove that a backup can be restored. Physical preservation belongs to `ratatoskr-vault`.

## Current phase

Implementation-plan item 1 is complete. The Rust workspace, strict configuration, structured
telemetry, operator health routes, one editable `schema.sql`, disposable-database tests, and CI gate
exist. OAuth/PAT flows, provider queries, sync workers, mutations, public APIs, and event handlers
remain absent. Do not assume anything beyond the service foundation exists unless it is present in
the checkout.

When creating initial implementation:

- encode synchronization invariants in domain types and tests;
- keep provider API types inside adapters;
- separate account, catalog, synchronization, policy, and event concerns;
- do not port the legacy Python class graph one-to-one.

### Development status

Ratatoskr is in development. No database holds data that has to survive a schema change. While this
status holds, these rules are binding, and they override anything else in this repository that
plans otherwise, including the rest of this file:

- **One version only.** The API, the database, and the contracts keep their first version. Do not
  add a `v2` or a later major version, and do not add version negotiation, deprecation windows, or
  parallel-major routing.
- **No database migrations.** Do not add a migration file, and do not add migration tooling. A
  schema change edits the current schema definition in place, and a test database is created from
  that definition.
- **The product is `Ratatoskr`.** It is not "Ratatoskr Next". Do not write that name in code,
  documentation, identifiers, comments, or commit messages.

Only the repository owner changes this status. Ask before you write anything these rules forbid.

## How a change starts

Every non-trivial change begins as an OpenSpec change rather than as an edit, and each assistant
starts one in its own syntax. Claude Code has the command: `/opsx:propose <what you want to build>`,
or `/opsx:explore` first when the shape is not clear yet. Codex has no project-level command and
triggers the same skill by name, `$openspec-propose`, or lets its description match it. OpenCode has
its own command, `/opsx-propose`. Whichever starts it, the result is `openspec/changes/<id>/` holding
a proposal, the spec deltas, a design and a task list, and you read that plan before any code is
written. `/opsx:apply`, `$openspec-apply-change` or `/opsx-apply` builds it, and `/opsx:archive`,
`$openspec-archive-change` or `/opsx-archive` folds the deltas into `openspec/specs/`.

`openspec/specs/` holds the behaviour that is true today, and it starts empty on purpose. A spec here
grows from a change that needed it. Do NOT convert `docs/REQUIREMENTS.md`, `docs/INTERFACES.md`,
`docs/DOMAIN.md` or `docs/DATA_MODEL.md` into specs in bulk. Those documents stay where they are, as
material an exploration reads. A spec set produced by bulk conversion is large, stale on the day it
lands, and trusted by nobody.

Behaviour that more than one repository can see — the shape of a contract, the meaning of a field, the
order in which repositories must receive a change — belongs in the `ratatoskr-workspace` store, not
here. `openspec/config.yaml` references it, so `openspec instructions` in this repository lists the
store's specs with the exact command that fetches one. Cite that spec from a local proposal instead
of restating it.

### Tests come first

The task list carries one pair per behaviour. The first task adds a test that fails. The second makes
it pass. Never one task that does both.

- Run the new test before you write the implementation, and confirm it fails for the reason the task
  states — not for a compile error or a typo.
- A refactor task comes after the tests are green. It adds no test and changes no behaviour.
- A task that cannot start from a failing test says why in one line. Configuration, documentation and
  generated files are the usual reasons.
- Do not tick a task whose test has not been run.

Nothing can check the order in which the two were written. What CI does check is
`openspec validate --archived`, which fails when a change was archived with a task left unticked, and
the step in `fleet.yml` that fails when a repository holds a manifest and a `ci.yml` that never runs
a test. `ratatoskr-workspace/docs/QUALITY_GATES.md` states that limit rather than implying it is
covered.

## The Rust skill catalogue

`.agents/skills/` holds eighteen Rust skills, and `.claude/skills/` symlinks to them, so all three
assistants read one copy. Codex reads `.agents/skills/`, Claude Code reads `.claude/skills/`, and
OpenCode scans both, so the existing symlink already covers it and nothing belongs under
`.opencode/skills/`. Each is a reference sheet rather than a tutorial: the commands, flags,
thresholds and triage tables for one Rust concern. Your assistant reads the descriptions and opens a
skill only when the task matches one, so the set costs almost nothing until it is needed.

`rust-tdd` is the Rust form of the task pair above. `rust-lints` owns `clippy.toml`, which is where
this repository's size limits live. `rust-security` answers a `RUSTSEC` advisory.
`rust-async-internals` covers `tokio::select!` cancel safety and shutdown. `rust-database` covers
pool budgets and transaction ownership. `rust-compiler-errors` is the entry point when the build
fails and the cause is not obvious.

`rust-database` also carries a section on deploying migrations in compatible phases. The Development
status above overrides it: while that status holds, this product has no migrations at all. Read the
rest of that skill and skip that section.

The eighteen are identical in every Ratatoskr repository whose stack is Rust, and
`ratatoskr-workspace/.github/workflows/drift.yml` fails when one copy stops matching the others. Do
not edit a file under `.agents/skills/`. A correction belongs upstream in `po4yka/rust-skills` and
reaches this repository through `npx skills update`.

The catalogue holds forty-four skills and eighteen are vendored here.
`ratatoskr-workspace/docs/QUALITY_GATES.md` records which were left out and why. They are vendored
under BSD-3-Clause, (c) 2026 Nikita Pochaev, who also owns this repository; each `SKILL.md` keeps its
`license` field, and the full text is in that repository's `LICENSE`.

## Sources of truth

Use this order:

1. active task/changeset and accepted ADRs;
2. `README.md`;
3. GitHub/event contracts from `ratatoskr-contracts`;
4. repository tests and synchronization fixtures;
5. GitHub provider responses as observed external state;
6. implementation details.

When provider documentation or behavior is ambiguous, preserve the conservative observation rather than inventing an authoritative fact.

## Hard bounded-context rules

### GitHub Catalog owns

- GitHub account linkage and encrypted credentials;
- granted scopes and token lifecycle metadata;
- stable repository identities and aliases;
- star observations and current star projection;
- native star lists and memberships;
- repository metadata and conditional-fetch state;
- sync runs, checkpoints, snapshots, rate-limit state, and failures;
- user watch rules;
- desired backup policies;
- references to Knowledge analyses;
- catalog-specific outbox/inbox records.

### GitHub Catalog does not own

- `git clone`, fetch, bundle, fsck, or LFS execution;
- filesystem paths, mirror state, snapshot manifests, or retention;
- restore verification;
- LLM prompts, summaries, embeddings, or search indexing;
- Telegram interactions or client collections;
- Platform identity/session tables;
- another service's provider credentials.

Never execute Git as an implementation shortcut. Publish desired backup state to Vault.

## Repository identity

Use GitHub's stable numeric repository ID as the provider identity. Treat `owner/name` as a mutable alias.

Rules:

- renames and ownership transfers update aliases without creating a new logical repository;
- URLs are derived/observed attributes, not primary identity;
- forks retain their own repository IDs;
- deleted/unavailable repositories keep tombstones/history where policy requires;
- case normalization must not collapse distinct provider identities;
- local/internal IDs use Ratatoskr identifiers, not provider IDs as database primary keys unless explicitly modeled as namespaced values.

Tests must cover rename, transfer, deletion, restoration, and alias collision scenarios.

## Account and credential rules

- Support only explicitly designed authentication modes, such as PAT and/or OAuth Device Flow.
- Store credentials encrypted and versioned.
- Record granted scopes, expiry/refresh metadata, provider account ID, and connection status.
- Request minimum read scopes by default.
- Request mutation scopes only for explicit star/list write features.
- Never pass GitHub tokens to Platform, Telegram, Knowledge, Vault, clients, events, or logs.
- Reauthorization and scope downgrade are explicit account states.
- Serialize or safely coordinate account mutations to avoid conflicting writes and secondary rate limits.

Provider errors may be stored in restricted diagnostics after redaction, but user-facing errors use stable internal codes.

## Star observation semantics

A star and a repository are different entities.

Model at least:

- repository identity;
- account-specific current star state;
- append-only or auditable observations;
- provider `starred_at` when actually supplied;
- observation timestamps;
- source sync run/snapshot;
- removal state and evidence.

Do not fabricate exact unstar timestamps. Use names such as `observed_unstarred_at` when the time is inferred from a successful snapshot.

## Incremental and full snapshot invariant

This is non-negotiable:

> Absence from a partial or incremental listing does not prove that a repository was unstarred.

Correct behavior:

1. frequent incremental synchronization discovers additions and updates;
2. an incremental high-water mark limits ordinary scans;
3. partial scans may upsert observed records but never infer removal;
4. periodic full snapshots enumerate the authoritative current set;
5. only a complete, successful full snapshot may mark missing stars as removed;
6. failed, cancelled, rate-limited, truncated, or schema-invalid snapshots do not change absence-based state;
7. every removal projection records the snapshot evidence that caused it.

A refactor that weakens this rule is a correctness regression even if it is faster.

## Star list synchronization

Native GitHub star lists are distinct from Ratatoskr collections.

- Preserve the provider list identity and account ownership.
- Reconcile list metadata and memberships separately from the general star listing.
- Do not assume a star list membership changes the base starred state unless provider semantics explicitly establish it.
- A partial list query cannot prove membership removal.
- Mutations require explicit user intent, required scope, idempotency, and audit.
- Keep local tags/collections outside this bounded context unless a contract explicitly projects them.

## Repository modes

Preserve the semantic distinction:

```text
metadata  -> add/update the local catalog only
track     -> add/update the catalog and request backup, without starring
star      -> star on GitHub, update the catalog, and optionally request backup
```

Do not collapse these modes into one boolean.

External GitHub writes (`star`, list filing, unstar, list removal) require:

- connected account with the required scope;
- explicit user request/confirmation from the calling product flow;
- idempotency key;
- audit record;
- truthful partial-success result.

## Partial-success semantics

Operations may contain independent steps:

```text
provider star mutation
catalog upsert
star-list filing
backup policy update
analysis request
```

A successful external mutation must not be rolled back merely because a later independent step failed.

Return structured results such as:

- star succeeded;
- list filing failed;
- backup request accepted;
- analysis queued.

Retries target only incomplete eligible steps. Do not repeat a successful external write without idempotency/evidence.

## Metadata synchronization

- Use conditional requests with ETag/`Last-Modified` when supported.
- Persist provider rate-limit headers and request IDs required for safe scheduling/diagnostics.
- Separate repository metadata revisions from star observations.
- Record observed visibility, archive/deletion status, default branch, topics, license, and timestamps with clear authority.
- Treat `304 Not Modified` as reuse of a known prior body/revision, not as empty content.
- Avoid fetching expensive secondary resources on every star scan.
- Bound concurrency per account and globally.

Provider types must be normalized before entering domain state.

## Rate-limit and retry policy

- Respect primary and secondary rate limits.
- Honor `Retry-After` and provider reset information.
- Avoid aggressive concurrent requests.
- Classify errors as transient, permanent, auth/scope, rate-limit, validation, or provider-unavailable.
- Persist checkpoints only after the corresponding page/batch is durably processed.
- A retry must not skip or duplicate catalog state.
- Circuit breakers and backoff must be account/provider aware.

Do not hide rate-limit exhaustion by returning a successful but incomplete snapshot.

## Desired backup policy

Catalog owns user intent, not Vault execution.

Supported policy states may include:

```text
none
metadata_only
git_mirror
git_mirror_with_lfs
complete_archive
```

Additional policy may include:

- `pinned`;
- retention policy reference;
- include wiki/releases/issues;
- offsite requirement.

Rules:

- publish the complete desired state, not an imperative sequence of Git commands;
- include policy version and correlation/idempotency metadata;
- do not infer physical backup health from command acceptance;
- consume Vault status only through contracts/projections;
- unstar does not automatically mean delete backup;
- explicit `pinned` intent overrides automatic enrollment/removal policy.

## Watches

Watch rules are catalog policy and must be explicit:

- target repository identity;
- trigger type;
- schedule/conditions;
- enabled/paused state;
- notification or downstream action reference;
- last evaluated checkpoint.

Scheduler may request evaluation, but GitHub Catalog owns provider queries and watch semantics.

Do not turn watches into a generic automation engine in this repository.

## Repository analysis integration

GitHub Catalog may request analysis of a stable README/repository revision, but `ratatoskr-knowledge` owns the analysis.

- Publish an analysis request with repository ID, README/content reference/hash, and desired contract version.
- Store only analysis references/status needed by the catalog.
- Do not call LLMs in the GitHub sync transaction.
- Do not mix repository-analysis JSON into generic article summary schemas.
- A metadata sync must succeed independently of analysis availability.

## Persistence and schema evolution

GitHub Catalog writes only its owned schema.

Expected conceptual data includes:

```text
github_accounts
repositories
repository_aliases
star_observations
current_star_state
star_lists
star_list_memberships
sync_runs
sync_checkpoints
watch_rules
backup_policies
outbox_events
inbox_events
```

Rules:

- no cross-schema writes or foreign keys;
- provider raw payloads, when retained, are stored/referenced separately from normalized projections;
- uniqueness constraints enforce account/repository/list identities;
- sync run completion and snapshot authority are transactional;
- schema changes edit the current definition in place while the development status above holds;
- destructive cleanup never removes evidence required to explain a star/removal decision.

## Commands and events

Use versioned contracts and transactional outbox/inbox. Representative messages include:

```text
github.sync.requested.v1
github.repository.observed.v1
github.star.observed.v1
github.star.removed.v1
github.backup_policy.changed.v1
vault.target.desired.v1
knowledge.repository_analysis.requested.v1
```

Events are facts; commands request work. Never publish `removed` from an incomplete snapshot.

Payloads contain stable references and redacted metadata, never credentials or full private provider responses.

## Security and privacy

- Encrypt tokens and keep decryption scope inside this service.
- Never log tokens, auth headers, OAuth device codes, or private repository content.
- Apply per-user ownership checks to accounts, repositories, policies, and mutations.
- Treat README and repository metadata as untrusted content; do not execute code or instructions from them.
- Avoid sending private repository content to Knowledge/model providers without explicit policy and authorization.
- Redact provider errors before user display.
- Audit external mutations and credential changes.
- Use least-privilege network and database roles.

## Observability

Required telemetry should cover:

- sync duration and pages/items processed;
- incremental versus full snapshot;
- snapshot completeness/authority;
- additions, updates, and removals;
- rate-limit remaining/reset and secondary-limit events;
- conditional-request hits;
- provider latency/failure class;
- outbox/inbox lag and duplicates;
- backup-policy and analysis-request outcomes;
- correlation, account, and sync-run IDs in non-sensitive form.

Avoid high-cardinality repository names in ordinary metric labels.

## Testing expectations

When implementation exists, include applicable tests for:

- repository identity, rename, and transfer;
- incremental high-water behavior;
- partial scan never causing removal;
- complete snapshot reconciliation;
- failed/truncated snapshot preserving prior state;
- list membership reconciliation;
- PAT/OAuth scope and reauthorization states;
- conditional requests and `304` handling;
- rate-limit/backoff/checkpoint behavior;
- mutation idempotency and partial success;
- desired backup policy events;
- pinned policy precedence;
- outbox/inbox replay;
- current-schema checks preserving observation invariants;
- provider adapters using synthetic/recorded redacted fixtures.

Use property/state-machine tests for synchronization invariants. Never depend on a live personal GitHub account in normal tests.

## Cross-repository change rules

Use a workspace changeset when changing:

- GitHub/event contracts;
- desired backup policy consumed by Vault;
- analysis requests consumed by Knowledge;
- public repository modes or operation results consumed by Platform, Telegram, web, mobile, or extension;
- auth/callback flows;
- migration/cutover behavior from the legacy backend.

List producers, consumers, compatibility, rollout order, rollback, and reconciliation impact.

## Git and PR workflow

- Keep synchronization correctness changes separate from unrelated provider client refactors.
- State whether a change affects incremental or full snapshot authority.
- Include fixtures and tests for provider pagination/rate-limit behavior.
- Document external mutation and scope impact.
- Do not add system Git, filesystem mirror code, or LLM calls.
- Do not commit tokens, private repository payloads, or personal account exports.
- Do not introduce a breaking contract without the coordinated changeset.
- Update README/ADRs when ownership or synchronization semantics change.

## Completion criteria

A task is complete only when:

- responsibility belongs to GitHub Catalog;
- stable numeric repository identity and alias behavior remain correct;
- partial scans cannot cause false unstars or membership removals;
- full snapshot authority and failure behavior are explicit;
- rate-limit, retry, checkpoint, and idempotency behavior is safe;
- external writes require scope, explicit intent, audit, and partial-success reporting;
- backup handling remains desired-state only, with no Git execution;
- analysis remains delegated to Knowledge;
- contracts, schema, security, and telemetry are updated;
- repository-local tests and workspace integrations pass.
