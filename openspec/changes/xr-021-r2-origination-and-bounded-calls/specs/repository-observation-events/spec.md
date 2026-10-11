## REMOVED Requirements

### Requirement: Committed repository source revisions publish a bounded analysis command

**Reason**: A source revision no longer appends its own analysis command. A command for the observing owner plus the watch's paced command would deliver two analyses for one track; the watch is the only origin of a request (see `repository-origination`).

**Migration**: Callers register a watch (`WatchBaseline::Unevaluated` to analyse the current state). The table `repository_analysis_publications` is removed from `schema.sql`.

### Requirement: Publication is atomic and replay-safe

**Reason**: The publication ledger is removed with the direct publication. Replay safety of a request is the deduplication key of `repository_analysis_requests` (watch, idempotency digest of the whole source revision).

**Migration**: None; no consumer read the publication ledger.
