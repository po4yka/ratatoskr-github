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
    created_at             timestamptz not null default now(),
    updated_at             timestamptz not null default now()
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
    started_at  timestamptz not null default now(),
    finished_at timestamptz,
    constraint sync_runs_mode_check check (mode in ('incremental', 'full')),
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

create table if not exists github_catalog.star_lists (
    list_id          uuid primary key,
    account_id       uuid not null references github_catalog.github_accounts (account_id),
    provider_list_id text not null,
    name             text not null,
    created_at       timestamptz not null default now(),
    constraint star_lists_provider_identity_key unique (account_id, provider_list_id)
);

create table if not exists github_catalog.star_list_memberships (
    list_id       uuid not null references github_catalog.star_lists (list_id),
    repository_id uuid not null references github_catalog.repositories (repository_id),
    added_at      timestamptz not null,
    primary key (list_id, repository_id)
);

create table if not exists github_catalog.repository_watches (
    watch_id     uuid primary key,
    repository_id uuid not null references github_catalog.repositories (repository_id),
    trigger_type text not null,
    enabled      boolean not null default true,
    created_at   timestamptz not null default now(),
    constraint repository_watches_trigger_type_check check (trigger_type in (
        'readme_changed',
        'release_published',
        'issue_opened',
        'archived_or_deleted',
        'visibility_changed',
        'activity_threshold'
    ))
);

-- Desired backup policy only: this context never owns physical backup state.
create table if not exists github_catalog.backup_policies (
    backup_policy_id uuid primary key,
    repository_id    uuid not null unique references github_catalog.repositories (repository_id),
    policy_level     text not null,
    pinned           boolean not null default false,
    updated_at       timestamptz not null default now(),
    constraint backup_policies_level_check check (policy_level in (
        'none',
        'metadata_only',
        'git_mirror',
        'git_mirror_with_lfs',
        'complete_archive'
    ))
);

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
        'vault.target.desired.v1',
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
        'vault.target.desired.v1',
        'knowledge.repository_analysis.requested.v1'
    )),
    constraint inbox_payload_is_object check (jsonb_typeof(payload) = 'object')
);
