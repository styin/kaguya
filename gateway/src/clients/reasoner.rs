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
use crate::proto::SessionRef;
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
    session: SessionRef,
}

impl ReasonerManager {
    pub fn new(
        endpoint: String,
        tasks: TaskSpawner,
        connection: ManagedConnectionHandle,
        session: SessionRef,
    ) -> Self {
        Self::with_reconnect_policy(
            endpoint,
            tasks,
            connection,
            ReconnectPolicy::default(),
            session,
        )
    }

    pub fn with_reconnect_policy(
        endpoint: String,
        tasks: TaskSpawner,
        connection: ManagedConnectionHandle,
        reconnect: ReconnectPolicy,
        session: SessionRef,
    ) -> Self {
        Self {
            agents: Arc::new(RwLock::new(HashMap::new())),
            endpoint,
            client: Arc::new(RwLock::new(None)),
            tasks,
            connection,
            reconnect,
            session,
        }
    }

    pub async fn start(
        &self,
        task_id: String,
        description: String,
        p3_tx: mpsc::Sender<InputEvent>,
    ) {
        let (instance_id, session_id) =
            match crate::instance::validate_session_ref(Some(&self.session)) {
                Ok((instance_id, session_id)) => (instance_id.to_owned(), session_id.to_owned()),
                Err(error) => {
                    warn!(%error, "rejecting Reasoner task with invalid configured scope");
                    return;
                }
            };
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
        let session = self.session.clone();

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
                                session: Some(session.clone()),
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
                                    let interrupt = proto::InterruptRequest {
                                        session: Some(session.clone()),
                                        signal: Some(proto::interrupt_request::Signal::Cancel(
                                            proto::TaskCancel { task_id: tid.clone() }
                                        )),
                                    };
                                    let _ = client.interrupt(interrupt).await;
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
                                                        instance_id: instance_id.clone(), session_id: session_id.clone(),
                                                        task_id: tid.clone(),
                                                        description: s.description,
                                                    }).await;
                                                }
                                                Some(proto::delegate_output::Event::Output(o)) => {
                                                    let _ = p3_tx.send(InputEvent::ReasonerStep {
                                                        instance_id: instance_id.clone(), session_id: session_id.clone(),
                                                        task_id: tid.clone(),
                                                        description: format!("[output] {}", o.content),
                                                    }).await;
                                                }
                                                Some(proto::delegate_output::Event::Completed(c)) => {
                                                    let _ = p3_tx.send(InputEvent::ReasonerCompleted {
                                                        instance_id: instance_id.clone(), session_id: session_id.clone(),
                                                        task_id: tid.clone(),
                                                        summary: c.summary,
                                                    }).await;
                                                    break;
                                                }
                                                Some(proto::delegate_output::Event::Error(e)) => {
                                                    let _ = p3_tx.send(InputEvent::ReasonerError {
                                                        instance_id: instance_id.clone(), session_id: session_id.clone(),
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
                                                instance_id: instance_id.clone(), session_id: session_id.clone(),
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
                                instance_id: instance_id.clone(), session_id: session_id.clone(),
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
                                instance_id: instance_id.clone(), session_id: session_id.clone(),
                                task_id: tid.clone(),
                                description: format!("Step {i}: processing..."),
                            }).await;
                        }
                        let _ = p3_tx.send(InputEvent::ReasonerCompleted {
                            instance_id: instance_id.clone(), session_id: session_id.clone(),
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
            SessionRef {
                instance_id: Some(crate::proto::InstanceId {
                    value: uuid::Uuid::new_v4().to_string(),
                }),
                session_id: Some(crate::proto::SessionId {
                    value: uuid::Uuid::new_v4().to_string(),
                }),
            },
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

    struct RecordingReasoner {
        calls: mpsc::UnboundedSender<(String, SessionRef, String)>,
    }

    #[tonic::async_trait]
    impl crate::proto::reasoner_service_server::ReasonerService for RecordingReasoner {
        type DelegateStream = ReceiverStream<Result<proto::DelegateOutput, tonic::Status>>;
        type TelemetryStream = ReceiverStream<Result<proto::TelemetryEvent, tonic::Status>>;

        async fn delegate(
            &self,
            request: tonic::Request<tonic::Streaming<proto::DelegateInput>>,
        ) -> Result<tonic::Response<Self::DelegateStream>, tonic::Status> {
            assert!(!request.metadata().contains_key("x-kaguya-instance-id"));
            assert!(!request.metadata().contains_key("x-kaguya-session-id"));
            let first = request.into_inner().message().await?.unwrap();
            let Some(proto::delegate_input::Payload::StartTask(task)) = first.payload else {
                return Err(tonic::Status::invalid_argument(
                    "task must start the stream",
                ));
            };
            crate::instance::validate_session_ref(task.session.as_ref())
                .map_err(|error| tonic::Status::invalid_argument(error.to_string()))?;
            let session = task.session.unwrap();
            self.calls
                .send(("delegate".into(), session, task.task_id.clone()))
                .unwrap();
            let (tx, rx) = mpsc::channel(2);
            tx.send(Ok(proto::DelegateOutput {
                task_id: task.task_id,
                timestamp_ms: 0,
                event: Some(proto::delegate_output::Event::Step(
                    proto::ReasonerIntermediateStep {
                        description: "progress".into(),
                    },
                )),
            }))
            .await
            .unwrap();
            tokio::spawn(async move {
                std::future::pending::<()>().await;
                drop(tx);
            });
            Ok(tonic::Response::new(ReceiverStream::new(rx)))
        }

        async fn interrupt(
            &self,
            request: tonic::Request<proto::InterruptRequest>,
        ) -> Result<tonic::Response<proto::InterruptAck>, tonic::Status> {
            assert!(!request.metadata().contains_key("x-kaguya-instance-id"));
            assert!(!request.metadata().contains_key("x-kaguya-session-id"));
            let request = request.into_inner();
            crate::instance::validate_session_ref(request.session.as_ref())
                .map_err(|error| tonic::Status::invalid_argument(error.to_string()))?;
            let session = request.session.unwrap();
            let Some(proto::interrupt_request::Signal::Cancel(cancel)) = request.signal else {
                return Err(tonic::Status::invalid_argument(
                    "expected task cancellation",
                ));
            };
            self.calls
                .send(("interrupt".into(), session, cancel.task_id))
                .unwrap();
            Ok(tonic::Response::new(proto::InterruptAck {}))
        }

        async fn telemetry(
            &self,
            _request: tonic::Request<proto::TelemetrySubscribe>,
        ) -> Result<tonic::Response<Self::TelemetryStream>, tonic::Status> {
            let (_tx, rx) = mpsc::channel(1);
            Ok(tonic::Response::new(ReceiverStream::new(rx)))
        }
    }

    #[tokio::test]
    async fn delegation_and_cancellation_use_explicit_scope_and_preserve_callback_origin() {
        use crate::proto::reasoner_service_server::ReasonerServiceServer;
        use tokio_stream::wrappers::TcpListenerStream;
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        let (calls_tx, mut calls_rx) = mpsc::unbounded_channel();
        let shutdown = CancellationToken::new();
        let token = shutdown.clone();
        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(ReasonerServiceServer::new(RecordingReasoner {
                    calls: calls_tx,
                }))
                .serve_with_incoming_shutdown(TcpListenerStream::new(tcp), token.cancelled())
                .await
                .unwrap();
        });
        let lifecycle = LifecycleSupervisor::new();
        let session = SessionRef {
            instance_id: Some(crate::proto::InstanceId {
                value: uuid::Uuid::new_v4().to_string(),
            }),
            session_id: Some(crate::proto::SessionId {
                value: uuid::Uuid::new_v4().to_string(),
            }),
        };
        let reasoner = ReasonerManager::new(
            format!("http://{addr}"),
            lifecycle.spawner(),
            lifecycle.register_connection("reasoner"),
            session.clone(),
        );
        let (p3_tx, mut p3_rx) = mpsc::channel(4);
        reasoner
            .start("task-1".into(), "test scoped delegation".into(), p3_tx)
            .await;
        let started = tokio::time::timeout(Duration::from_secs(3), calls_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            started,
            ("delegate".into(), session.clone(), "task-1".into())
        );
        let event = tokio::time::timeout(Duration::from_secs(3), p3_rx.recv())
            .await
            .unwrap()
            .unwrap();
        match event {
            InputEvent::ReasonerStep {
                instance_id,
                session_id,
                task_id,
                ..
            } => {
                assert_eq!(
                    instance_id,
                    session
                        .instance_id
                        .as_ref()
                        .expect("validated SessionRef")
                        .value
                        .as_str()
                );
                assert_eq!(
                    session_id,
                    session
                        .session_id
                        .as_ref()
                        .expect("validated SessionRef")
                        .value
                        .as_str()
                );
                assert_eq!(task_id, "task-1");
            }
            _ => panic!("expected scoped Reasoner progress callback"),
        }
        reasoner.cancel_all().await;
        let cancelled = tokio::time::timeout(Duration::from_secs(3), calls_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cancelled, ("interrupt".into(), session, "task-1".into()));
        shutdown.cancel();
    }
}
