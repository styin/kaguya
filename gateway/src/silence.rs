//! Silence Timer Management
//!
//! 3s → soft prompt opportunity
//! 8s → follow-up opportunity
//! 30s → context shift
//! Cancelled on speech start (vad_speech_start) or new dispatch

use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use crate::types::InputEvent;

pub struct SilenceTimers {
    soft_secs: u64,
    follow_up_secs: u64,
    context_shift_secs: u64,
    p4_tx: mpsc::Sender<InputEvent>,
}

impl SilenceTimers {
    pub fn new(soft: u64, follow_up: u64, context_shift: u64, p4_tx: mpsc::Sender<InputEvent>) -> Self {
        Self { soft_secs: soft, follow_up_secs: follow_up, context_shift_secs: context_shift, p4_tx }
    }

    /// three tier silence timers, returning a CancellationToken that can be used to cancel all timers
    pub fn start(&self) -> CancellationToken {
        let token = CancellationToken::new();
        let child = token.child_token();
        let targets = [self.soft_secs, self.follow_up_secs, self.context_shift_secs];
        let tx = self.p4_tx.clone();

        tokio::spawn(async move {
            let mut elapsed = 0u64;
            for &target in &targets {
                let wait = target.saturating_sub(elapsed);
                tokio::select! {
                    _ = child.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_secs(wait)) => {
                        elapsed = target;
                        let _ = tx.send(InputEvent::SilenceExceeded {
                            duration: Duration::from_secs(target),
                        }).await;
                    }
                }
            }
        });

        token
    }
}