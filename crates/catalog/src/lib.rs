#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Domain library for the Ratatoskr GitHub Catalog bounded context.
//!
//! The foundation owns process configuration, telemetry bootstrap, and
//! application of the first-version `github_catalog` schema. Account
//! credentials, synchronization, and provider access arrive with later
//! implementation plan items.

mod backup_policy;
mod commands;
mod config;
mod database;
mod identity;
mod incremental;
mod list_mutations;
mod metadata;
mod modes;
mod mutation_trail;
mod mutations;
mod observe;
pub mod provider;
mod provider_mutations;
pub mod rate_limit;
mod snapshot;
mod star_lists;
mod telemetry;
mod watches;

#[cfg(feature = "test-support")]
pub mod test_support;

pub use backup_policy::{
    BackupPolicyError, BackupPolicyInput, FeedbackOutcome, POLICY_DEBOUNCE, PolicyFeedback,
    PublicationOutcome, derive_backup_policy, latest_backup_policy_feedback,
    mark_backup_policy_dirty, publish_due_backup_policy, record_backup_policy_acknowledgment,
};
pub use commands::{
    ConsumedSyncCommand, HandledSyncCommand, RequestedSyncMode, SYNC_REQUESTED_TYPE,
    SyncCommandError, handle_sync_command,
};
pub use config::{AdminConfig, Config, ConfigError, Limits, StorageConfig};
pub use database::{Database, PersistenceError};
pub use identity::{
    AliasKind, IdentityError, RepositoryIdentity, apply_alias_observation, record_alias,
    resolve_alias, upsert_repository,
};
pub use incremental::{IncrementalScanError, IncrementalScanOutcome, run_incremental_scan};
pub use metadata::{AppliedOutcome, REVISION_HISTORY_LIMIT, apply_fresh_body, apply_not_modified};
pub use modes::{RequestedMode, SetModeRequest, set_repository_mode};
pub use mutations::{
    MutationContext, MutationError, MutationOutcome, MutationRequest, MutationSource,
    MutationStatus, RefusalReason, RepositoryRef, execute_batch, execute_mutation,
};
pub use observe::{ObserveError, ObserveOutcome, observe_repository};
pub use snapshot::{FullSnapshotOutcome, SnapshotError, run_full_snapshot};
pub use star_lists::{
    ListMember, StarListSnapshotOutcome, StarListSummary, StarListsError, current_list_members,
    current_star_lists, run_star_list_snapshot,
};
pub use telemetry::{TelemetryError, init_telemetry};
pub use watches::{
    AnalysisDispatch, AnalysisRequestState, RepositoryAnalysisRequestStatus, TerminalFactOutcome,
    WatchError, WatchEvaluation, WatchRegistration, consume_repository_analysis_completed,
    consume_repository_analysis_failed, dispatch_due_repository_analysis,
    evaluate_metadata_watches, register_repository_analysis_watch,
    repository_analysis_request_state, set_repository_analysis_watch_enabled,
};
