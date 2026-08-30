#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Domain library for the Ratatoskr GitHub Catalog bounded context.
//!
//! The foundation owns process configuration, telemetry bootstrap, and
//! application of the first-version `github_catalog` schema. Account
//! credentials, synchronization, and provider access arrive with later
//! implementation plan items.

mod account_erasure;
mod analysis_terminal;
mod backup_policy;
mod commands;
mod config;
mod credentials;
mod database;
mod due;
mod identity;
mod inbox;
mod incremental;
mod legacy;
mod list_mutations;
mod metadata;
mod modes;
mod mutation_trail;
mod mutations;
mod observe;
mod outbox;
pub mod provider;
mod provider_mutations;
pub mod rate_limit;
mod snapshot;
mod star_lists;
mod telemetry;
mod watches;

#[cfg(feature = "test-support")]
pub mod test_support;

pub use account_erasure::{AccountErasureError, erase_account};
pub use analysis_terminal::{
    consume_repository_analysis_completed, consume_repository_analysis_completed_delivery,
    consume_repository_analysis_failed, consume_repository_analysis_failed_delivery,
};
pub use backup_policy::{
    BackupPolicyError, BackupPolicyInput, FeedbackOutcome, POLICY_DEBOUNCE, PolicyFeedback,
    PublicationOutcome, derive_backup_policy, latest_backup_policy_feedback,
    mark_backup_policy_dirty, publish_due_backup_policy, record_backup_policy_acknowledgment,
    record_backup_policy_acknowledgment_delivery,
};
pub use commands::{
    ConsumedSyncCommand, HandledSyncCommand, RequestedSyncMode, SYNC_REQUESTED_TYPE,
    SyncCommandError, handle_authenticated_sync_delivery, handle_sync_command,
};
pub use config::{
    AdminConfig, ApiConfig, BusConfig, Config, ConfigError, CredentialsConfig, GithubOAuthConfig,
    LegacyConfig, Limits, OAuthAppCredentials, ProviderConfig, StorageConfig,
};
pub use credentials::{
    CredentialError, CredentialKey, VerifiedGithubAccount, load_active_oauth, load_active_pat,
    register_oauth, register_pat,
};
pub use database::{Database, PersistenceError};
pub use due::{DueWorkError, DueWorkReport, run_due_work_once};
pub use identity::{
    AliasKind, IdentityError, RepositoryIdentity, apply_alias_observation, record_alias,
    resolve_alias, upsert_repository,
};
pub use inbox::{
    InboxClaimOutcome, InboxDelivery, claim_inbox_delivery, complete_inbox_delivery,
    reject_inbox_delivery, retry_inbox_delivery,
};
pub use incremental::{IncrementalScanError, IncrementalScanOutcome, run_incremental_scan};
pub use legacy::{
    LegacyCutoverReadiness, LegacyImportError, LegacyImportOutcome, LegacyImportRequest,
    LegacyIntegration, LegacyOwnerMap, LegacyOwnerMapError, LegacyRepository, LegacyShadowError,
    LegacyShadowReport, LegacySnapshot, LegacySource, LegacySourceError,
    generate_legacy_shadow_report, import_legacy_snapshot, legacy_cutover_readiness,
    legacy_shadow_account_ids,
};
pub use metadata::{
    AppliedOutcome, REVISION_HISTORY_LIMIT, ReadmeBlobError, RepositoryAnalysisPublicationError,
    RepositoryAnalysisSource, apply_fresh_body, apply_fresh_source, apply_not_modified,
    store_readme,
};
pub use modes::{
    RequestedMode, SetModeRequest, TrackOutcome, set_repository_mode, track_repository,
};
pub use mutations::{
    MutationContext, MutationError, MutationOutcome, MutationRequest, MutationSource,
    MutationStatus, RefusalReason, RepositoryRef, execute_batch, execute_mutation,
};
pub use observe::{ObserveError, ObserveOutcome, observe_repository};
pub use outbox::{
    ClaimedOutboxMessage, OutboxFailureCode, OutboxPublishReport, OutboxTransport,
    claim_due_outbox, confirm_outbox_published, fail_outbox_publication, publish_outbox_batch,
    requeue_dead_letter,
};
pub use snapshot::{FullSnapshotOutcome, SnapshotError, run_full_snapshot};
pub use star_lists::{
    ListMember, StarListSnapshotOutcome, StarListSummary, StarListsError, current_list_members,
    current_star_lists, run_star_list_snapshot,
};
pub use telemetry::{TelemetryError, init_telemetry};
pub use watches::{
    AnalysisDispatch, AnalysisRequestState, RepositoryAnalysisRequestStatus, TerminalFactOutcome,
    WatchError, WatchEvaluation, WatchRegistration, dispatch_due_repository_analysis,
    evaluate_metadata_watches, register_repository_analysis_watch,
    repository_analysis_request_state, set_repository_analysis_watch_enabled,
};
