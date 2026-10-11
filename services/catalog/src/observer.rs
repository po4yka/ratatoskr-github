//! Periodic observation of repositories that have an enabled watch (XR-021 CONTRACTS.md R2-08 c).

use std::time::Duration;

use ratatoskr_github_catalog::provider::ReqwestGithubApi;
use ratatoskr_github_catalog::rate_limit::{RateLimitLedger, TokenRef};
use ratatoskr_github_catalog::{
    Database, ObserveError, ObserveOutcome, WatchConfig, observe_repository,
    record_observation_attempt, select_watched_repositories,
};
use time::OffsetDateTime;
use tokio::sync::watch;

/// Budget label every observer request is accounted under: the observer is unauthenticated.
pub const OBSERVER_TOKEN_LABEL: &str = "watch-observer";

/// How long after boot the first pass runs when the interval is longer, so a service restarted
/// more often than the interval still observes.
const FIRST_PASS_DELAY: Duration = Duration::from_secs(5);

/// What one observer pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ObserverPass {
    /// Repositories whose fresh body was recorded.
    pub observed: u32,
    /// Repositories the provider confirmed unchanged.
    pub unchanged: u32,
    /// Repositories whose observation failed; the pass continued with the next one.
    pub failed: u32,
    /// Whether the shared budget withheld the rest of the pass.
    pub rate_limited: bool,
}

/// A catalog failure that ends the observer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ObserverError {
    /// The watched repositories could not be selected.
    #[error("the watched repositories could not be selected")]
    Catalog,
}

/// The periodic observer.
#[derive(Debug)]
pub struct WatchObserver {
    gateway: ReqwestGithubApi,
    ledger: RateLimitLedger,
    interval: Duration,
    batch: u32,
}

impl WatchObserver {
    /// Builds the observer, or `None` when the interval is `0` (disabled).
    #[must_use]
    pub fn new(config: &WatchConfig, gateway: ReqwestGithubApi) -> Option<Self> {
        if config.observe_interval_seconds == 0 {
            return None;
        }
        Some(Self {
            gateway,
            ledger: RateLimitLedger::new(),
            interval: Duration::from_secs(u64::from(config.observe_interval_seconds)),
            batch: config.batch,
        })
    }

    /// The ledger every pass is gated by.
    #[must_use]
    pub fn ledger(&self) -> &RateLimitLedger {
        &self.ledger
    }

    /// Observes at most one batch of watched repositories, oldest observation first.
    ///
    /// # Errors
    ///
    /// Returns [`ObserverError`] when the selection fails.
    pub async fn pass(
        &self,
        database: &Database,
        now: OffsetDateTime,
    ) -> Result<ObserverPass, ObserverError> {
        let watched = select_watched_repositories(database, self.batch)
            .await
            .map_err(|_| ObserverError::Catalog)?;
        let token = TokenRef::from_label(OBSERVER_TOKEN_LABEL);
        let mut pass = ObserverPass::default();
        for repository in watched {
            let outcome = observe_repository(
                database,
                &self.gateway,
                &self.ledger,
                &token,
                &repository.alias,
                now,
            )
            .await;
            // A withheld budget attempted nothing, so the repository keeps its place; every
            // other outcome, failures included, moves it behind the repositories not yet tried.
            if !matches!(outcome, Ok(ObserveOutcome::RateLimited { .. })) {
                record_observation_attempt(database, repository.repository_id)
                    .await
                    .map_err(|_| ObserverError::Catalog)?;
            }
            match outcome {
                Ok(ObserveOutcome::Observed { .. }) => pass.observed += 1,
                Ok(ObserveOutcome::NotModified { .. }) => pass.unchanged += 1,
                Ok(ObserveOutcome::RateLimited { .. }) => {
                    pass.rate_limited = true;
                    break;
                }
                Ok(ObserveOutcome::MovedTo { .. }) => {
                    tracing::warn!(class = "moved", "a watched repository moved; not followed");
                    pass.failed += 1;
                }
                Err(error) => {
                    tracing::warn!(
                        class = failure_class(&error),
                        "a watched repository could not be observed"
                    );
                    pass.failed += 1;
                }
            }
        }
        Ok(pass)
    }

    /// Runs a pass every interval until `shutdown` is signalled.
    ///
    /// # Errors
    ///
    /// Returns [`ObserverError`] when a pass cannot select its repositories.
    pub async fn run(
        self,
        database: Database,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), ObserverError> {
        let first_pass = tokio::time::Instant::now() + self.interval.min(FIRST_PASS_DELAY);
        let mut ticker = tokio::time::interval_at(first_pass, self.interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            // cancel-safe: Interval::tick and watch::Receiver::wait_for retain no partial work.
            tokio::select! {
                biased;
                _ = shutdown.wait_for(|stopping| *stopping) => return Ok(()),
                _ = ticker.tick() => {}
            }
            // A pass is a series of independent transactions, so abandoning it on shutdown leaves
            // only whole observations behind.
            tokio::select! {
                biased;
                _ = shutdown.wait_for(|stopping| *stopping) => return Ok(()),
                pass = self.pass(&database, OffsetDateTime::now_utc()) => {
                    pass?;
                }
            }
        }
    }
}

/// A safe, closed class for the log: never provider text, a URL or a credential.
const fn failure_class(error: &ObserveError) -> &'static str {
    match error {
        ObserveError::Provider(_) => "provider",
        ObserveError::Persistence(_) | ObserveError::Identity(_) => "persistence",
        ObserveError::Watch(_) | ObserveError::Source(_) | ObserveError::ReadmeBlob(_) => "record",
        _ => "other",
    }
}
