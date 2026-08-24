## Purpose

Gives the catalog a controlled seam to GitHub's REST API: conditional request mechanics, rename evidence from provider responses, and one rate-limit budget per token shared across all operations.

## ADDED Requirements

### Requirement: Conditional request mechanics

The gateway SHALL present a stored ETag as `If-None-Match` on repository fetches, SHALL report a not-modified outcome for `304 Not Modified` without requiring a response body, and SHALL return the decoded payload with the fresh validator on `200`.

#### Scenario: Not modified is reported cheaply from the HTTP layer

- **WHEN** a request presenting a stored ETag receives `304 Not Modified`
- **THEN** the gateway reports a not-modified outcome and no body is read or parsed

#### Scenario: Fresh response returns payload and validator

- **WHEN** the same fetch path receives `200` with an ETag header
- **THEN** the gateway returns the decoded repository payload together with that ETag

### Requirement: Rename evidence from provider responses

The gateway SHALL surface rename evidence to callers in two forms: a permanent-move response carrying a target location, and a successful body whose full name differs from the alias that was requested.

#### Scenario: Permanent move reports the new location

- **WHEN** a repository fetch by old alias receives a permanent-move redirect
- **THEN** the caller learns the fetch did not produce a payload and obtains the new owner/name location to follow

#### Scenario: Mismatched full name in a 200 body is rename evidence

- **WHEN** a repository fetch by one alias succeeds but the body declares a different full name
- **THEN** the caller receives both the payload and the rename evidence naming the observed owner/name

### Requirement: Per-token rate-limit budget shared across operations

The gateway-side accounting SHALL keep one budget per token reference, updated from provider rate-limit headers, shared by every operation using that token, refusing requests once the remaining allowance reaches a reserve floor until the reset time, and honoring `Retry-After` as a cooldown. Token secrets SHALL NOT be used as ledger keys, logged, or recorded.

#### Scenario: Budget is shared across operations

- **WHEN** one operation records provider headers showing most of the allowance consumed
- **THEN** another operation using the same token reference observes the reduced remaining allowance

#### Scenario: Requests are refused at the reserve floor until reset

- **WHEN** the remaining allowance has reached the reserve floor and reset time is still in the future
- **THEN** acquiring permission for another request fails with a rate-limited outcome naming when it may proceed, while a token whose allowance remains above the floor proceeds

#### Scenario: Retry-After sets a cooldown

- **WHEN** a response carries `Retry-After`
- **THEN** requests for that token are refused until the cooldown elapses even if numeric allowance remains

### Requirement: Recorded provider payloads are a parsing contract

The gateway SHALL normalize recorded synthetic GitHub repository payloads deterministically, so committed fixture files pin how provider field names and shapes map to catalog metadata.

#### Scenario: A recorded payload normalizes to stable projected fields

- **WHEN** the parser runs over a committed recorded repository payload
- **THEN** the produced projection fields equal the fixture's expected normalized values exactly
