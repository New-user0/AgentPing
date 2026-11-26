//! Exponential backoff policy for notification delivery.

use std::time::Duration;

/// Retry schedule for failed deliveries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Delay after the first failure.
    pub base: Duration,
    /// Upper bound for any single delay.
    pub max_delay: Duration,
    /// Total attempts (including the first) before a notification is marked failed.
    pub max_attempts: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            base: Duration::from_secs(5),
            max_delay: Duration::from_secs(15 * 60),
            max_attempts: 8,
        }
    }
}

impl RetryPolicy {
    /// Delay before the next attempt after `failed_attempts` failures (1-based), with
    /// `jitter` in `[0, 1)` spreading delays by up to ±20%.
    pub fn delay(&self, failed_attempts: u32, jitter: f64) -> Duration {
        let exp = failed_attempts.saturating_sub(1).min(20);
        let raw = self.base.saturating_mul(1u32 << exp);
        let capped = raw.min(self.max_delay);
        let factor = 0.8 + 0.4 * jitter.clamp(0.0, 1.0);
        capped.mul_f64(factor).min(self.max_delay)
    }

    /// Delay with random jitter.
    pub fn delay_random(&self, failed_attempts: u32) -> Duration {
        self.delay(failed_attempts, rand::random::<f64>())
    }

    /// Whether another attempt is allowed after `attempts_made` attempts.
    pub fn should_retry(&self, attempts_made: u32) -> bool {
        attempts_made < self.max_attempts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_grow_exponentially_and_cap() {
        let p = RetryPolicy::default();
        assert_eq!(p.delay(1, 0.5), Duration::from_secs(5));
        assert_eq!(p.delay(2, 0.5), Duration::from_secs(10));
        assert_eq!(p.delay(3, 0.5), Duration::from_secs(20));
        assert_eq!(p.delay(30, 0.5), Duration::from_secs(900));
        assert_eq!(
            p.delay(30, 1.0),
            Duration::from_secs(900),
            "jitter never exceeds max"
        );
    }

    #[test]
    fn jitter_stays_within_twenty_percent() {
        let p = RetryPolicy::default();
        assert_eq!(p.delay(1, 0.0), Duration::from_secs(4));
        assert_eq!(p.delay(1, 1.0), Duration::from_secs(6));
    }

    #[test]
    fn retry_budget() {
        let p = RetryPolicy::default();
        assert!(p.should_retry(7));
        assert!(!p.should_retry(8));
    }
}
