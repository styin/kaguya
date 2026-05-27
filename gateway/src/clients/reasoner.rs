//! Reasoner Manager — adapted for new Delegate/Interrupt/Telemetry proto.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tonic::transport::Channel;
use tracing::{error, info, warn};

use crate::lifecycle::{ManagedConnectionHandle, Readiness, ReconnectPolicy, TaskSpawner};
use crate::proto;
use crate::proto::reasoner_service_client::ReasonerServiceClient;
use crate::types::*;

struct Agent {
    task_id: String,
    description: String,
    cancel: CancellationToken,
}

pub struct ReasonerManager {
    agents: Arc<RwLock<HashMap<String, Agent>>>,
    endpoint: String,
    client: Arc<RwLock<Option<ReasonerServiceClient<Channel>>>>,
    tasks: TaskSpawner,
    connection: ManagedConnectionHandle,
    reconnect: ReconnectPolicy,
}

impl ReasonerManager {
    pub fn new(endpoint: String, tasks: TaskSpawner, connection: ManagedConnectionHandle) -> Self {
        Self::with_reconnect_policy(endpoint, tasks, connection, ReconnectPolicy::default())
    }

    pub fn with_reconnect_policy(
        endpoint: String,
        tasks: TaskSpawner,
        connection: ManagedConnectionHandle,
        reconnect: ReconnectPolicy,
    ) -> Self {
        Self {
            agents: Arc::new(RwLock::new(HashMap::new())),
            endpoint,
            client: Arc::new(RwLock::new(None)),
            tasks,
            connection,
            reconnect,
        }
    }

    pub async fn start(
        &self,
        task_id: String,
        description: String,
        p3_tx: mpsc::Sender<InputEvent>,
    ) {
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

        let agents = Arc::clone(&self.agents);
        let client_arc = Arc::clone(&self.client);
        let endpoint = self.endpoint.clone();
        let tid = task_id.clone();
        let connection = self.connection.clone();
        let reconnect = self.reconnect;

        self.tasks
            .spawn(format!("reasoner_task:{tid}"), async move {
            info!(task_id = %tid, "Reasoner task started");
            connection.set_readiness(Readiness::Starting);

            // Try to get/connect client
            let maybe_client = {
                let g = client_arc.read().await;
                g.clone()
            };
            let maybe_client = match maybe_client {
                Some(c) => Some(c),
                None => {
                    match Self::connect_with_policy(&endpoint, connection.clone(), reconnect).await {
                        Some(c) => {
                            *client_arc.write().await = Some(c.clone());
                            Some(c)
                        }
                        None => None,
                    }
                }
            };

            if let Some(mut client) = maybe_client {
                connection.set_readiness(Readiness::Ready);
                // Open Delegate bidi stream
                let (del_tx, del_rx) = mpsc::channel::<proto::DelegateInput>(16);
                let outbound = ReceiverStream::new(del_rx);

                // Send TaskRequest
                let _ = del_tx
                    .send(proto::DelegateInput {
                        payload: Some(proto::delegate_input::Payload::StartTask(
                            proto::TaskRequest {
                                task_id: tid.clone(),
                                description: description.clone(),
                                metadata: HashMap::new(),
                            },
                        )),
                    })
                    .await;

                match client.delegate(outbound).await {
                    Ok(resp) => {
                        connection.set_readiness(Readiness::Ready);
                        let mut stream = resp.into_inner();
                        loop {
                            tokio::select! {
                                _ = child.cancelled() => {
                                    info!(task_id = %tid, "Reasoner cancelled");
                                    // Send Interrupt
                                    let _ = client.interrupt(proto::InterruptRequest {
                                        signal: Some(proto::interrupt_request::Signal::Cancel(
                                            proto::TaskCancel { task_id: tid.clone() }
                                        )),
                                    }).await;
                                    break;
                                }
                                result = stream.message() => {
                                    match result {
                                        Ok(Some(event)) => {
                                            match event.event {
                                                Some(proto::delegate_output::Event::Started(_)) => {
                                                    info!(task_id = %tid, "Reasoner started");
                                                }
                                                Some(proto::delegate_output::Event::Step(s)) => {
                                                    let _ = p3_tx.send(InputEvent::ReasonerStep {
                                                        task_id: tid.clone(),
                                                        description: s.description,
                                                    }).await;
                                                }
                                                Some(proto::delegate_output::Event::Output(o)) => {
                                                    let _ = p3_tx.send(InputEvent::ReasonerStep {
                                                        task_id: tid.clone(),
                                                        description: format!("[output] {}", o.content),
                                                    }).await;
                                                }
                                                Some(proto::delegate_output::Event::Completed(c)) => {
                                                    let _ = p3_tx.send(InputEvent::ReasonerCompleted {
                                                        task_id: tid.clone(),
                                                        summary: c.summary,
                                                    }).await;
                                                    break;
                                                }
                                                Some(proto::delegate_output::Event::Error(e)) => {
                                                    let _ = p3_tx.send(InputEvent::ReasonerError {
                                                        task_id: tid.clone(),
                                                        message: e.message,
                                                        code: e.code,
                                                    }).await;
                                                    break;
                                                }
                                                None => {}
                                            }
                                        }
                                        Ok(None) => break,
                                        Err(e) => {
                                            connection.set_readiness(Readiness::Degraded);
                                            error!("Reasoner stream error: {e}");
                                            let _ = p3_tx.send(InputEvent::ReasonerError {
                                                task_id: tid.clone(), message: e.to_string(), code: -1,
                                            }).await;
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        connection.set_readiness(Readiness::Degraded);
                        error!("Delegate failed: {e}");
                        let _ = p3_tx
                            .send(InputEvent::ReasonerError {
                                task_id: tid.clone(),
                                message: e.to_string(),
                                code: -1,
                            })
                            .await;
                    }
                }
            } else {
                // ── Stub fallback ──
                connection.set_readiness(Readiness::Degraded);
                warn!(task_id = %tid, "Reasoner unavailable, using stub");
                tokio::select! {
                    _ = child.cancelled() => {}
                    _ = async {
                        for i in 1..=3 {
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                            let _ = p3_tx.send(InputEvent::ReasonerStep {
                                task_id: tid.clone(),
                                description: format!("Step {i}: processing..."),
                            }).await;
                        }
                        let _ = p3_tx.send(InputEvent::ReasonerCompleted {
                            task_id: tid.clone(),
                            summary: format!("[stub] Completed: {description}"),
                        }).await;
                    } => {}
                }
            }

            agents.write().await.remove(&tid);
        });
    }

    async fn connect_with_policy(
        endpoint: &str,
        connection: ManagedConnectionHandle,
        reconnect: ReconnectPolicy,
    ) -> Option<ReasonerServiceClient<Channel>> {
        let retry_delays = reconnect.retry_delays();
        connection.set_readiness(Readiness::Starting);
        for attempt in 1..=reconnect.max_attempts() {
            match tokio::time::timeout(reconnect.attempt_timeout(), Self::connect_once(endpoint))
                .await
            {
                Ok(Ok(client)) => {
                    connection.set_readiness(Readiness::Ready);
                    return Some(client);
                }
                Ok(Err(e)) => {
                    warn!(
                        attempt,
                        max_attempts = reconnect.max_attempts(),
                        "Reasoner connect attempt failed: {e}"
                    );
                }
                Err(_) => {
                    warn!(
                        attempt,
                        max_attempts = reconnect.max_attempts(),
                        timeout_ms = reconnect.attempt_timeout().as_millis(),
                        "Reasoner connect attempt timed out"
                    );
                }
            }

            if let Some(delay) = retry_delays.get(attempt - 1) {
                tokio::time::sleep(*delay).await;
            }
        }
        connection.set_readiness(Readiness::Degraded);
        None
    }

    async fn connect_once(endpoint: &str) -> Result<ReasonerServiceClient<Channel>, String> {
        let channel = Channel::from_shared(endpoint.to_string())
            .map_err(|e| format!("bad Reasoner endpoint: {e}"))?
            .connect()
            .await
            .map_err(|e| e.to_string())?;
        Ok(ReasonerServiceClient::new(channel))
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
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::lifecycle::LifecycleSupervisor;

    #[tokio::test]
    async fn start_degrades_readiness_and_uses_stub_after_policy_exhaustion() {
        let lifecycle = LifecycleSupervisor::new();
        let connection = lifecycle.register_connection("reasoner");
        let reasoner = ReasonerManager::with_reconnect_policy(
            "not a valid uri".into(),
            lifecycle.spawner(),
            connection.clone(),
            ReconnectPolicy::bounded(
                1,
                Duration::from_millis(1),
                Duration::from_millis(1),
                Duration::from_millis(1),
            ),
        );
        let (p3_tx, mut p3_rx) = mpsc::channel(4);

        reasoner
            .start("task-1".into(), "test unavailable reasoner".into(), p3_tx)
            .await;
        tokio::time::sleep(Duration::from_millis(20)).await;

        assert_eq!(connection.readiness(), Readiness::Degraded);
        assert_eq!(reasoner.active_tasks().await.len(), 1);

        reasoner.cancel_all().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(p3_rx.try_recv().is_err());
    }
}
