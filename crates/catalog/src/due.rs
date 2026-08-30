use time::OffsetDateTime;

use crate::{
    AnalysisDispatch, BackupPolicyError, Database, PublicationOutcome, WatchError,
    dispatch_due_repository_analysis, publish_due_backup_policy,
};

/// Result of one bounded database-driven due-work pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DueWorkReport {
    /// Analysis request dispatch result.
    pub analysis: AnalysisDispatch,
    /// Desired backup policy reconciliation result.
    pub policy: PublicationOutcome,
}

/// Failure of one due-work family; the supervisor may retry the next iteration.
#[derive(Debug, thiserror::Error)]
pub enum DueWorkError {
    /// Analysis dispatch failed.
    #[error("due analysis dispatch failed")]
    Analysis(#[source] WatchError),
    /// Policy reconciliation failed.
    #[error("due policy reconciliation failed")]
    Policy(#[source] BackupPolicyError),
}

/// Advances database-authoritative due work without depending on an HTTP request.
///
/// # Errors
///
/// Returns a classified family error; a long-running worker must retry on its next bounded tick.
pub async fn run_due_work_once(
    database: &Database,
    now: OffsetDateTime,
) -> Result<DueWorkReport, DueWorkError> {
    let analysis = dispatch_due_repository_analysis(database, now)
        .await
        .map_err(DueWorkError::Analysis)?;
    let policy = publish_due_backup_policy(database, now)
        .await
        .map_err(DueWorkError::Policy)?;
    Ok(DueWorkReport { analysis, policy })
}
