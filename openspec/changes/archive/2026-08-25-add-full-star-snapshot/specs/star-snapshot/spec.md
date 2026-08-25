# star-snapshot Specification

## Purpose

Establishes authoritative per-account star state by fully enumerating GitHub's starred-repository listing, making the completed snapshot - never a partial pass - the single authority over what is starred, and preserving evidence for every unstar it records.

## ADDED Requirements

### Requirement: Complete starred-set enumeration

The catalog SHALL enumerate the account's complete starred-repository listing page by page until the provider reports exhaustion, upserting each observed repository into identity before any authority decision is made.

#### Scenario: All pages are traversed in order until exhaustion

- **WHEN** a full snapshot runs against a provider serving several pages of starred repositories followed by an empty final page
- **THEN** every page is requested exactly once in ascending order and every listed repository exists in the catalog afterward

### Requirement: Rate-budget-governed pagination

The catalog SHALL acquire from the shared per-token rate-limit budget before each page request, and SHALL treat a budget refusal as a pause that changes no star authority.

#### Scenario: Budget refusal pauses the scan without touching authority

- **WHEN** the shared budget refuses a page acquisition mid-scan
- **THEN** the run does not complete successfully, the previous star authority remains entirely unchanged, and the pause point is recorded so the scan can continue later

### Requirement: Resumable checkpoints

The catalog SHALL persist a checkpoint after each durably processed page, and an interrupted scan SHALL resume from the recorded position without re-fetching any completed page.

#### Scenario: Interrupted scan resumes from the next page

- **WHEN** a scan is interrupted after some pages and restarted for the same run
- **THEN** the provider receives requests only for pages after the last checkpoint, and no completed page is fetched again

### Requirement: Atomic authority swap

The catalog SHALL promote a completed snapshot to be the sole star authority in one transaction, so that readers observe either the entire previous authority or the entire new one, never an intermediate mixture.

#### Scenario: Readers never see a partially applied snapshot

- **WHEN** a snapshot has processed some pages but has not completed its traversal
- **THEN** reads of current star state return exactly the prior authority, unaffected by in-flight page results

#### Scenario: The completed snapshot replaces authority wholesale

- **WHEN** a snapshot completes its traversal and applies its authority
- **THEN** additions become starred, absent repositories become unstarred, and both kinds of transition are visible together as one consistent state

### Requirement: Unstar observations carry evidence

The catalog SHALL record each repository that was previously starred but is absent from a completed snapshot as an unstar observation with an inferred removal time named as an observation time, carrying the establishing snapshot run as evidence, rather than deleting or silently dropping the prior star.

#### Scenario: Absent repository becomes an evidenced unstar

- **WHEN** a repository starred under the prior authority does not appear in a completed snapshot
- **THEN** its current state is unstarred with a non-null observation time for the unstar and the completing run id as evidence, and an append-only unstar observation row exists

### Requirement: Star timestamp continuity

The catalog SHALL preserve the originally established provider starred-at timestamp for a repository starred in both the prior authority and a completed snapshot, instead of replacing it with the newly observed value.

#### Scenario: Continuing stars keep their original starred-at

- **WHEN** a repository appears with a provider starred-at value in two consecutive completed snapshots
- **THEN** the stored starred-at remains the value established by the earlier snapshot

### Requirement: Failed or incomplete snapshots preserve prior authority

The catalog SHALL leave the previous star authority entirely unchanged when a snapshot fails, is cancelled, or otherwise does not complete its traversal, and SHALL record the attempt with its failure reason.

#### Scenario: Mid-run provider failure preserves prior authority

- **WHEN** a page request fails permanently partway through a scan that had prior authority
- **THEN** the run terminates as failed, the prior star authority is unchanged, no unstar is recorded, and the run row names the failure

### Requirement: Snapshot runs record outcome and statistics

The catalog SHALL record every full snapshot attempt as a run with mode full, start and finish times, a terminal status, and item statistics covering pages processed, repositories observed, additions, and unstars recorded.

#### Scenario: A completed run carries its statistics

- **WHEN** a full snapshot completes successfully
- **THEN** its run row is completed with finish time and statistics matching the pages processed, repositories observed, additions, and unstars recorded during the run

#### Scenario: A failed run carries its terminal state without statistics claims of success

- **WHEN** a full snapshot fails mid-run
- **THEN** its run row is failed with finish time and a failure reason, and no completion statistics are presented as a successful outcome
