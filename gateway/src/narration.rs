//! Deliberative Narration Protocol - Filtering and rate-limiting for reasoner narration.

use std::time::{Duration, Instant};

pub struct NarrationFilter {
    min_interval: Duration,
    last_time: Option<Instant>,
    last_desc: String,
}

impl NarrationFilter {
    pub fn new(min_interval_secs: u64) -> Self {
        Self {
            min_interval: Duration::from_secs(min_interval_secs),
            last_time: None,
            last_desc: String::new(),
        }
    }

    pub fn should_narrate(&mut self, description: &str) -> bool {
        if let Some(t) = self.last_time {
            if t.elapsed() < self.min_interval { return false; }
        }
        if description == self.last_desc { return false; }
        self.last_time = Some(Instant::now());
        self.last_desc = description.to_string();
        true
    }
}