//! 静默计时器管理。
//!
//! Spec §8:
//!   3s → soft prompt opportunity
//!   8s → follow-up opportunity
//!   30s → context shift
//!   全部在 vad_speech_start 或 text_command 时取消。

use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{self, Instant};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::types::InputEvent;

pub struct SilenceTimers {
    thresholds: [Duration; 3],
    p4_tx: mpsc::Sender<InputEvent>,
}

impl SilenceTimers {
    pub fn new(
        soft_secs: u64,
        follow_up_secs: u64,
        context_shift_secs: u64,
        p4_tx: mpsc::Sender<InputEvent>,
    ) -> Self {
        Self {
            thresholds: [
                Duration::from_secs(soft_secs),
                Duration::from_secs(follow_up_secs),
                Duration::from_secs(context_shift_secs),
            ],
            p4_tx,
        }
    }

    /// 启动静默计时器。返回 CancellationToken 供事件循环持有。
    /// 到达每个阈值时发出 P4 事件。被 cancel 则所有计时器立即停止。
    pub fn start(&self) -> CancellationToken {
        let token = CancellationToken::new();
        let child = token.child_token();
        let thresholds = self.thresholds;
        let tx = self.p4_tx.clone();

        tokio::spawn(async move {
            let start = Instant::now();

            for threshold in &thresholds {
                let remaining = threshold.saturating_sub(start.elapsed());
                tokio::select! {
                    _ = time::sleep(remaining) => {
                        let elapsed = start.elapsed();
                        debug!(secs = elapsed.as_secs(), "silence threshold reached");
                        let _ = tx
                            .send(InputEvent::SilenceExceeded { duration: elapsed })
                            .await;
                    }
                    _ = child.cancelled() => return,
                }
            }
        });

        token
    }
}