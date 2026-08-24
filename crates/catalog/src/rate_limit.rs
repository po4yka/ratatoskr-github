//! Per-token rate-limit accounting shared across operations.
//!
//! The ledger keys budgets by an opaque token reference, never by a secret.
//! It enforces a reserve floor against the provider reset time and honors
//! `Retry-After` cooldowns, so no single operation can exhaust an account.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

/// The reserve floor: acquisition stops when this little remains.
const RESERVE_FLOOR: i64 = 1;

/// An opaque reference to one provider token. The secret itself never enters
/// the ledger; callers pass an account identifier or key fingerprint label.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TokenRef(String);

impl TokenRef {
    /// Labels the budget with a non-secret reference such as an account id.
    #[must_use]
    pub fn from_label(label: impl Into<String>) -> Self {
        Self(label.into())
    }

    /// The non-secret label of the token reference.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TokenRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Provider rate-limit header values for one response.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RateLimitHeaders {
    /// `x-ratelimit-limit`.
    pub limit: Option<i64>,
    /// `x-ratelimit-remaining`.
    pub remaining: Option<i64>,
    /// `x-ratelimit-reset`, epoch seconds.
    pub reset_epoch_seconds: Option<i64>,
    /// `retry-after` seconds for a secondary limit hit.
    pub retry_after_seconds: Option<u64>,
}

/// Why acquisition was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AcquireError {
    /// The budget is exhausted or cooling down until this time.
    RateLimited {
        /// When the next request may proceed.
        retry_at: SystemTime,
    },
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    remaining: i64,
    reset_at: SystemTime,
    cooldown_until: Option<SystemTime>,
}

/// One shared per-token rate-limit budget.
#[derive(Debug, Default)]
pub struct RateLimitLedger {
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl RateLimitLedger {
    /// Creates an empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reserves one request from the token's budget.
    ///
    /// # Errors
    ///
    /// Returns [`AcquireError::RateLimited`] naming the earliest retry time
    /// when the allowance is at the floor or a cooldown is active.
    pub fn acquire(&self, token: &TokenRef) -> Result<(), AcquireError> {
        let now = SystemTime::now();
        let mut buckets = Self::lock(&self.buckets);
        let Some(bucket) = buckets.get_mut(token.label()) else {
            // No observed state yet: an unknown budget cannot be enforced.
            return Ok(());
        };
        if let Some(retry_at) = bucket
            .cooldown_until
            .filter(|cooldown_until| now < *cooldown_until)
        {
            return Err(AcquireError::RateLimited { retry_at });
        }
        if now < bucket.reset_at && bucket.remaining <= RESERVE_FLOOR {
            return Err(AcquireError::RateLimited {
                retry_at: bucket.reset_at,
            });
        }
        bucket.remaining -= 1;
        Ok(())
    }

    /// Records provider rate-limit headers into the token's budget.
    pub fn observe(&self, token: &TokenRef, headers: &RateLimitHeaders) {
        let now = SystemTime::now();
        let mut buckets = Self::lock(&self.buckets);
        let bucket = buckets.entry(token.label().to_owned()).or_insert(Bucket {
            remaining: i64::MAX,
            reset_at: now,
            cooldown_until: None,
        });
        if let Some(remaining) = headers.remaining {
            bucket.remaining = remaining;
        }
        if let Some(reset_epoch_seconds) = headers.reset_epoch_seconds {
            let since_epoch = u64::try_from(reset_epoch_seconds.max(0)).unwrap_or_default();
            bucket.reset_at = SystemTime::UNIX_EPOCH + Duration::from_secs(since_epoch);
        }
        if let Some(retry_after_seconds) = headers.retry_after_seconds {
            bucket.cooldown_until = Some(now + Duration::from_secs(retry_after_seconds));
        }
    }

    /// The current remaining allowance for the token, if any state is known.
    #[must_use]
    pub fn remaining(&self, token: &TokenRef) -> Option<i64> {
        Self::lock(&self.buckets)
            .get(token.label())
            .map(|bucket| bucket.remaining)
    }

    fn lock(
        mutex: &Mutex<HashMap<String, Bucket>>,
    ) -> std::sync::MutexGuard<'_, HashMap<String, Bucket>> {
        match mutex.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AcquireError, RateLimitHeaders, RateLimitLedger, TokenRef};
    use std::time::{Duration, SystemTime};

    fn headers(remaining: i64, reset_in_seconds: i64) -> RateLimitHeaders {
        let now_epoch = i64::try_from(
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        )
        .unwrap_or_default();
        RateLimitHeaders {
            limit: Some(5000),
            remaining: Some(remaining),
            reset_epoch_seconds: Some(now_epoch + reset_in_seconds),
            retry_after_seconds: None,
        }
    }

    #[test]
    fn budget_refuses_requests_at_reserve_until_reset() {
        let ledger = RateLimitLedger::new();
        let token = TokenRef::from_label("account-1");

        ledger.observe(&token, &headers(2, 3600));

        let granted = ledger.acquire(&token);
        assert!(granted.is_ok(), "allowance above the floor must proceed");

        let refused = ledger.acquire(&token);
        assert!(
            matches!(refused, Err(AcquireError::RateLimited { .. })),
            "acquisition at the reserve floor must be refused, got {refused:?}"
        );
        let Err(AcquireError::RateLimited { retry_at }) = refused else {
            return;
        };
        {
            let now = SystemTime::now();
            assert!(
                retry_at > now,
                "the refusal must name a future reset time, got {retry_at:?}"
            );
            assert!(
                retry_at <= now + Duration::from_secs(3700),
                "the refusal must track the provider reset window"
            );
        }
    }

    #[test]
    fn budget_is_shared_across_operations() {
        let ledger = std::sync::Arc::new(RateLimitLedger::new());
        let token = TokenRef::from_label("account-2");

        // One operation observes the provider state through its handle.
        let first_operation = std::sync::Arc::clone(&ledger);
        first_operation.observe(&token, &headers(0, 1800));

        // Another operation sees the depleted shared budget through its own.
        let second_operation = std::sync::Arc::clone(&ledger);
        let outcome = second_operation.acquire(&token);
        assert!(
            matches!(outcome, Err(AcquireError::RateLimited { retry_at: _ })),
            "a depleted shared budget must refuse other operations"
        );

        // A different token keeps its own budget.
        let other_token = TokenRef::from_label("account-3");
        assert!(
            second_operation.acquire(&other_token).is_ok(),
            "another token's budget is independent"
        );
    }

    #[test]
    fn retry_after_sets_cooldown_before_numeric_reset() {
        let ledger = RateLimitLedger::new();
        let token = TokenRef::from_label("account-4");

        ledger.observe(
            &token,
            &RateLimitHeaders {
                limit: Some(5000),
                remaining: Some(4000),
                reset_epoch_seconds: None,
                retry_after_seconds: Some(60),
            },
        );

        let refused = ledger.acquire(&token);
        assert!(
            matches!(refused, Err(AcquireError::RateLimited { .. })),
            "Retry-After must block acquisition, got {refused:?}"
        );
        let Err(AcquireError::RateLimited { retry_at }) = refused else {
            return;
        };
        let now = SystemTime::now();
        assert!(
            retry_at > now && retry_at <= now + Duration::from_secs(61),
            "cooldown must follow Retry-After, got {retry_at:?}"
        );

        let untouched_token = TokenRef::from_label("account-5");
        assert!(
            ledger.acquire(&untouched_token).is_ok(),
            "cooldown applies only to the observed token"
        );
    }
}
