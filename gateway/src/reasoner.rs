//! Reasoner Manager
//! 
//! Manages long-running reasoning tasks executed by the Reasoner component.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;
use tonic::transport::Channel;
use tracing::{error, info, warn};

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
}

impl ReasonerManager {
    pub fn new(endpoint: String) -> Self {
        Self {
            agents: Arc::new(RwLock::new(HashMap::new())),
            endpoint,
            client: Arc::new(RwLock::new(None)),
        }
    }

    #[allow(dead_code)]
    async fn get_client(&self) -> Option<ReasonerServiceClient<Channel>> {
        let guard = self.client.read().await;
        if guard.is_some() {
            return guard.clone();
        }
        drop(guard);
        match Channel::from_shared(self.endpoint.clone()) {
            Ok(ch) => match ch.connect().await {
                Ok(channel) => {
                    let c = ReasonerServiceClient::new(channel);
                    *self.client.write().await = Some(c.clone());
                    info!(addr = %self.endpoint, "connected to Reasoner");
                    Some(c)
                }
                Err(e) => { warn!("Reasoner not available: {e}"); None }
            },
            Err(e) => { warn!("bad reasoner endpoint: {e}"); None }
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

        self.agents.write().await.insert(task_id.clone(), Agent {
            task_id: task_id.clone(),
            description: description.clone(),
            cancel,
        });

        let agents = Arc::clone(&self.agents);
        let client_arc = Arc::clone(&self.client);
        let endpoint = self.endpoint.clone();
        let tid = task_id.clone();

        tokio::spawn(async move {
            info!(task_id = %tid, "Reasoner task started");

            // Attempts to get cached client or connect if not available
            let maybe_client = {
                let g = client_arc.read().await;
                g.clone()
            };

            // Attempt connection if client not cached
            let maybe_client = match maybe_client {
                Some(c) => Some(c),
                None => {
                    match Channel::from_shared(endpoint) {
                        Ok(ch) => match ch.connect().await {
                            Ok(channel) => {
                                let c = ReasonerServiceClient::new(channel);
                                *client_arc.write().await = Some(c.clone());
                                Some(c)
                            }
                            Err(_) => None,
                        },
                        Err(_) => None,
                    }
                }
            };

            if let Some(mut client) = maybe_client {
                // ── gRPC TaskRequest ──
                let task = proto::TaskRequest {
                    task_id: tid.clone(),
                    description: description.clone(),
                    metadata: HashMap::new(),
                };

                match client.execute_task(task).await {
                    Ok(resp) => {
                        let mut stream: tonic::Streaming<proto::ReasonerEvent> = resp.into_inner();
                        loop {
                            tokio::select! {
                                _ = child.cancelled() => {
                                    info!(task_id = %tid, "Reasoner cancelled");
                                    let _ = client.cancel_task(proto::CancelRequest {
                                        task_id: tid.clone(),
                                    }).await;
                                    break;
                                }
                                result = stream.message() => {
                                    match result {
                                        Ok(Some(event)) => {
                                            match event.event {
                                                Some(proto::reasoner_event::Event::Started(_s)) => {
                                                    info!(task_id = %tid, "Reasoner started");
                                                }
                                                Some(proto::reasoner_event::Event::Step(s)) => {
                                                    // intermediate step update -> potential narration
                                                    let _ = p3_tx.send(InputEvent::ReasonerStep {
                                                        task_id: tid.clone(),
                                                        description: s.description,
                                                    }).await;
                                                }
                                                Some(proto::reasoner_event::Event::Output(o)) => {
                                                    // intermediate output -> potential narration
                                                    let _ = p3_tx.send(InputEvent::ReasonerStep {
                                                        task_id: tid.clone(),
                                                        description: format!("[output] {}", o.content),
                                                    }).await;
                                                }
                                                Some(proto::reasoner_event::Event::Completed(c)) => {
                                                    // final completion -> ProcessPrompt call
                                                    let _ = p3_tx.send(InputEvent::ReasonerCompleted {
                                                        task_id: tid.clone(),
                                                        summary: c.summary,
                                                    }).await;
                                                    break;
                                                }
                                                Some(proto::reasoner_event::Event::Error(e)) => {
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
                                            error!("Reasoner stream error: {e}");
                                            let _ = p3_tx.send(InputEvent::ReasonerError {
                                                task_id: tid.clone(),
                                                message: e.to_string(),
                                                code: -1,
                                            }).await;
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!("ExecuteTask failed: {e}");
                        let _ = p3_tx.send(InputEvent::ReasonerError {
                            task_id: tid.clone(),
                            message: e.to_string(),
                            code: -1,
                        }).await;
                    }
                }
            } else {
                // ── Reasoner unavailable - fallback ──
                // TODO: Implement a more meaningful fallback behavior, e.g. local execution of simple tasks or retry logic.
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

    pub async fn cancel_all(&self) {
        for (_, agent) in self.agents.write().await.drain() {
            agent.cancel.cancel();
        }
    }

    pub async fn active_tasks(&self) -> Vec<ActiveTask> {
        self.agents.read().await.values().map(|a| ActiveTask {
            task_id: a.task_id.clone(),
            description: a.description.clone(),
        }).collect()
    }
}