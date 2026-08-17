# Security Policy for Ratatoskr GitHub

Report vulnerabilities privately. Do not publish PATs, OAuth tokens, private repository metadata, organization membership, webhook secrets, or production API responses.

Security review is required for OAuth/PAT storage, scopes, callback binding, provider mutations, private repository access, GraphQL queries, webhook handling, audit, token refresh/revoke, and backup policy changes.

Baseline: least privilege; read/write consent separation; per-account encrypted credentials; no token logs/events; validate callback state/PKCE; rate-limit mutations; authorize repository visibility; emit only safe metadata; never pass provider tokens to Platform, Knowledge, Telegram, or Vault.
