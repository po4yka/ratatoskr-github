# star-snapshot Specification

## Purpose

Establishes authoritative per-account star state by fully enumerating GitHub's starred-repository listing, making the completed snapshot - never a partial pass - the single authority over what is starred, preserving evidence for every unstar it records, and keeping frequent incremental observation safe: bounded by a watermark, never establishing a removal, and forcing a full rescan whenever ordering coverage breaks.

## ADDED Requirements

### Requirement: Watermark-governed incremental ingestion

The catalog SHALL fetch the starred listing newest-first by provider starred-at, ingest exactly the items strictly newer than the account's persisted high-water mark, stop once an item at or below the watermark proves the remainder of the listing was already covered, upsert identity for every ingested item, and advance the watermark to the oldest ingested item's timestamp only after that ingestion is durably recorded.

#### Scenario: The scan stops after covering the newer-than-watermark window

- **WHEN** an incremental scan runs with a persisted watermark against a provider whose first page holds two items newer than the watermark followed by an older item on the next page
- **THEN** both newer items are ingested into identity and star state, the older item is not ingested, no request beyond the page carrying proof of coverage is made, and the run completes as mode incremental

#### Scenario: A whole listing newer than the watermark is covered by exhaustion

- **WHEN** every fetched item is strictly newer than the watermark and the provider reports exhaustion
- **THEN** all items are ingested and the watermark advances to the oldest ingested timestamp

#### Scenario: The watermark does not move on failure

- **WHEN** an incremental scan ends without durable success
- **THEN** the account's stored watermark keeps its previous value

### Requirement: Incremental scans never infer removals

The catalog SHALL leave every current star decision untouched by what an incremental pass did not see: an incremental scan SHALL NOT unstar, delete, or downgrade any repository regardless of absence from the fetched window, and SHALL preserve the originally established starred-at timestamp when re-ingesting an already starred repository.

#### Scenario: A repository outside the window stays starred

- **WHEN** a repository is starred under current state with a timestamp at or below the watermark and an incremental scan observes only newer items
- **THEN** the repository remains starred with unchanged timestamps, no unstar observation row appears, and its state carries no reference to the incremental run

#### Scenario: Re-ingesting a continuing star keeps its established timestamp

- **WHEN** an incremental scan ingests a repository that is already starred under current state
- **THEN** the stored starred-at remains the previously established value

### Requirement: Ordering gaps force a full rescan

The catalog SHALL treat a listed item without a provider starred-at value, and a non-monotonic starred-at sequence across consecutively fetched items including the boundary carried across a resumed run, as a gap: the incremental run terminates as failed with the reason recorded, staging rows are cleared, star authority and the watermark stay untouched, and the caller receives an outcome that requires a full rescan.

#### Scenario: A missing provider timestamp aborts the scan

- **WHEN** a fetched item within the newer-than-watermark window carries no starred_at
- **THEN** the run fails naming the ordering gap, prior authority and watermark are unchanged, and the returned outcome marks a gap requiring a full rescan

#### Scenario: An out-of-order page boundary aborts a resumed scan

- **WHEN** a resumed run fetches a page whose leading item is newer than the boundary timestamp recorded with the last checkpoint
- **THEN** the run fails naming the ordering gap and nothing observed in the offending page is ingested

### Requirement: Incremental scans require a full baseline

The catalog SHALL refuse to run an incremental scan for an account that has no persisted watermark and SHALL defer to a full snapshot instead, because coverage cannot be proven against a mark that does not exist.

#### Scenario: First synchronization for an account runs full

- **WHEN** an incremental scan is requested for an account with no watermark row
- **THEN** a full-mode snapshot run is performed instead and no incremental run row is created

### Requirement: Reconciliation records drift repairs explicitly

The catalog SHALL, inside the same transaction that swaps a completed snapshot into authority, compare the fresh enumeration against prior state and record one repair row per drifted repository: absent while locally starred records `unstar_after_drift`, present again while locally unstarred records `restore_after_miss`, keyed so repetition cannot duplicate a repair.

#### Scenario: Drifted repositories get named repair rows

- **WHEN** a completed snapshot finds one locally starred repository missing from the listing and one locally unstarred repository present again
- **THEN** the swap transaction records exactly those two repair rows bound to the completing run, alongside the normal evidenced unstar and restore effects

#### Scenario: Immediate reconciliation of converged state records nothing

- **WHEN** a second full snapshot completes immediately after a first with no upstream change between them
- **THEN** the second run records zero repairs, zero additions, and zero unstars, and current star state is identical after both runs

### Requirement: Completed snapshots re-anchor the incremental baseline

The catalog SHALL set the account's watermark from a successfully completed full snapshot to the newest provider starred-at the enumeration observed, so subsequent incremental scans resume from proven coverage; a completed empty enumeration leaves the watermark unset rather than invented.

#### Scenario: A completed snapshot establishes the next incremental start

- **WHEN** a full snapshot completes having observed starred-at values for its items
- **THEN** the account's watermark equals the newest observed value before the next incremental scan runs
