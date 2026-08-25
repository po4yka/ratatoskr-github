# Design: add full star snapshot with atomic authority and checkpoints

## Context

The foundation provides stable numeric repository identity with alias handling (`identity.rs`), a metadata projection fed through conditional requests (`metadata.rs`, `observe.rs`), a per-token rate-limit ledger (`rate_limit.rs`), and a reqwest provider seam with injectable base URL (`provider.rs`). The schema already carries placeholder `sync_runs`, `sync_checkpoints`, `star_observations`, and `current_star_state` tables encoding decided invariants: removal evidence needs a run reference, unstar times are observations rather than facts, and observations are append-only. Development status: the schema is edited in place, no migrations.

## Goals / Non-Goals

Goals: one flow that turns a complete provider enumeration into authoritative star state, safe to interrupt, honest about failure, and cheap to resume.

Non-Goals: incremental scans and high-water marks (item 5), native star lists (item 6), event publication, external mutations, scheduling, multi-account fan-out inside one run.

## Decisions

### D1: Termination by ascending pages until an empty page

Pages are requested with an increasing `page` parameter and fixed `per_page`; exhaustion is an empty page. Alternatives: `Link: rel="next"` header walking (more adapter parsing for no behavioral gain here), GraphQL cursor pagination (item 6 territory). Cost: one extra request per scan, accepted for determinism and easy wiremock harnessing.

### D2: Durable per-page staging instead of in-memory accumulation

Each processed page inserts its items into a new `snapshot_items` staging table (run id, position, provider repository id, provider starred-at) in the same transaction as the checkpoint row. Rationale: resume after process death must reconstruct what earlier pages saw; recomputing a seen-set from metadata revisions would be indirect and lossy. The staging rows are deleted in the run's terminal transaction, so the table is empty between runs.

### D3: Authority writes happen only in the final swap transaction

Identity, aliases, metadata, staging rows, and checkpoints may land progressively - none of them is star authority. `current_star_state` and append-only `star_observations` are written exclusively inside one transaction at successful traversal end: additions become starred, continuations keep their established starred-at, absences become evidenced unstars, the run completes with statistics, staging is cleared. Readers therefore see either the whole previous authority or the whole new one.

### D4: Starred-at continuity lives on the projection

`current_star_state` gains a `starred_at timestamptz` column with a presence constraint mirroring the existing removal-evidence check. The swap applies `starred_at = coalesce(prior.starred_at, incoming.provider_starred_at)` for continuing stars, so the earliest established provider value survives every later confirmation; an unstar clears it, so a later re-star takes the fresh provider value instead of resurrecting history.

### D5: Three interruption modes, three treatments

- Budget refusal: outcome is a pause; the run stays `running`, its checkpoint stands, nothing else changes; a later call resumes the same run.
- Process death: the run row still reads `running`; resume detects the newest `running` full run for the account and continues from its latest checkpoint's next page.
- Permanent provider failure: the run terminates `failed` with the reason; a later attempt starts a fresh run (prior authority untouched). Resuming a failed run would conflate a broken page with progress already invalidated.

### D6: Provider listing joins the existing gateway seam

`GithubApi` gains a paginated starred-listing call whose reply pairs normalized items (provider id, full name, starred-at) with the response's rate-limit headers, mirroring the existing fetch/reply pattern; response types stay inside the adapter. The real client sends the `star+json` accept header so `starred_at` is supplied.

### D7: Flow composition mirrors the observe flow

A `snapshot` module composes ledger acquisition, gateway paging, identity upsert, metadata application, checkpoint persistence, and the swap, exposing structured outcomes (`Completed`, `Paused`, `Failed`) like `observe_repository` does. Tests drive it through wiremock plus `TestDatabase`, matching the established integration style.

## Risks / Trade-offs

- [Empty-page termination adds one request per scan] -> Accepted; deterministic and harness-friendly.
- [Staging rows outlive a crashed process] -> They belong to the run and are cleared by whichever transaction reaches a terminal state next; resume consumes them rather than duplicating them.
- [Two concurrent scans for one account interleave checkpoints] -> Out of scope for item 4; account mutation serialization arrives with the account work, noted here so it is not forgotten.
- [DB clock vs app clock mixing] -> All swap-side timestamps come from the database transaction, keeping comparisons internally consistent.

## Migration Plan

None. Development status: `schema.sql` is edited in place and test databases are created from it; no deployment holds data that must survive.

## Open Questions

None.
