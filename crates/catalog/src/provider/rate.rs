//! Rate-limit normalization shared by REST and GraphQL adapters.

use super::GraphqlRateLimit;
use crate::rate_limit::RateLimitHeaders;

/// Reads rate-limit headers off a response, tolerating absent values.
#[must_use]
pub(crate) fn rate_headers_from(headers: &reqwest::header::HeaderMap) -> RateLimitHeaders {
    let parse_i64 = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<i64>().ok())
    };
    RateLimitHeaders {
        limit: parse_i64("x-ratelimit-limit"),
        remaining: parse_i64("x-ratelimit-remaining"),
        reset_epoch_seconds: parse_i64("x-ratelimit-reset"),
        retry_after_seconds: headers
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok()),
    }
}

/// Maps GraphQL accounting onto the shared ledger shape.
#[must_use]
pub(crate) fn graphql_rate_limit(
    header_rate: RateLimitHeaders,
    body_rate: Option<&GraphqlRateLimit>,
) -> RateLimitHeaders {
    let from_headers = header_rate.limit.is_some()
        || header_rate.remaining.is_some()
        || header_rate.reset_epoch_seconds.is_some();
    if from_headers || body_rate.is_none() {
        return header_rate;
    }
    RateLimitHeaders {
        limit: None,
        remaining: body_rate.and_then(|rate| rate.remaining),
        reset_epoch_seconds: body_rate
            .and_then(|rate| rate.reset_at.as_deref())
            .and_then(rfc3339_epoch),
        retry_after_seconds: None,
    }
}

/// Parses an RFC 3339 timestamp into whole epoch seconds.
#[must_use]
pub(crate) fn rfc3339_epoch(value: &str) -> Option<i64> {
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(time::OffsetDateTime::unix_timestamp)
}
