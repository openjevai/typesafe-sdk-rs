//! Retry policy, backoff computation, and server-provided delay parsing.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use http::{HeaderMap, StatusCode};

use crate::constants::{RETRY_AFTER_HEADER, RETRY_AFTER_MS_HEADER};
use crate::error::{ConfigError, Error};

/// A caller-supplied predicate that opts additional failures into retrying.
type RetryPredicate = Arc<dyn Fn(&Error) -> bool + Send + Sync>;

/// What the retry loop should do after a failed attempt.
pub(crate) enum Decision {
    /// Wait for the given delay and try again.
    Retry(Duration),
    /// Stop retrying and return the failure.
    Stop,
}

/// Configuration for SDK retry behaviour.
///
/// Every client has a policy; per-call overrides are accepted by each request builder. The defaults
/// retry up to twice, honour `Retry-After` and `retry-after-ms`, and give each SDK call a 30 second
/// budget.
///
/// ```
/// use std::time::Duration;
/// use typesafe_sdk::RetryPolicy;
///
/// let policy = RetryPolicy::new()
///     .with_max_retries(3)
///     .with_backoff_initial(Duration::from_millis(200))
///     .with_budget(Duration::from_secs(10));
///
/// assert_eq!(policy.max_retries(), 3);
/// ```
#[derive(Clone)]
pub struct RetryPolicy {
    /// Maximum retries after the initial attempt; `0` disables retries.
    max_retries: u32,
    /// First backoff delay, doubled each attempt up to `backoff_max`; zero disables backoff.
    backoff_initial: Duration,
    /// Maximum backoff delay; zero disables backoff.
    backoff_max: Duration,
    /// Fraction of each backoff delay randomly subtracted, between 0 and 1.
    backoff_jitter: f64,
    /// HTTP status codes that are retried.
    statuses: BTreeSet<StatusCode>,
    /// Whether to honour `Retry-After` and `retry-after-ms` response headers.
    respect_retry_after: bool,
    /// Whether to retry requests that cannot reach or read from the server.
    retry_connection_errors: bool,
    /// Whether to retry requests that exceed their timeout.
    retry_timeout_errors: bool,
    /// Optional predicate called with the raised error; returning `true` triggers a retry.
    predicate: Option<RetryPredicate>,
    /// Total retry budget per SDK call, including the initial attempt and delays.
    budget: Option<Duration>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 2,
            backoff_initial: Duration::from_millis(500),
            backoff_max: Duration::from_secs(5),
            backoff_jitter: 0.25,
            statuses: [StatusCode::REQUEST_TIMEOUT, StatusCode::TOO_MANY_REQUESTS]
                .into_iter()
                .chain(
                    (500..600).map(|code| {
                        StatusCode::from_u16(code).expect("5xx is a valid status code")
                    }),
                )
                .collect(),
            respect_retry_after: true,
            retry_connection_errors: true,
            retry_timeout_errors: true,
            predicate: None,
            budget: Some(Duration::from_secs(30)),
        }
    }
}

impl fmt::Debug for RetryPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RetryPolicy")
            .field("max_retries", &self.max_retries)
            .field("backoff_initial", &self.backoff_initial)
            .field("backoff_max", &self.backoff_max)
            .field("backoff_jitter", &self.backoff_jitter)
            .field("statuses", &self.statuses)
            .field("respect_retry_after", &self.respect_retry_after)
            .field("retry_connection_errors", &self.retry_connection_errors)
            .field("retry_timeout_errors", &self.retry_timeout_errors)
            .field("predicate", &self.predicate.as_ref().map(|_| "<custom>"))
            .field("budget", &self.budget)
            .finish()
    }
}

impl RetryPolicy {
    /// Creates a policy with the SDK defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the maximum number of retries after the initial attempt.
    #[must_use]
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Sets the first backoff delay, doubled on each subsequent attempt.
    #[must_use]
    pub fn with_backoff_initial(mut self, delay: Duration) -> Self {
        self.backoff_initial = delay;
        self
    }

    /// Sets the maximum backoff delay.
    #[must_use]
    pub fn with_backoff_max(mut self, delay: Duration) -> Self {
        self.backoff_max = delay;
        self
    }

    /// Sets the fraction of each backoff delay randomly subtracted.
    ///
    /// Non-finite fractions become `0.0`; finite fractions are clamped to `0.0..=1.0`.
    #[must_use]
    pub fn with_backoff_jitter(mut self, fraction: f64) -> Self {
        self.backoff_jitter = if fraction.is_finite() {
            fraction.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self
    }

    /// Replaces the set of HTTP status codes that are retried.
    #[must_use]
    pub fn with_retry_statuses(mut self, statuses: impl IntoIterator<Item = StatusCode>) -> Self {
        self.statuses = statuses.into_iter().collect();
        self
    }

    /// Sets whether `Retry-After` and `retry-after-ms` response headers are honoured.
    #[must_use]
    pub fn with_respect_retry_after(mut self, respect: bool) -> Self {
        self.respect_retry_after = respect;
        self
    }

    /// Sets whether connection errors are retried.
    #[must_use]
    pub fn with_retry_connection_errors(mut self, retry: bool) -> Self {
        self.retry_connection_errors = retry;
        self
    }

    /// Sets whether timeout errors are retried.
    #[must_use]
    pub fn with_retry_timeout_errors(mut self, retry: bool) -> Self {
        self.retry_timeout_errors = retry;
        self
    }

    /// Sets a predicate that opts additional failures into retrying.
    #[must_use]
    pub fn retry_if(mut self, predicate: impl Fn(&Error) -> bool + Send + Sync + 'static) -> Self {
        self.predicate = Some(Arc::new(predicate));
        self
    }

    /// Sets the total retry budget per SDK call, measured from before the first attempt.
    ///
    /// A retry whose delay would reach or exceed the budget is not attempted, and the last error is
    /// returned. Pass `None` to disable the budget.
    #[must_use]
    pub fn with_budget(mut self, timeout: impl Into<Option<Duration>>) -> Self {
        self.budget = timeout.into();
        self
    }

    /// Returns the maximum number of retries after the initial attempt.
    pub fn max_retries(&self) -> u32 {
        self.max_retries
    }

    /// Returns the first backoff delay.
    pub fn backoff_initial(&self) -> Duration {
        self.backoff_initial
    }

    /// Returns the maximum backoff delay.
    pub fn backoff_max(&self) -> Duration {
        self.backoff_max
    }

    /// Returns the fraction of each backoff delay randomly subtracted.
    pub fn backoff_jitter(&self) -> f64 {
        self.backoff_jitter
    }

    /// Returns the status codes that are retried.
    pub fn statuses(&self) -> &BTreeSet<StatusCode> {
        &self.statuses
    }

    /// Returns whether `Retry-After` and `retry-after-ms` headers are honoured.
    pub fn respects_retry_after(&self) -> bool {
        self.respect_retry_after
    }

    /// Returns whether connection errors are retried.
    pub fn retries_connection_errors(&self) -> bool {
        self.retry_connection_errors
    }

    /// Returns whether timeout errors are retried.
    pub fn retries_timeout_errors(&self) -> bool {
        self.retry_timeout_errors
    }

    /// Returns the total retry budget per SDK call, when one is set.
    pub fn budget(&self) -> Option<Duration> {
        self.budget
    }

    /// Validates the policy, rejecting a zero retry budget.
    ///
    /// Policies are validated when a client is built and when a call is sent, so calling this is only
    /// needed to fail earlier. The Python SDK performs the same check in its constructor.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when the budget is zero.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.budget == Some(Duration::ZERO) {
            return Err(ConfigError::new(
                "timeout must be a positive, finite number of seconds.",
            ));
        }
        Ok(())
    }
}

/// Decides whether `error` should be retried after `attempt` (1-based), `elapsed` into the call.
///
/// The retry rules -- including any caller predicate -- are evaluated before the attempt cap, the
/// order the Python SDK's retry engine uses, so a predicate observes every failed attempt.
pub(crate) fn decide(
    policy: &RetryPolicy,
    attempt: u32,
    elapsed: Duration,
    error: &Error,
) -> Decision {
    let retryable = is_retryable(policy, error);
    if !retryable || attempt > policy.max_retries {
        return Decision::Stop;
    }
    let delay = wait(policy, attempt, error);
    if policy
        .budget
        .is_some_and(|budget| elapsed.saturating_add(delay) >= budget)
    {
        return Decision::Stop;
    }
    Decision::Retry(delay)
}

/// Returns whether the policy retries this failure.
fn is_retryable(policy: &RetryPolicy, error: &Error) -> bool {
    let builtin = match error {
        Error::Timeout(_) => policy.retry_timeout_errors,
        Error::Connection(_) => policy.retry_connection_errors,
        other => other
            .status()
            .is_some_and(|status| policy.statuses.contains(&status)),
    };
    builtin
        || policy
            .predicate
            .as_ref()
            .is_some_and(|predicate| predicate(error))
}

/// Returns how long to wait before the retry that follows `attempt` (1-based).
fn wait(policy: &RetryPolicy, attempt: u32, error: &Error) -> Duration {
    if policy.respect_retry_after {
        if let Some(delay) = error.headers().and_then(parse_retry_after) {
            return delay;
        }
    }
    backoff(
        attempt,
        policy.backoff_initial,
        policy.backoff_max,
        policy.backoff_jitter,
    )
}

/// Parses the server's requested delay from `retry-after-ms` or `Retry-After`.
///
/// `retry-after-ms` is consulted first and takes precedence. Values may be a number of milliseconds,
/// or a number of seconds or an HTTP date in `Retry-After`; malformed, negative, or non-finite values
/// are ignored so the caller can fall back to its own backoff.
pub(crate) fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    parse_retry_after_at(headers, SystemTime::now())
}

/// [`parse_retry_after`] with an explicit current time, so HTTP dates are testable.
fn parse_retry_after_at(headers: &HeaderMap, now: SystemTime) -> Option<Duration> {
    for (name, multiplier) in [
        (RETRY_AFTER_MS_HEADER, 1.0_f64),
        (RETRY_AFTER_HEADER, 1000.0_f64),
    ] {
        let Some(raw) = crate::error::joined_header(headers, name) else {
            continue;
        };
        let raw = raw.as_ref();
        let Ok(value) = parse_number(raw) else {
            if name == RETRY_AFTER_HEADER {
                if let Some(delay) = http_date_delay(raw, now) {
                    return Some(delay);
                }
            }
            continue;
        };
        if value.is_finite() {
            if value >= 0.0 {
                let milliseconds = value * multiplier;
                if milliseconds.is_finite() {
                    return Some(duration_from_secs(milliseconds / 1000.0));
                }
            } else if name == RETRY_AFTER_HEADER {
                // A negative `Retry-After` is malformed, and stops the search.
                return None;
            }
        }
    }
    None
}

/// Parses a numeric header value the way `float()` does, treating an empty value as zero.
fn parse_number(raw: &str) -> Result<f64, ()> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(0.0);
    }
    trimmed.parse::<f64>().map_err(|_| ())
}

/// Converts seconds into a [`Duration`], saturating at [`Duration::MAX`].
fn duration_from_secs(seconds: f64) -> Duration {
    Duration::try_from_secs_f64(seconds).unwrap_or(Duration::MAX)
}

/// Returns how long until the HTTP date in `raw`, clamped at zero.
fn http_date_delay(raw: &str, now: SystemTime) -> Option<Duration> {
    let target = parse_http_date(raw)?;
    let now_seconds = now
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or(0.0);
    let delta = target - now_seconds;
    if !delta.is_finite() || delta <= 0.0 {
        return Some(Duration::ZERO);
    }
    Some(duration_from_secs(delta))
}

/// Parses an RFC 7231 HTTP date (IMF-fixdate, RFC 850, or asctime) into Unix seconds.
fn parse_http_date(raw: &str) -> Option<f64> {
    let normalized = raw.replace(',', " ");
    let tokens = normalized.split_whitespace().collect::<Vec<_>>();
    if tokens.len() < 4 {
        return None;
    }
    let (date, time, zone) = if tokens[1].contains('-') {
        // RFC 850: "Sunday, 06-Nov-94 08:49:37 GMT"
        let mut parts = tokens[1].split('-');
        let day = parse_u32(parts.next()?)?;
        let month = parse_month(parts.next()?)?;
        let year = parse_two_digit_year(parts.next()?)?;
        ((year, month, day), tokens[2], tokens.get(3).copied())
    } else if parse_month(tokens[1]).is_some() {
        // asctime: "Sun Nov  6 08:49:37 1994"
        if tokens.len() < 5 {
            return None;
        }
        (
            (
                parse_u32(tokens[4])?,
                parse_month(tokens[1])?,
                parse_u32(tokens[2])?,
            ),
            tokens[3],
            tokens.get(5).copied(),
        )
    } else {
        // IMF-fixdate: "Sun, 06 Nov 1994 08:49:37 GMT"
        if tokens.len() < 5 {
            return None;
        }
        (
            (
                parse_u32(tokens[3])?,
                parse_month(tokens[2])?,
                parse_u32(tokens[1])?,
            ),
            tokens[4],
            tokens.get(5).copied(),
        )
    };
    let (hour, minute, second) = parse_clock(time)?;
    let offset = zone.map(parse_zone).unwrap_or(0);
    let days = days_from_civil(date.0, date.1, date.2);
    Some((days * 86_400 + hour * 3_600 + minute * 60 + second - offset) as f64)
}

/// Parses a three-letter month name, case-insensitively.
fn parse_month(name: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let lowered = name.to_ascii_lowercase();
    MONTHS
        .iter()
        .position(|month| *month == lowered)
        .map(|index| index as u32 + 1)
}

/// Parses a four-digit year.
fn parse_u32(raw: &str) -> Option<u32> {
    raw.parse::<u32>().ok()
}

/// Expands a two-digit year the way `email.utils` does: `00..68` → 2000s, `69..99` → 1900s.
fn parse_two_digit_year(raw: &str) -> Option<u32> {
    let year = parse_u32(raw)?;
    if raw.len() > 2 {
        return Some(year);
    }
    Some(if year < 69 { 2000 + year } else { 1900 + year })
}

/// Parses `HH:MM:SS` or `HH:MM` into hours, minutes, and seconds.
fn parse_clock(raw: &str) -> Option<(i64, i64, i64)> {
    let mut parts = raw.split(':');
    let hour = parts.next()?.parse::<i64>().ok()?;
    let minute = parts.next()?.parse::<i64>().ok()?;
    let second = parts
        .next()
        .map(str::parse::<i64>)
        .transpose()
        .ok()?
        .unwrap_or(0);
    Some((hour, minute, second))
}

/// Parses a timezone token into its offset from UTC in seconds.
fn parse_zone(raw: &str) -> i64 {
    let lowered = raw.to_ascii_lowercase();
    match lowered.as_str() {
        "gmt" | "utc" | "ut" | "z" => return 0,
        _ => {}
    }
    let (sign, digits) = match lowered.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, lowered.strip_prefix('+').unwrap_or(lowered.as_str())),
    };
    if digits.len() == 4 {
        if let (Ok(hours), Ok(minutes)) = (digits[..2].parse::<i64>(), digits[2..].parse::<i64>()) {
            return sign * (hours * 3_600 + minutes * 60);
        }
    }
    0
}

/// Returns the number of days between 1970-01-01 and the given civil date.
fn days_from_civil(year: u32, month: u32, day: u32) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = i64::from(month) + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Returns the backoff delay for `attempt` (1-based), applying jitter.
fn backoff(attempt: u32, initial: Duration, maximum: Duration, jitter: f64) -> Duration {
    backoff_with(attempt, initial, maximum, jitter, random_f64())
}

/// [`backoff`] with an explicit random value in `0.0..=1.0`, so the timing rules are testable.
fn backoff_with(
    attempt: u32,
    initial: Duration,
    maximum: Duration,
    jitter: f64,
    random: f64,
) -> Duration {
    let initial_seconds = initial.as_secs_f64();
    let maximum_seconds = maximum.as_secs_f64();
    if initial_seconds == 0.0 || maximum_seconds == 0.0 {
        return Duration::ZERO;
    }
    let exponent = attempt.saturating_sub(1);
    let exponential = if f64::from(exponent) >= maximum_seconds.log2() - initial_seconds.log2() {
        maximum_seconds
    } else {
        initial_seconds * 2f64.powi(exponent as i32)
    };
    let delay = exponential * (1.0 - random * jitter);
    // Round to milliseconds the way Python's `round(delay, 3)` does: ties go to the even digit.
    let rounded = (delay * 1000.0).round_ties_even() / 1000.0;
    duration_from_secs(exponential.min(rounded))
}

/// Counter mixed into the seed so concurrent processes and threads differ.
static RANDOM_STATE: AtomicU64 = AtomicU64::new(0);

/// Returns a non-cryptographic random value in `0.0..1.0` for backoff jitter.
fn random_f64() -> f64 {
    let mut state = RANDOM_STATE.load(Ordering::Relaxed);
    if state == 0 {
        state = seed();
    }
    state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut scrambled = state;
    scrambled = (scrambled ^ (scrambled >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    scrambled = (scrambled ^ (scrambled >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    scrambled ^= scrambled >> 31;
    RANDOM_STATE.store(state, Ordering::Relaxed);
    (scrambled >> 11) as f64 * (1.0 / (1_u64 << 53) as f64)
}

/// Seeds the jitter generator from the wall clock, the process ID, and the address of a stack value.
fn seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0);
    nanos ^ u64::from(std::process::id()) ^ (RANDOM_STATE.as_ptr() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{ApiError, ConnectionError, TimeoutError};
    use serde_json::json;
    use std::sync::atomic::AtomicU32;
    use std::time::UNIX_EPOCH;

    /// Builds a 429 error carrying `value` in the given header.
    fn rate_limit_error(name: &str, value: &str) -> Error {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            value.parse().unwrap(),
        );
        Error::RateLimit(crate::error::RateLimitError::new(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            Some(json!({})),
            headers,
            None,
            None,
        )))
    }

    /// Simulates the retry loop, returning the attempt count and the delays waited.
    fn simulate(
        policy: &RetryPolicy,
        duration: Duration,
        server_delay: Duration,
    ) -> (u32, Vec<Duration>) {
        let mut elapsed = Duration::ZERO;
        let mut attempts = 0;
        let mut delays = Vec::new();
        loop {
            attempts += 1;
            elapsed += duration;
            let error = rate_limit_error(
                RETRY_AFTER_HEADER,
                &format!("{}", server_delay.as_secs_f64()),
            );
            match decide(policy, attempts, elapsed, &error) {
                Decision::Retry(delay) => {
                    delays.push(delay);
                    elapsed += delay;
                }
                Decision::Stop => return (attempts, delays),
            }
        }
    }

    #[test]
    fn budget_table_matches_the_reference_implementation() {
        let cases: [(Option<Duration>, Duration, Duration, u32); 6] = [
            (None, Duration::from_secs(1), Duration::from_millis(500), 3),
            (
                Some(Duration::from_secs(30)),
                Duration::from_secs(10),
                Duration::from_secs(5),
                2,
            ),
            (
                Some(Duration::from_millis(2_500)),
                Duration::from_millis(750),
                Duration::from_millis(500),
                2,
            ),
            (
                Some(Duration::from_secs(2)),
                Duration::from_secs(1),
                Duration::ZERO,
                2,
            ),
            (
                Some(Duration::from_secs(1)),
                Duration::ZERO,
                Duration::from_secs(1),
                1,
            ),
            (
                Some(Duration::from_secs(1)),
                Duration::ZERO,
                Duration::from_secs(60),
                1,
            ),
        ];
        for (budget, duration, delay, expected) in cases {
            let policy = RetryPolicy::new().with_budget(budget);
            let (attempts, delays) = simulate(&policy, duration, delay);
            assert_eq!(
                attempts, expected,
                "budget {budget:?} duration {duration:?} delay {delay:?}"
            );
            assert_eq!(delays, vec![delay; attempts as usize - 1]);
        }
    }

    #[test]
    fn budget_allows_two_calls_with_a_fresh_clock() {
        let policy = RetryPolicy::new().with_budget(Duration::from_secs(30));
        for _ in 0..2 {
            assert_eq!(
                simulate(&policy, Duration::from_secs(10), Duration::from_secs(5)).0,
                2
            );
        }
    }

    #[test]
    fn max_retries_caps_attempts() {
        for (max_retries, attempts) in [(0, 1), (1, 2), (4, 5)] {
            let policy = RetryPolicy::new()
                .with_max_retries(max_retries)
                .with_budget(None);
            assert_eq!(
                simulate(&policy, Duration::ZERO, Duration::ZERO).0,
                attempts
            );
        }
    }

    #[tokio::test]
    async fn retry_decisions_follow_status_and_error_kind() {
        let policy = RetryPolicy::new();
        let api_error = |status| {
            Error::Api(ApiError::new(
                StatusCode::from_u16(status).unwrap(),
                None,
                HeaderMap::new(),
                None,
                None,
            ))
        };
        for status in [408, 429, 500, 503, 599] {
            assert!(matches!(
                decide(&policy, 1, Duration::ZERO, &api_error(status)),
                Decision::Retry(_)
            ));
        }
        for status in [400, 401, 403, 404, 409, 422] {
            assert!(matches!(
                decide(&policy, 1, Duration::ZERO, &api_error(status)),
                Decision::Stop
            ));
        }
        assert!(matches!(
            decide(
                &policy,
                1,
                Duration::ZERO,
                &Error::Timeout(TimeoutError::new(None))
            ),
            Decision::Retry(_)
        ));
        let connection = Error::Connection(ConnectionError::new(connect_error().await));
        assert!(matches!(
            decide(&policy, 1, Duration::ZERO, &connection),
            Decision::Retry(_)
        ));
        assert!(matches!(
            decide(
                &policy,
                1,
                Duration::ZERO,
                &Error::InvalidInput(crate::error::InvalidInputError::new("x"))
            ),
            Decision::Stop
        ));
    }

    /// Produces a real `reqwest::Error` by connecting to a closed port.
    async fn connect_error() -> reqwest::Error {
        reqwest::Client::new()
            .get("http://127.0.0.1:1/")
            .send()
            .await
            .expect_err("connecting to a closed port fails")
    }

    #[tokio::test]
    async fn custom_statuses_and_error_kinds_are_configurable() {
        let policy = RetryPolicy::new()
            .with_retry_statuses([StatusCode::CONFLICT])
            .with_respect_retry_after(false);
        let conflict = Error::Api(ApiError::new(
            StatusCode::CONFLICT,
            None,
            HeaderMap::new(),
            None,
            None,
        ));
        assert!(matches!(
            decide(&policy, 1, Duration::ZERO, &conflict),
            Decision::Retry(_)
        ));
        let server_error = Error::Api(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            None,
            HeaderMap::new(),
            None,
            None,
        ));
        assert!(matches!(
            decide(&policy, 1, Duration::ZERO, &server_error),
            Decision::Stop
        ));

        let policy = RetryPolicy::new()
            .with_retry_connection_errors(false)
            .with_retry_timeout_errors(false);
        assert!(matches!(
            decide(
                &policy,
                1,
                Duration::ZERO,
                &Error::Timeout(TimeoutError::new(None))
            ),
            Decision::Stop
        ));
        assert!(matches!(
            decide(
                &policy,
                1,
                Duration::ZERO,
                &Error::Connection(ConnectionError::new(connect_error().await))
            ),
            Decision::Stop
        ));
    }

    #[test]
    fn predicate_opts_extra_failures_into_retrying() {
        let policy =
            RetryPolicy::new().retry_if(|error| error.status() == Some(StatusCode::NOT_FOUND));
        let not_found = Error::NotFound(ApiError::new(
            StatusCode::NOT_FOUND,
            None,
            HeaderMap::new(),
            None,
            None,
        ));
        assert!(matches!(
            decide(&policy, 1, Duration::ZERO, &not_found),
            Decision::Retry(_)
        ));
        let bad_request = Error::BadRequest(ApiError::new(
            StatusCode::BAD_REQUEST,
            None,
            HeaderMap::new(),
            None,
            None,
        ));
        assert!(matches!(
            decide(&policy, 1, Duration::ZERO, &bad_request),
            Decision::Stop
        ));
    }

    /// One row of the retry-after table: response headers and the delay they encode.
    type RetryAfterCase<'a> = (&'a [(&'a str, &'a str)], Option<Duration>);

    #[test]
    fn retry_after_table_matches_the_reference_implementation() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let cases: [RetryAfterCase<'_>; 10] = [
            (&[], None),
            (&[("Retry-After", "bad")], None),
            (&[("Retry-After", "-1")], None),
            (
                &[("retry-after-ms", "NaN"), ("Retry-After", "1.5")],
                Some(Duration::from_millis(1_500)),
            ),
            (
                &[("retry-after-ms", "-1"), ("Retry-After", "2")],
                Some(Duration::from_secs(2)),
            ),
            (&[("Retry-After", "")], Some(Duration::ZERO)),
            (&[("retry-after-ms", "inf")], None),
            (
                &[("retry-after-ms", "bad"), ("Retry-After", "2")],
                Some(Duration::from_secs(2)),
            ),
            (&[("Retry-After", "1e308")], None),
            (
                &[("retry-after-ms", "125")],
                Some(Duration::from_millis(125)),
            ),
        ];
        for (pairs, expected) in cases {
            let mut headers = HeaderMap::new();
            for (name, value) in pairs {
                headers.insert(
                    http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                    value.parse().unwrap(),
                );
            }
            assert_eq!(parse_retry_after_at(&headers, now), expected, "{pairs:?}");
        }
    }

    #[test]
    fn http_dates_are_converted_relative_to_now() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let future = httpdate(1_000_010);
        let past = httpdate(999_990);
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER_HEADER, future.parse().unwrap());
        assert_eq!(
            parse_retry_after_at(&headers, now),
            Some(Duration::from_secs(10))
        );
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER_HEADER, past.parse().unwrap());
        assert_eq!(parse_retry_after_at(&headers, now), Some(Duration::ZERO));
    }

    #[test]
    fn http_date_formats_are_parsed() {
        // The same instant written in the three formats RFC 7231 allows.
        let expected = 784_111_777.0;
        assert_eq!(
            parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(expected)
        );
        assert_eq!(
            parse_http_date("Sunday, 06-Nov-94 08:49:37 GMT"),
            Some(expected)
        );
        assert_eq!(parse_http_date("Sun Nov  6 08:49:37 1994"), Some(expected));
        assert_eq!(
            parse_http_date("Sun, 06 Nov 1994 08:49:37 +0000"),
            Some(expected)
        );
        assert_eq!(
            parse_http_date("Sun, 06 Nov 1994 09:49:37 +0100"),
            Some(expected)
        );
        assert_eq!(
            parse_http_date("Wed, 21 Oct 2015 07:28:00 GMT"),
            Some(1_445_412_480.0)
        );
        assert_eq!(parse_http_date("not a date"), None);
    }

    #[test]
    fn backoff_doubles_then_caps() {
        let initial = Duration::from_millis(500);
        let maximum = Duration::from_secs(5);
        for (attempt, expected) in [
            (1_u32, 0.5),
            (2, 1.0),
            (3, 2.0),
            (4, 4.0),
            (5, 5.0),
            (20, 5.0),
        ] {
            assert_eq!(
                backoff_with(attempt, initial, maximum, 0.25, 0.0),
                duration_from_secs(expected),
                "attempt {attempt}"
            );
        }
        assert_eq!(
            backoff_with(1, initial, maximum, 0.25, 1.0),
            duration_from_secs(0.375)
        );
        assert_eq!(
            backoff_with(1, Duration::ZERO, maximum, 0.25, 0.0),
            Duration::ZERO
        );
        assert_eq!(
            backoff_with(1, initial, Duration::ZERO, 0.25, 0.0),
            Duration::ZERO
        );
    }

    #[test]
    fn backoff_handles_extreme_values() {
        // `Duration` has nanosecond resolution, so sub-nanosecond delays round to zero rather than
        // behaving like the fractional seconds Python can represent.
        let cases: [(Duration, Duration, u32, Duration); 4] = [
            (
                Duration::from_nanos(1),
                Duration::from_secs(1_000_000_000),
                1,
                Duration::ZERO,
            ),
            (
                Duration::from_nanos(1),
                Duration::from_secs(1_000_000_000),
                100,
                Duration::from_secs(1_000_000_000),
            ),
            (
                Duration::from_secs(1 << 40),
                Duration::from_secs(1 << 40),
                1,
                Duration::from_secs(1 << 40),
            ),
            (
                Duration::from_millis(500),
                Duration::from_micros(600),
                1,
                Duration::from_micros(600),
            ),
        ];
        for (initial, maximum, attempt, expected) in cases {
            let actual = backoff_with(attempt, initial, maximum, 0.25, 0.0);
            assert_eq!(
                actual, expected,
                "initial {initial:?} maximum {maximum:?} attempt {attempt}"
            );
        }
        // A delay that overflows a `Duration` saturates rather than panicking.
        assert_eq!(
            backoff_with(2, Duration::MAX, Duration::MAX, 0.0, 0.0),
            Duration::MAX
        );
    }

    #[test]
    fn half_millisecond_ties_round_to_even_like_python() {
        // Python's round(delay, 3) sends exact ties to the even millisecond, so a tie that rounds down
        // lowers the delay (`min(exponential, rounded)`), while one that rounds up keeps the exponential
        // and is indistinguishable from rounding away from zero.
        let maximum = Duration::from_secs(5);
        assert_eq!(
            backoff_with(1, Duration::from_micros(62_500), maximum, 0.0, 0.0),
            Duration::from_millis(62),
            "0.0625 s rounds to 0.062 s, not 0.063 s"
        );
        assert_eq!(
            backoff_with(1, Duration::from_micros(562_500), maximum, 0.0, 0.0),
            Duration::from_millis(562),
            "0.5625 s rounds to 0.562 s, not 0.563 s"
        );
        assert_eq!(
            backoff_with(1, Duration::from_micros(187_500), maximum, 0.0, 0.0),
            Duration::from_micros(187_500)
        );
    }

    #[test]
    fn the_predicate_observes_every_failed_attempt() {
        // The retry rules run before the attempt cap, so the predicate sees the final failure too.
        let seen = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&seen);
        let policy = RetryPolicy::new()
            .with_max_retries(1)
            .retry_if(move |error| {
                counter.fetch_add(1, Ordering::SeqCst);
                error.status() == Some(StatusCode::NOT_FOUND)
            });
        let not_found = Error::NotFound(ApiError::new(
            StatusCode::NOT_FOUND,
            None,
            HeaderMap::new(),
            None,
            None,
        ));
        assert!(matches!(
            decide(&policy, 1, Duration::ZERO, &not_found),
            Decision::Retry(_)
        ));
        assert!(matches!(
            decide(&policy, 2, Duration::ZERO, &not_found),
            Decision::Stop
        ));
        assert_eq!(
            seen.load(Ordering::SeqCst),
            2,
            "one call per failed attempt, including the last"
        );

        let capped = Arc::new(AtomicU32::new(0));
        let observe = Arc::clone(&capped);
        let policy = RetryPolicy::new().with_max_retries(0).retry_if(move |_| {
            observe.fetch_add(1, Ordering::SeqCst);
            false
        });
        assert!(matches!(
            decide(&policy, 1, Duration::ZERO, &not_found),
            Decision::Stop
        ));
        assert_eq!(
            capped.load(Ordering::SeqCst),
            1,
            "the predicate still runs when the attempt cap stops the retry"
        );
    }

    #[test]
    fn waits_follow_the_configured_backoff_exactly() {
        // Ignoring `Retry-After` falls back to the backoff, which doubles per attempt.
        let long = rate_limit_error(RETRY_AFTER_HEADER, "61");
        let policy = RetryPolicy::new()
            .with_backoff_initial(Duration::from_millis(200))
            .with_backoff_max(Duration::from_secs(5))
            .with_backoff_jitter(0.0)
            .with_respect_retry_after(false);
        assert_eq!(wait(&policy, 1, &long), Duration::from_millis(200));
        assert_eq!(wait(&policy, 2, &long), Duration::from_millis(400));
        assert_eq!(
            wait(&policy, 5, &long),
            Duration::from_secs(3) + Duration::from_millis(200)
        );
        assert_eq!(
            wait(&policy, 6, &long),
            Duration::from_secs(5),
            "capped at backoff_max"
        );
        // With jitter enabled the wait is drawn from the documented range for that attempt.
        let jittered = RetryPolicy::new()
            .with_backoff_jitter(1.0)
            .with_respect_retry_after(false);
        for _ in 0..100 {
            let delay = wait(&jittered, 1, &long);
            assert!(delay <= Duration::from_millis(500), "{delay:?}");
        }
    }

    #[test]
    fn jitter_stays_within_bounds() {
        let initial = Duration::from_millis(500);
        let maximum = Duration::from_secs(5);
        for _ in 0..1_000 {
            let delay = backoff(1, initial, maximum, 0.25);
            assert!(
                delay >= Duration::from_millis(375) && delay <= initial,
                "{delay:?}"
            );
            let delay = backoff(2, initial, maximum, 0.25);
            assert!(
                delay >= Duration::from_millis(750) && delay <= Duration::from_secs(1),
                "{delay:?}"
            );
        }
    }

    #[test]
    fn server_delays_are_honoured_regardless_of_length() {
        let policy = RetryPolicy::new().with_backoff_initial(Duration::from_millis(500));
        let long = rate_limit_error(RETRY_AFTER_HEADER, "61");
        assert_eq!(wait(&policy, 1, &long), Duration::from_secs(61));
        let milliseconds = rate_limit_error(RETRY_AFTER_MS_HEADER, "60001");
        assert_eq!(
            wait(&policy, 1, &milliseconds),
            Duration::from_millis(60_001)
        );
        let unparseable = rate_limit_error(RETRY_AFTER_HEADER, "bad");
        let delay = wait(&policy, 1, &unparseable);
        assert!(
            delay >= Duration::from_millis(375) && delay <= Duration::from_millis(500),
            "{delay:?}"
        );
        let without_retry_after = RetryPolicy::new().with_respect_retry_after(false);
        let delay = wait(&without_retry_after, 1, &long);
        assert!(delay <= Duration::from_millis(500), "{delay:?}");
    }

    #[test]
    fn validation_rejects_a_zero_budget() {
        assert!(
            RetryPolicy::new()
                .with_budget(Duration::ZERO)
                .validate()
                .is_err()
        );
        assert!(
            RetryPolicy::new()
                .with_budget(Duration::from_millis(1))
                .validate()
                .is_ok()
        );
        assert!(RetryPolicy::new().with_budget(None).validate().is_ok());
    }

    #[test]
    fn jitter_is_clamped_and_non_finite_values_become_zero() {
        assert_eq!(
            RetryPolicy::new().with_backoff_jitter(2.0).backoff_jitter(),
            1.0
        );
        assert_eq!(
            RetryPolicy::new()
                .with_backoff_jitter(-1.0)
                .backoff_jitter(),
            0.0
        );
        assert_eq!(
            RetryPolicy::new()
                .with_backoff_jitter(f64::NAN)
                .backoff_jitter(),
            0.0
        );
        assert_eq!(
            RetryPolicy::new()
                .with_backoff_jitter(f64::INFINITY)
                .backoff_jitter(),
            0.0
        );
        assert_eq!(
            RetryPolicy::new().with_backoff_jitter(0.5).backoff_jitter(),
            0.5
        );
    }

    #[test]
    fn debug_reports_custom_predicates_without_their_contents() {
        let policy = RetryPolicy::new().retry_if(|_| true);
        let rendered = format!("{policy:?}");
        assert!(rendered.contains("Some(\"<custom>\")"), "{rendered}");
        assert!(rendered.contains("statuses"), "{rendered}");
    }

    /// Formats `seconds` since the epoch as an IMF-fixdate.
    fn httpdate(seconds: u64) -> String {
        let civil = civil_from_days((seconds / 86_400) as i64);
        let day_of_week = (seconds / 86_400 + 4) % 7;
        let names = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        let months = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        let remainder = seconds % 86_400;
        format!(
            "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
            names[day_of_week as usize],
            civil.2,
            months[civil.1 as usize - 1],
            civil.0,
            remainder / 3_600,
            (remainder % 3_600) / 60,
            remainder % 60
        )
    }

    /// Inverts [`days_from_civil`] for the test date formatter.
    fn civil_from_days(days: i64) -> (i64, u32, u32) {
        let z = days + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let day_of_era = z - era * 146_097;
        let year_of_era =
            (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let year = year_of_era + era * 400;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let shifted_month = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
        let month = if shifted_month < 10 {
            shifted_month + 3
        } else {
            shifted_month - 9
        };
        (year + i64::from(month <= 2), month as u32, day as u32)
    }
}
