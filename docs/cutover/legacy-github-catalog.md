# Legacy GitHub Catalog cutover

This runbook is an owner-gated procedure. It does not authorize a routing,
credential, or production change by itself. Never put a legacy dump, CSV,
token, encrypted token, database URL, password, or device code in this
repository, an issue, command-line argument, report, or terminal capture.

## Preconditions

1. Restore the archive into an isolated PostgreSQL instance using an operator
   account that cannot write to it. Confirm it is not reachable from normal
   application networks.
2. Create an owner-map JSON file outside this repository. It is an array of
   `legacy_user_id` / `owner_ref` entries, and every `owner_ref` is a current
   canonical `user:<uuid>` Platform tenant reference. Review it with the
   owner before import.
3. Configure the temporary source only as
   `RATATOSKR__LEGACY__SOURCE_DATABASE_URL`. The catalog process redacts this
   setting and forces source sessions to read-only. Configure the catalog
   credential encryption key and version separately; do not reuse a legacy
   encryption key.
4. Run the full local repository gate and retain its commit/hash evidence.

## Import and reauthorization

1. Run `ratatoskr-github-catalog import-legacy --source-id <label>
   --owner-map <path>`. Both values are non-secret labels/paths; a source URL
   or PAT argument is refused.
2. Retain only the command's redacted JSON result and import run identifier.
   Repeating the same source label is expected to add zero duplicate catalog
   rows.
3. Each mapped account holder supplies a replacement fine-grained PAT through
   standard input to `ratatoskr-github-catalog reconnect-pat <account-uuid>`.
   The operator must not paste it into a command, shell history, report, or
   chat. GitHub's authenticated-user response binds the credential to that
   one imported account.
4. An account still in `reauthorization_required` is excluded from cutover.
   Legacy encrypted tokens are neither read nor copied.

## Shadow and owner review

1. Run `ratatoskr-github-catalog shadow-legacy --source-id <label>` after every
   mapped account has reconnected. It performs a complete stars and native-list
   snapshot for each connected imported account before rendering the report.
   A partial, failed, cancelled, rate-limited, or truncated snapshot is not
   authority and must not be used for a clean result.
2. Retain only the canonical JSON and its digest from each report.
3. A report is reviewable only when it has no pending reauthorization, missing
   full/list snapshots, star mismatches, unresolved provider star times, or
   missing list claims. Repeat the full cycle and report before treating a
   result as stable.
4. Run `ratatoskr-github-catalog cutover-readiness --source-id <label>` to
   retrieve the newest clean report ID/digest. This verifies evidence only; it
   cannot activate anything.
5. The owner must approve a specific clean report digest together with the
   exact green gate evidence. Record that approval outside source control.

## Read then write activation

1. With the bounded approval, the owner changes **reads only** in the
   externally owned routing layer. Observe the agreed stability window and
   retain the redacted report digest and observed outcome.
2. The owner separately approves write activation after that window. Do not
   enable catalog star/list writes merely because read routing was approved.
3. This repository does not own the external router; no service command
   activates either phase.

## Rollback

On any regression, disable new catalog writes first, restore the previous
read route, and preserve the catalog import records and shadow reports for
diagnosis. Do not re-import to hide a discrepancy and do not recreate a
legacy database bridge. The owner decides whether to resume from a new,
reviewed clean report after the fault is understood.
