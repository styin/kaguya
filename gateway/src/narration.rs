//! Deliberative Narration Protocol — 过滤 Reasoner 中间步骤。
//!
//! Spec §9: "One utterance per meaningful state transition;
//! rate-limited to prevent manic narration."

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

    /// 判断这个中间步骤是否值得叙述给用户
    pub fn should_narrate(&mut self, description: &str, progress: f32) -> bool {
        // 速率限制
        if let Some(t) = self.last_time {
            if t.elapsed() < self.min_interval {
                return false;
            }
        }
        // 去重
        if description == self.last_desc {
            return false;
        }
        // 有意义的进展节点
        let significant =
            progress <= 0.05 || progress >= 0.95 || (progress * 10.0).fract() < 0.15;
        if significant {
            self.last_time = Some(Instant::now());
            self.last_desc = description.to_string();
        }
        significant
    }
}