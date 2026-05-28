use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectPolicy {
    max_attempts: usize,
    initial_delay: Duration,
    max_delay: Duration,
    attempt_timeout: Duration,
}

impl ReconnectPolicy {
    pub fn bounded(
        max_attempts: usize,
        initial_delay: Duration,
        max_delay: Duration,
        attempt_timeout: Duration,
    ) -> Self {
        Self {
            max_attempts: max_attempts.max(1),
            initial_delay,
            max_delay,
            attempt_timeout,
        }
    }

    pub fn max_attempts(self) -> usize {
        self.max_attempts
    }

    pub fn attempt_timeout(self) -> Duration {
        self.attempt_timeout
    }

    pub fn retry_delays(self) -> Vec<Duration> {
        let mut delay = self.initial_delay;
        let mut delays = Vec::with_capacity(self.max_attempts.saturating_sub(1));
        for _ in 1..self.max_attempts {
            delays.push(delay.min(self.max_delay));
            delay = delay.saturating_mul(2);
        }
        delays
    }

    pub fn worst_case_elapsed(self) -> Duration {
        self.attempt_timeout
            .saturating_mul(self.max_attempts as u32)
            + self.retry_delays().into_iter().sum::<Duration>()
    }
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self::bounded(
            3,
            Duration::from_millis(250),
            Duration::from_secs(2),
            Duration::from_secs(2),
        )
    }
}
