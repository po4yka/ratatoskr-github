-- Current GitHub Catalog-owned PostgreSQL schema. Development has no migration history: a schema
-- change edits this file and test databases are created from it.
--
-- First-version placeholder bodies: each table carries its identity, status vocabulary, and the
-- invariants already decided by the bounded context. Later implementation plan items extend these
-- definitions in place; nothing here implies migration history.

create schema if not exists github_catalog;

comment on schema github_catalog is 'State owned exclusively by ratatoskr-github.';

create table if not exists github_catalog.github_accounts (
    account_id uuid primary key,
    owner_ref  text not null,
    status     text not null,
    -- Granted provider scopes as observed at connect/refresh time; mutation
    -- authorization checks capabilities against this set. Empty until the
    -- credential flows populate it.
    granted_scopes text[] not null default '{}',
    created_at timestamptz not null default now(),
    constraint github_accounts_owner_ref_check
        check (owner_ref ~ '^[a-z][a-z0-9-]{1,63}$'),
    constraint github_accounts_status_check
        check (status in ('connected', 'reauthorization_required', 'revoked'))
);

-- GitHub's numeric repository ID is the stable upstream identity; owner/name and URLs are mutable
-- aliases recorded in repository_aliases and must never serve as the primary key. Exactly one
-- repository may hold an alias value live at a time; superseded rows keep redirect history
-- resolvable after renames, transfers, or name reuse by another repository.
create table if not exists github_catalog.repositories (
    repository_id          uuid primary key,
    provider_repository_id bigint not null unique,
    -- Whose decision governs this catalog entry: auto (star-driven), tracked
    -- (explicitly kept without a star), ignored (deliberately excluded), or
    -- unclassified (null: known but never classified). Synchronization may
    -- promote only unclassified to auto; explicit modes are never overridden.
    mode                   text,
    created_at             timestamptz not null default now(),
    updated_at             timestamptz not null default now(),
    constraint repositories_mode_check
        check (mode in ('auto', 'tracked', 'ignored'))
);

create table if not exists github_catalog.repository_aliases (
    alias_id      uuid primary key,
    repository_id uuid not null references github_catalog.repositories (repository_id),
    alias_kind    text not null,
    alias_value   text not null,
    status        text not null default 'active',
    redirect_to   uuid references github_catalog.repository_aliases (alias_id),
    created_at    timestamptz not null default now(),
    constraint repository_aliases_kind_check
        check (alias_kind in ('owner_name', 'html_url', 'clone_url')),
    constraint repository_aliases_status_check
        check (status in ('active', 'superseded', 'released')),
    constraint repository_aliases_redirect_targets_alias
        check ((redirect_to is null) or (status = 'superseded'))
);

create unique index if not exists repository_aliases_live_identity_key
    on github_catalog.repository_aliases (alias_kind, alias_value)
    where status = 'active';

create index if not exists repository_aliases_value_lookup
    on github_catalog.repository_aliases (alias_kind, alias_value);

-- One current metadata projection per repository plus conditional-request state; raw observed
-- bodies live in repository_metadata_revisions and are pruned to a bounded recent window.
create table if not exists github_catalog.repository_metadata (
    repository_id    uuid primary key references github_catalog.repositories (repository_id),
    description      text,
    language         text,
    stargazers_count bigint not null,
    topics           jsonb not null default '[]'::jsonb,
    default_branch   text,
    pushed_at        timestamptz,
    provider_etag    text,
    content_hash     text not null,
    fetched_at       timestamptz not null,
    constraint repository_metadata_topics_is_array check (jsonb_typeof(topics) = 'array')
);

create table if not exists github_catalog.repository_metadata_revisions (
    revision_id   uuid primary key,
    repository_id uuid not null references github_catalog.repositories (repository_id),
    payload       jsonb not null,
    content_hash  text not null,
    observed_at   timestamptz not null,
    constraint repository_metadata_revisions_payload_is_object check (jsonb_typeof(payload) = 'object')
);

create index if not exists repository_metadata_revisions_repo_observed_idx
    on github_catalog.repository_metadata_revisions (repository_id, observed_at);

create table if not exists github_catalog.sync_runs (
    sync_run_id    uuid primary key,
    account_id     uuid not null references github_catalog.github_accounts (account_id),
    mode           text not null,
    status         text not null,
    failure_reason text,
    pages_processed integer not null default 0,
    items_observed  integer not null default 0,
    additions       integer not null default 0,
    unstars         integer not null default 0,
    -- Star-list snapshot statistics: lists and membership transitions are a
    -- separate authority from stars, so they carry their own counters instead
    -- of stretching the star columns.
    lists_observed  integer not null default 0,
    removals        integer not null default 0,
    started_at  timestamptz not null default now(),
    finished_at timestamptz,
    constraint sync_runs_mode_check check (mode in ('incremental', 'full', 'star_lists')),
    constraint sync_runs_status_check
        check (status in ('running', 'completed', 'failed', 'cancelled')),
    constraint sync_runs_finish_matches_terminal_status
        check ((status in ('completed', 'failed', 'cancelled')) = (finished_at is not null)),
    constraint sync_runs_failure_reason_needs_failed_status
        check ((status = 'failed') = (failure_reason is not null))
);

create table if not exists github_catalog.sync_checkpoints (
    checkpoint_id uuid primary key,
    sync_run_id   uuid not null references github_catalog.sync_runs (sync_run_id),
    next_page     bigint not null,
    -- Smallest provider starred_at seen so far in an incremental run; restores the
    -- monotonicity guard across a resumed run. Null for full snapshots, whose page
    -- order carries no meaning.
    boundary_starred_at timestamptz,
    -- Provider continuation token for cursor-paginated enumerations (star-list
    -- snapshots over GraphQL). Null means the first page.
    graphql_cursor text,
    recorded_at   timestamptz not null default now()
);

-- Durable per-run staging of what a full snapshot saw; consumed by the
-- authority swap and cleared when the run reaches a terminal state.
create table if not exists github_catalog.snapshot_items (
    sync_run_id            uuid not null references github_catalog.sync_runs (sync_run_id),
    position               bigint not null,
    provider_repository_id bigint not null,
    provider_starred_at    timestamptz,
    primary key (sync_run_id, position)
);

-- Durable per-run staging of what a star-list snapshot saw: one flat row per
-- observed membership with its list identity denormalized; consumed by the
-- list authority swap and cleared when the run reaches a terminal state.
create table if not exists github_catalog.list_snapshot_items (
    sync_run_id            uuid not null references github_catalog.sync_runs (sync_run_id),
    position               bigint not null,
    provider_list_id       text not null,
    list_name              text not null,
    provider_repository_id bigint not null,
    primary key (sync_run_id, position)
);

-- Observations are append-only evidence; the projection lives in current_star_state.
create table if not exists github_catalog.star_observations (
    observation_id       uuid primary key,
    account_id           uuid not null references github_catalog.github_accounts (account_id),
    repository_id        uuid not null references github_catalog.repositories (repository_id),
    starred              boolean not null,
    provider_starred_at  timestamptz,
    observed_at          timestamptz not null,
    constraint star_observations_provider_time_is_evidence
        check ((starred = false) or (provider_starred_at is not null))
);

-- The exact upstream unstar time is unknown, so removal evidence uses observed_unstarred_at and
-- an unstarred state must carry the snapshot that established it. A starred state must carry the
-- provider starred-at that established it; confirmations never overwrite an established value.
create table if not exists github_catalog.current_star_state (
    account_id            uuid not null references github_catalog.github_accounts (account_id),
    repository_id         uuid not null references github_catalog.repositories (repository_id),
    starred               boolean not null,
    starred_at            timestamptz,
    last_observed_at      timestamptz not null,
    observed_unstarred_at timestamptz,
    evidence_run_id       uuid references github_catalog.sync_runs (sync_run_id),
    primary key (account_id, repository_id),
    constraint current_star_state_removal_evidence_check
        check ((starred = true) or (observed_unstarred_at is not null)),
    constraint current_star_state_starred_at_presence_check
        check ((starred = false) or (starred_at is not null))
);

-- Incremental scans ingest only what is newer than the account's high-water mark; the mark moves
-- only when a scan durably proves coverage of everything newer than it. A completed full snapshot
-- re-anchors it to the newest observed starred-at.
create table if not exists github_catalog.star_watermarks (
    account_id      uuid primary key references github_catalog.github_accounts (account_id),
    high_water_mark timestamptz not null,
    updated_at      timestamptz not null default now()
);

-- Drift repairs recorded by a completed full snapshot inside its authority swap: a repository
-- absent while locally starred, or present again while locally unstarred. One row per drifted
-- repository per completing run; repetition on converged state writes nothing.
create table if not exists github_catalog.reconciliation_repairs (
    sync_run_id   uuid not null references github_catalog.sync_runs (sync_run_id),
    repository_id uuid not null references github_catalog.repositories (repository_id),
    action        text not null,
    recorded_at   timestamptz not null default now(),
    primary key (sync_run_id, repository_id),
    constraint reconciliation_repairs_action_check
        check (action in ('unstar_after_drift', 'restore_after_miss'))
);

-- Native provider lists: the stable GraphQL node id is the provider identity;
-- a list that disappears upstream is tombstoned with evidence, never deleted.
create table if not exists github_catalog.star_lists (
    list_id          uuid primary key,
    account_id       uuid not null references github_catalog.github_accounts (account_id),
    provider_list_id text not null,
    name             text not null,
    status           text not null default 'active',
    observed_removed_at timestamptz,
    evidence_run_id  uuid references github_catalog.sync_runs (sync_run_id),
    created_at       timestamptz not null default now(),
    constraint star_lists_provider_identity_key unique (account_id, provider_list_id),
    constraint star_lists_status_check check (status in ('active', 'removed')),
    constraint star_lists_removal_evidence_check
        check ((status = 'active') or (observed_removed_at is not null))
);

-- The current membership projection: rows persist across removals so every
-- transition stays explainable. The provider supplies no per-item added-at,
-- so membership timing is modeled purely as observation times.
create table if not exists github_catalog.star_list_memberships (
    list_id       uuid not null references github_catalog.star_lists (list_id),
    repository_id uuid not null references github_catalog.repositories (repository_id),
    member        boolean not null,
    last_observed_at timestamptz not null,
    observed_removed_at timestamptz,
    evidence_run_id uuid references github_catalog.sync_runs (sync_run_id),
    primary key (list_id, repository_id),
    constraint star_list_memberships_removal_evidence_check
        check ((member = true) or (observed_removed_at is not null))
);

-- Observations are append-only membership evidence; the projection lives in
-- star_list_memberships. Every completed enumeration appends one row per seen
-- membership and one row per evidenced removal.
create table if not exists github_catalog.star_list_membership_observations (
    observation_id  uuid primary key,
    list_id         uuid not null references github_catalog.star_lists (list_id),
    repository_id   uuid not null references github_catalog.repositories (repository_id),
    member          boolean not null,
    observed_at     timestamptz not null,
    evidence_run_id uuid references github_catalog.sync_runs (sync_run_id)
);

create table if not exists github_catalog.repository_watches (
    watch_id                      uuid primary key,
    owner_ref                     text not null,
    repository_id                 uuid not null references github_catalog.repositories (repository_id),
    trigger_type                  text not null,
    downstream_action             text not null,
    enabled                       boolean not null default true,
    last_evaluated_content_hash   text not null,
    created_at                    timestamptz not null default now(),
    updated_at                    timestamptz not null default now(),
    constraint repository_watches_owner_ref_check
        check (owner_ref ~ '^user:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'),
    constraint repository_watches_trigger_type_check check (trigger_type in (
        'metadata_changed'
    )),
    constraint repository_watches_action_check check (downstream_action = 'repository_analysis'),
    constraint repository_watches_identity_key
        unique (owner_ref, repository_id, trigger_type, downstream_action)
);

create table if not exists github_catalog.repository_analysis_requests (
    request_id                      uuid primary key,
    watch_id                        uuid not null references github_catalog.repository_watches (watch_id),
    owner_ref                       text not null,
    repository_id                   uuid not null references github_catalog.repositories (repository_id),
    github_repository_numeric_id    bigint not null,
    source_revision                 jsonb not null,
    repository_attributes           jsonb not null,
    request_payload                 jsonb not null,
    attributes_digest_hex           text not null,
    idempotency_digest_hex          text not null,
    requested_contract              text not null,
    status                          text not null default 'queued',
    not_before                      timestamptz not null,
    outbox_message_id               uuid unique,
    analysis_result_ref             text,
    failure_code                    text,
    retryable                       boolean,
    terminal_at                     timestamptz,
    created_at                      timestamptz not null default now(),
    constraint repository_analysis_requests_owner_ref_check
        check (owner_ref ~ '^user:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'),
    constraint repository_analysis_requests_numeric_id_check check (github_repository_numeric_id > 0),
    constraint repository_analysis_requests_attributes_digest_check
        check (attributes_digest_hex ~ '^[0-9a-f]{64}$'),
    constraint repository_analysis_requests_idempotency_digest_check
        check (idempotency_digest_hex ~ '^[0-9a-f]{64}$'),
    constraint repository_analysis_requests_contract_check
        check (requested_contract = 'repository_analysis'),
    constraint repository_analysis_requests_json_check
        check (jsonb_typeof(source_revision) = 'object'
            and jsonb_typeof(repository_attributes) = 'object'
            and jsonb_typeof(request_payload) = 'object'),
    constraint repository_analysis_requests_status_check
        check (status in ('queued', 'pending', 'completed', 'failed')),
    constraint repository_analysis_requests_terminal_check check (
        (status = 'queued' and outbox_message_id is null and analysis_result_ref is null
            and failure_code is null and retryable is null and terminal_at is null)
        or (status = 'pending' and outbox_message_id is not null and analysis_result_ref is null
            and failure_code is null and retryable is null and terminal_at is null)
        or (status = 'completed' and outbox_message_id is not null and analysis_result_ref is not null
            and failure_code is null and retryable is null and terminal_at is not null)
        or (status = 'failed' and outbox_message_id is not null and analysis_result_ref is null
            and failure_code is not null and retryable is not null and terminal_at is not null)
    ),
    constraint repository_analysis_requests_deduplication_key
        unique (watch_id, attributes_digest_hex, requested_contract)
);

create index if not exists repository_analysis_requests_pending_idx
    on github_catalog.repository_analysis_requests (not_before, request_id)
    where status = 'queued';

create table if not exists github_catalog.repository_analysis_dispatch_cursor (
    scope          text primary key check (scope = 'repository_analysis'),
    next_not_before timestamptz not null
);

create table if not exists github_catalog.repository_analysis_links (
    owner_ref            text not null,
    repository_id        uuid not null references github_catalog.repositories (repository_id),
    request_id           uuid not null unique references github_catalog.repository_analysis_requests (request_id),
    analysis_result_ref  text not null,
    completed_at         timestamptz not null,
    primary key (owner_ref, repository_id),
    constraint repository_analysis_links_owner_ref_check
        check (owner_ref ~ '^user:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$')
);

-- Desired backup policy only: this context never owns physical backup state.
create table if not exists github_catalog.backup_policies (
    backup_policy_id uuid primary key,
    repository_id    uuid not null unique references github_catalog.repositories (repository_id),
    policy_level     text not null,
    pinned           boolean not null default false,
    mirror_cadence   text not null default 'daily' check (mirror_cadence in ('eager', 'daily', 'weekly')),
    priority_hint    text not null default 'standard' check (priority_hint in ('critical', 'standard', 'bulk')),
    size_hint_bytes  bigint check (size_hint_bytes is null or size_hint_bytes > 0),
    exclusions       jsonb not null default '[]'::jsonb check (jsonb_typeof(exclusions) = 'array'),
    updated_at       timestamptz not null default now(),
    constraint backup_policies_level_check check (policy_level in (
        'none',
        'metadata_only',
        'git_mirror',
        'git_mirror_with_lfs',
        'complete_archive'
    ))
);

-- One cursor serializes whole-catalog desired-policy versions and keeps the trailing debounce
-- durable across restart. Policy bodies are immutable audit evidence, never actual backup state.
create table if not exists github_catalog.backup_policy_publication_cursor (
    scope text primary key check (scope = 'catalog'),
    dirty_generation bigint not null default 0,
    published_generation bigint not null default 0,
    not_before timestamptz,
    last_policy_version bigint not null default 0,
    last_fingerprint text,
    constraint backup_policy_cursor_generations check (published_generation <= dirty_generation)
);
create table if not exists github_catalog.backup_policy_publications (
    policy_version bigint primary key check (policy_version > 0),
    fingerprint text not null unique,
    document jsonb not null check (jsonb_typeof(document) = 'object'),
    created_at timestamptz not null default now()
);
create table if not exists github_catalog.backup_policy_feedback (
    message_id uuid primary key,
    acknowledged_policy_version bigint not null check (acknowledged_policy_version > 0),
    outcome text not null check (outcome in ('accepted', 'rejected')),
    last_applied_policy_version bigint not null check (last_applied_policy_version >= 0),
    reasons jsonb not null default '[]'::jsonb check (jsonb_typeof(reasons) = 'array'),
    received_at timestamptz not null default now()
);

-- Append-only audit of every repository-mode transition and provider mutation
-- attempt: who requested it, through which calling source, what was targeted,
-- how it ended. One successful outcome per idempotency key makes replays
-- converge on the recorded truth; failed attempts leave the key free for a
-- retry. Credential material never enters this trail.
create table if not exists github_catalog.mutation_audit (
    audit_id        uuid primary key,
    idempotency_key text not null,
    -- The acting account as the attempt claimed it. Deliberately free of a
    -- foreign key: a refusal must stay auditable even when no such account
    -- exists, and the trail records claims rather than vouching for them.
    account_id      uuid not null,
    repository_id   uuid not null references github_catalog.repositories (repository_id),
    list_id         uuid references github_catalog.star_lists (list_id),
    operation_kind  text not null,
    principal       text not null,
    source          text not null,
    outcome         text not null,
    detail          jsonb not null default '{}'::jsonb,
    created_at      timestamptz not null default now(),
    constraint mutation_audit_operation_kind_check check (operation_kind in (
        'star',
        'unstar',
        'list_member_add',
        'list_member_remove',
        'mode_set'
    )),
    constraint mutation_audit_source_check check (source in ('telegram', 'web')),
    constraint mutation_audit_outcome_check
        check (outcome in ('applied', 'already_applied', 'rejected', 'failed')),
    constraint mutation_audit_detail_is_object check (jsonb_typeof(detail) = 'object')
);

-- The replay contract: a retried operation with a consumed key must find the
-- recorded successful outcome instead of executing twice. Failures do not
-- occupy keys, so a retry after failure can complete.
create unique index if not exists mutation_audit_one_success_per_key
    on github_catalog.mutation_audit (idempotency_key)
    where outcome in ('applied', 'already_applied');

create index if not exists mutation_audit_key_lookup
    on github_catalog.mutation_audit (idempotency_key);

create table if not exists github_catalog.outbox_events (
    message_id   uuid primary key,
    subject      text not null,
    payload      jsonb not null,
    created_at   timestamptz not null default now(),
    published_at timestamptz,
    constraint outbox_subject_is_known check (subject in (
        'github.sync.requested.v1',
        'github.repository.observed.v1',
        'github.star.observed.v1',
        'github.star.removed.v1',
        'github.backup_policy.changed.v1',
        'cmd.vault.target.desired.v1',
        'knowledge.repository_analysis.requested.v1'
    )),
    constraint outbox_payload_is_object check (jsonb_typeof(payload) = 'object')
);

create table if not exists github_catalog.inbox_events (
    message_id  uuid primary key,
    subject     text not null,
    payload     jsonb not null,
    created_at  timestamptz not null default now(),
    consumed_at timestamptz,
    constraint inbox_subject_is_known check (subject in (
        'github.sync.requested.v1',
        'github.repository.observed.v1',
        'github.star.observed.v1',
        'github.star.removed.v1',
        'github.backup_policy.changed.v1',
        'evt.vault.backup_policy.acknowledged.v1',
        'knowledge.repository_analysis.requested.v1',
        'knowledge.repository_analysis.completed.v1',
        'knowledge.repository_analysis.failed.v1'
    )),
    constraint inbox_payload_is_object check (jsonb_typeof(payload) = 'object')
);
