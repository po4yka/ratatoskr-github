# GitHub Catalog threat model

## Assets

OAuth/PAT credentials, private repository metadata, star/list intent, organization visibility, provider mutation authority, rate budgets, and backup policy.

## Threats and controls

- **Token theft/overscope:** encrypted least-privilege credentials, scope display, rotation/revocation, no token propagation.
- **OAuth account mix-up:** PKCE/state and exact internal-user/callback binding.
- **False unstar/list removal:** only complete snapshots authorize absence; interrupted pages cannot commit.
- **Duplicate external mutation:** idempotency, current-state checks, serialized account writes, audit.
- **Private metadata leak:** owner authorization before query/event/projection and safe telemetry.
- **Rate-limit exhaustion:** conditional requests, checkpoints, budgets, retry-after, bounded concurrency.
- **Policy escalation:** explicit consent and audit for star/track/pin/complete archive changes.
- **Webhook/API spoofing:** signed/verified callbacks where used and provider response validation.

Re-review for GitHub App installation mode, organization-wide administration, webhooks, issue/release backup, or public sharing.
