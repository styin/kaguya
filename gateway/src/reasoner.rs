//! Reasoner Manager — adapted for new Delegate/Interrupt/Telemetry proto.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio_stream::wrappers::ReceiverStream;
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

    async fn ensure_client(&self) -> Option<ReasonerServiceClient<Channel>> {
        let guard = self.client.read().await;
        if guard.is_some() { return guard.clone(); }
        drop(guard);
        match Channel::from_shared(self.endpoint.clone()) {
            Ok(ch) => match ch.connect().await {
                Ok(channel) => {
                    let c = ReasonerServiceClient::new(channel);
                    *self.client.write().await = Some(c.clone());
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

            // Try to get/connect client
            let maybe_client = {
                let g = client_arc.read().await;
                g.clone()
            };
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
                // Open Delegate bidi stream
                let (del_tx, del_rx) = mpsc::channel::<proto::DelegateInput>(16);
                let outbound = ReceiverStream::new(del_rx);

                // Send TaskRequest
                let _ = del_tx.send(proto::DelegateInput {
                    payload: Some(proto::delegate_input::Payload::StartTask(
                        proto::TaskRequest {
                            task_id: tid.clone(),
                            description: description.clone(),
                            metadata: HashMap::new(),
                        }
                    )),
                }).await;

                match client.delegate(outbound).await {
                    Ok(resp) => {
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
                        error!("Delegate failed: {e}");
                        let _ = p3_tx.send(InputEvent::ReasonerError {
                            task_id: tid.clone(), message: e.to_string(), code: -1,
                        }).await;
                    }
                }
            } else {
                // ── Stub fallback ──
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