use std::{
    collections::{HashSet, hash_map::RandomState},
    hash::{BuildHasher, Hasher},
    time::Duration,
};

use reqwest::header::HeaderMap;

use crate::error::{Error, Result};

pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Retry configuration. Mirrors the TS SDK defaults.
#[derive(Clone, Debug)]
pub struct RetryPolicy {
    /// Maximum retries after the initial attempt; `0` disables retries. Default: 2.
    pub max_retries: u32,
    /// First backoff delay, doubled up to `backoff_max`. Default: 500 ms.
    pub backoff_initial: Duration,
    /// Maximum backoff delay. Default: 5 s.
    pub backoff_max: Duration,
    /// Fraction of each backoff delay randomly subtracted, from 0 to 1. Default: 0.25.
    pub backoff_jitter: f64,
    /// HTTP status codes to retry. Default: 408, 429, and 500–599.
    pub http_statuses: HashSet<u16>,
    /// Honor `Retry-After` and `retry-after-ms` up to `max_retry_after`. Default: true.
    pub respect_retry_after: bool,
    /// Maximum server retry delay; longer delays use backoff. Default: 60 s.
    pub max_retry_after: Duration,
    /// Retry connection failures. Default: true.
    pub retry_connection_errors: bool,
    /// Retry timeouts. Default: true.
    pub retry_timeouts: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 2,
            backoff_initial: Duration::from_millis(500),
            backoff_max: Duration::from_secs(5),
            backoff_jitter: 0.25,
            http_statuses: [408, 429].into_iter().chain(500..600).collect(),
            respect_retry_after: true,
            max_retry_after: Duration::from_secs(60),
            retry_connection_errors: true,
            retry_timeouts: true,
        }
    }
}

impl RetryPolicy {
    pub(crate) fn validate(&self) -> Result<()> {
        if !(0.0..=1.0).contains(&self.backoff_jitter) {
            return Err(Error::sdk(format!(
                "`retry.backoff_jitter` must be between 0 and 1, got {}.",
                self.backoff_jitter
            )));
        }
        if let Some(s) = self
            .http_statuses
            .iter()
            .find(|s| !(100..=999).contains(*s))
        {
            return Err(Error::sdk(format!(
                "`retry.http_statuses` must contain HTTP status codes, got {s}."
            )));
        }
        Ok(())
    }

    pub(crate) fn retries_error(&self, err: &Error) -> bool {
        match err {
            Error::Timeout(_) => self.retry_timeouts,
            Error::Connection(_) => self.retry_connection_errors,
            _ => false,
        }
    }

    /// Delay before the retry that follows zero-based `attempt`: an allowed server delay,
    /// else capped exponential backoff with jitter.
    pub(crate) fn delay(&self, attempt: u32, headers: Option<&HeaderMap>) -> Duration {
        self.delay_with(attempt, headers, random_unit())
    }

    fn delay_with(&self, attempt: u32, headers: Option<&HeaderMap>, random: f64) -> Duration {
        if self.respect_retry_after
            && let Some(after) = headers.and_then(parse_retry_after)
            && after <= self.max_retry_after
        {
            return after;
        }
        let exp = self
            .backoff_initial
            .saturating_mul(2u32.saturating_pow(attempt));
        exp.min(self.backoff_max)
            .mul_f64(1.0 - random * self.backoff_jitter)
    }
}

/// Parse `retry-after-ms` or `Retry-After` (seconds), preferring `retry-after-ms`.
///
/// ponytail: the HTTP-date form of `Retry-After` is not parsed (needs a date parser); it falls
/// back to backoff. Add when the API is seen to send dates.
pub(crate) fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    let num = |name: &str| -> Option<f64> {
        let v: f64 = headers.get(name)?.to_str().ok()?.trim().parse().ok()?;
        (v.is_finite() && v >= 0.0).then_some(v)
    };
    if let Some(ms) = num("retry-after-ms") {
        return Some(Duration::from_secs_f64(ms / 1000.0));
    }
    num("retry-after").map(Duration::from_secs_f64)
}

/// Uniform-ish value in `[0, 1)` from std's randomly seeded hasher.
// ponytail: not cryptographic; fine for jitter and multipart boundaries.
pub(crate) fn random_unit() -> f64 {
    (random_u64() >> 11) as f64 / (1u64 << 53) as f64
}

pub(crate) fn random_u64() -> u64 {
    RandomState::new().build_hasher().finish()
}

#[cfg(test)]
mod tests {
    use reqwest::header::HeaderValue;

    use super::*;

    fn headers(name: &'static str, value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(name, HeaderValue::from_str(value).unwrap());
        h
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let p = RetryPolicy {
            backoff_jitter: 0.0,
            ..Default::default()
        };
        let ms = |a| p.delay_with(a, None, 0.5).as_millis();
        assert_eq!(
            (ms(0), ms(1), ms(2), ms(3), ms(9)),
            (500, 1000, 2000, 4000, 5000)
        );
    }

    #[test]
    fn jitter_only_subtracts() {
        let p = RetryPolicy::default();
        assert_eq!(p.delay_with(0, None, 1.0).as_millis(), 375);
    }

    #[test]
    fn retry_after_prefers_ms_and_respects_cap() {
        let p = RetryPolicy::default();
        assert_eq!(
            p.delay_with(0, Some(&headers("retry-after-ms", "1500")), 0.0)
                .as_millis(),
            1500
        );
        assert_eq!(
            p.delay_with(0, Some(&headers("retry-after", "2")), 0.0)
                .as_secs(),
            2
        );
        // Above `max_retry_after`: falls back to backoff.
        assert_eq!(
            p.delay_with(0, Some(&headers("retry-after", "600")), 0.0)
                .as_millis(),
            500
        );
        assert_eq!(parse_retry_after(&headers("retry-after", "-1")), None);
    }

    #[test]
    fn default_statuses() {
        let p = RetryPolicy::default();
        assert!(
            [408, 429, 500, 503, 599]
                .iter()
                .all(|s| p.http_statuses.contains(s))
        );
        assert!(
            ![400, 401, 404, 600]
                .iter()
                .any(|s| p.http_statuses.contains(s))
        );
    }
}
