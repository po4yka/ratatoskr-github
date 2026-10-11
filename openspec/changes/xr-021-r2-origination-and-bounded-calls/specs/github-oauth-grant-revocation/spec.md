## MODIFIED Requirements

### Requirement: Account erasure revokes a matching GitHub OAuth grant

For an account-erasure command with a stored OAuth credential issued to the
configured application, GitHub Catalog SHALL ask GitHub to delete the
application grant before deleting its local credential. It SHALL report
verified external revocation only after GitHub answers 204, or answers 404 and
a probe with the same access token (`GET /user`) answers 401, and then remove
its user-keyed local state. A 404 followed by any other probe answer or a probe
transport failure, a 422, and any other status SHALL be incomplete revocation.

#### Scenario: GitHub confirms grant deletion
- **WHEN** GitHub confirms deletion of the matching OAuth application grant
- **THEN** the owner acknowledgement reports verified erasure and the local
  credential and user-keyed catalog state are no longer readable

#### Scenario: GitHub reports no grant and the token is dead
- **WHEN** GitHub answers 404 for the grant deletion and the probe with the same token answers 401
- **THEN** the owner acknowledgement reports verified erasure

#### Scenario: GitHub reports no grant but the token is live
- **WHEN** GitHub answers 404 for the grant deletion and the probe answers 200 or fails
- **THEN** GitHub Catalog removes local credential custody and user-keyed state
  but acknowledges incomplete external grant revocation

#### Scenario: GitHub grant deletion fails
- **WHEN** the provider call for a matching OAuth grant fails or returns a
  non-success response
- **THEN** GitHub Catalog removes local credential custody and user-keyed state
  but acknowledges incomplete external grant revocation without exposing token
  or client-secret material
