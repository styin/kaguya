//! Reasoner 生命周期管理。
//!
//! 管理多个并发 Reasoner Agent，每个有独立 task_id。
//! 输出作为 P3 事件（ReasonerStep / ReasonerOutput / ReasonerError）回流。

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::info;
use uuid::Uuid;

use crate::types::*;

struct Agent {
    task_id: String,
    description: String,
    cancel: CancellationToken,
}

pub struct ReasonerManager {
    agents: Arc<RwLock<HashMap<String, Agent>>>,
    _endpoint: String,
}

impl ReasonerManager {
    pub fn new(endpoint: String) -> Self {
        Self {
            agents: Arc::new(RwLock::new(HashMap::new())),
            _endpoint: endpoint,
        }
    }

    /// 启动新的 Reasoner Agent
    pub async fn start(
        &self,
        description: String,
        _ctx: serde_json::Value,
        p3_tx: mpsc::Sender<InputEvent>,
    ) -> String {
        let task_id = Uuid::new_v4().to_string();
        let cancel = CancellationToken::new();
        let child = cancel.child_token();

        self.agents.write().await.insert(
            task_id.clone(),
            Agent {
                task_id: task_id.clone(),
                description: description.clone(),
                cancel,
            },
        );

        // Arc::clone → 'static，可以安全 move 进 spawn
        let agents = Arc::clone(&self.agents);
        let tid = task_id.clone();

        tokio::spawn(async move {
            info!(task_id = %tid, "Reasoner agent started");

            // TODO: gRPC 流式调用 ReasonerService.ExecuteTask
            tokio::select! {
                _ = child.cancelled() => {
                    info!(task_id = %tid, "Reasoner cancelled");
                }
                _ = async {
                    for i in 1..=3 {
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        let _ = p3_tx.send(InputEvent::ReasonerStep {
                            task_id: tid.clone(),
                            description: format!("Step {i}: processing..."),
                            progress: i as f32 / 3.0,
                        }).await;
                    }
                    let _ = p3_tx.send(InputEvent::ReasonerOutput {
                        task_id: tid.clone(),
                        result: format!("Completed: {description}"),
                    }).await;
                } => {}
            }

            agents.write().await.remove(&tid);
        });

        task_id
    }

    pub async fn cancel_all(&self) {
        for (_, agent) in self.agents.write().await.drain() {
            agent.cancel.cancel();
        }
    }

    pub async fn active_tasks(&self) -> Vec<ActiveTask> {
        self.agents
            .read()
            .await
            .values()
            .map(|a| ActiveTask {
                task_id: a.task_id.clone(),
                description: a.description.clone(),
                status: TaskStatus::Running,
            })
            .collect()
    }
}