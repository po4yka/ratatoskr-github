# star-list-snapshot Specification

## Purpose

Establishes authoritative per-account native star-list state by completely enumerating GitHub's star lists and their repository memberships, making the completed snapshot - never a partial pass - the sole authority over list identity and membership, recording membership transitions as evidenced observations, and leaving star authority entirely independent of list authority.

## ADDED Requirements

### Requirement: Complete list-and-membership enumeration

The catalog SHALL enumerate the account's complete native star-list set page by page until the provider reports exhaustion, with each list carrying its repository memberships, upserting every listed repository into identity before any authority decision is made.

#### Scenario: All pages are traversed in order until exhaustion

- **WHEN** a star-list snapshot runs against a provider serving several pages of lists followed by an empty final page
- **THEN** every page is requested exactly once in ascending order, every listed repository exists in the catalog afterward, and every observed list and membership reaches staging

### Requirement: Rate-budget-governed pagination

The catalog SHALL acquire from the shared per-token rate-limit budget before each page request and SHALL treat a budget refusal as a pause that changes no list or membership authority.

#### Scenario: Budget refusal pauses the scan without touching authority

- **WHEN** the shared budget refuses a page acquisition mid-scan
- **THEN** the run does not complete successfully, prior list and membership state remains entirely unchanged, and the pause point is recorded so the scan can continue later

### Requirement: Resumable cursor checkpoints

The catalog SHALL persist a checkpoint carrying the provider continuation token after each durably processed page, and an interrupted scan SHALL resume from that token without re-fetching any completed page.

#### Scenario: Interrupted scan resumes from the recorded token

- **WHEN** a scan is interrupted after some pages and restarted for the same run
- **THEN** the provider receives requests only from the recorded continuation onward, and no completed page is fetched again

### Requirement: Truncated membership refuses authority

The catalog SHALL treat a list whose membership exceeds what one provider page carries as a truncated enumeration: the run terminates failed with the reason naming the truncated list, staged rows clear, and list authority stays untouched.

#### Scenario: A list larger than one page fails the run without side effects

- **WHEN** an enumerated list reports more items than the fetched page holds
- **THEN** the run ends failed naming the truncated list, no list or membership authority changes, and nothing from any page of that run is applied

### Requirement: Atomic list authority swap

The catalog SHALL promote a completed enumeration to be the sole list authority in one transaction, so readers observe either the entire previous list authority or the entire new one, never an intermediate mixture.

#### Scenario: Readers never see a partially applied enumeration

- **WHEN** a snapshot has processed some pages but has not completed its traversal
- **THEN** reads of current lists and memberships return exactly the prior authority

#### Scenario: The completed enumeration replaces list authority wholesale

- **WHEN** a snapshot completes its traversal and applies its authority
- **THEN** new lists exist with their names, renamed lists carry their current name, newly listed repositories become members, absent memberships become non-members, and all transitions are visible together as one consistent state

### Requirement: Membership observations carry evidence

The catalog SHALL record each membership seen by a completed enumeration as an append-only observation row, and each previously-member but now-absent pair as a removal observation with an inferred removal time named as an observation time and the completing run as evidence.

#### Scenario: Absent membership becomes an evidenced removal

- **WHEN** a repository member under the prior authority does not appear in its list within a completed enumeration
- **THEN** its current membership state is non-member with a non-null observation time for the removal and the completing run id as evidence, and an append-only removal observation row exists

#### Scenario: Confirmations invent no timestamps

- **WHEN** a pair appears as members in consecutive completed enumerations
- **THEN** the pair's membership carries only provider-independent observation times, and no added or removal timestamp is invented where the provider supplies none

### Requirement: Removed lists are tombstoned, never deleted

The catalog SHALL mark a list absent from a completed enumeration as removed with an inferred observation time and evidence, SHALL demote all of its memberships in the same transaction, and SHALL NOT delete the list row or any observation referencing it.

#### Scenario: A list deleted upstream keeps its history

- **WHEN** a list present under prior authority does not appear in a completed enumeration
- **THEN** its row remains with a removed status, a non-null observed removal time, and the establishing run as evidence, and its memberships are all non-member with the same evidence

### Requirement: Failed or incomplete snapshots preserve prior list authority

The catalog SHALL leave previous list and membership authority entirely unchanged when a list snapshot fails, pauses, or otherwise does not complete its traversal, and SHALL record the attempt with its failure reason.

#### Scenario: Mid-run provider failure preserves prior authority

- **WHEN** a page request fails permanently partway through a list scan that had prior authority
- **THEN** the run terminates as failed, prior list and membership state is unchanged, no removal observation is written, and the run row names the failure

### Requirement: List runs record outcome and statistics

The catalog SHALL record every star-list snapshot attempt as a run with mode `star_lists`, start and finish times, a terminal status, and statistics covering pages processed, memberships observed, lists observed, additions, and removals.

#### Scenario: A completed list run carries its statistics

- **WHEN** a star-list snapshot completes successfully
- **THEN** its run row is completed with finish time and statistics matching the pages processed, memberships observed, lists observed, additions, and removals recorded during the run

### Requirement: Listed unstarred repositories stay truthful members

The catalog SHALL represent list membership independently of star state: a repository may hold current membership while locally unstarred or while never star-observed at all, and no list operation SHALL create, alter, or remove any star state.

#### Scenario: A list containing an unstarred repository is preserved

- **WHEN** a completed enumeration contains a repository whose local star state is unstarred, or which has no star state at all
- **THEN** the repository becomes and stays a truthful member of that list, and its star state is exactly as it was before the run

### Requirement: Current lists and members are readable

The catalog SHALL expose internal read functions returning the account's active lists and a list's current members, reflecting only promoted authority.

#### Scenario: Reads return the current authority after a swap

- **WHEN** the read functions are called after a completed snapshot
- **THEN** they report exactly the active lists with their current names and each list's current member repositories, excluding tombstoned lists and demoted memberships

### Requirement: Parsing contract pins the provider wire shape

The catalog SHALL pin the provider list-enumeration response shape with a committed synthetic fixture asserted by a parsing-contract test through the real adapter, so wire drift fails the build rather than corrupting observations.

#### Scenario: A drifted response shape fails the fixture contract

- **WHEN** the adapter parses a committed synthetic provider response
- **THEN** the parsed lists, items, continuation tokens, and rate data match the fixture's declared values exactly
