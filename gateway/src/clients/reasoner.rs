//! Scoped Reasoner delegation, cancellation, and failure reporting.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tonic::transport::Channel;
use tracing::{error, info, warn};

use crate::core::types::*;
use crate::lifecycle::{ManagedConnectionHandle, Readiness, ReconnectPolicy, TaskSpawner};
use kaguya_protocol::proto;
use kaguya_protocol::proto::reasoner_service_client::ReasonerServiceClient;
use kaguya_protocol::session_scope::SessionScope;

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
    scope: SessionScope,
}

impl ReasonerManager {
    pub fn new(
        endpoint: String,
        tasks: TaskSpawner,
        connection: ManagedConnectionHandle,
        scope: SessionScope,
    ) -> Self {
        Self::with_reconnect_policy(
            endpoint,
            tasks,
            connection,
            ReconnectPolicy::default(),
            scope,
        )
    }

    pub fn with_reconnect_policy(
        endpoint: String,
        tasks: TaskSpawner,
        connection: ManagedConnectionHandle,
        reconnect: ReconnectPolicy,
        scope: SessionScope,
    ) -> Self {
        Self {
            agents: Arc::new(RwLock::new(HashMap::new())),
            endpoint,
            client: Arc::new(RwLock::new(None)),
            tasks,
            connection,
            reconnect,
            scope,
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
        let scope = self.scope;

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
                    let connected = tokio::select! {
                        biased;
                        _ = child.cancelled() => {
                            agents.write().await.remove(&tid);
                            return;
                        }
                        result = Self::connect_with_policy(&endpoint, connection.clone(), reconnect) => result,
                    };
                    match connected {
                        Some(c) => {
                            *client_arc.write().await = Some(c.clone());
                            Some(c)
                        }
                        None => None,
                    }
                }
            };

            if child.is_cancelled() {
                agents.write().await.remove(&tid);
                return;
            }
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
                                session: Some(scope.to_proto()),
                                task_id: tid.clone(),
                                description: description.clone(),
                                metadata: HashMap::new(),
                            },
                        )),
                    })
                    .await;

                let delegated = tokio::select! {
                    biased;
                    _ = child.cancelled() => {
                        agents.write().await.remove(&tid);
                        return;
                    }
                    result = client.delegate(outbound) => result,
                };
                match delegated {
                    Ok(resp) => {
                        connection.set_readiness(Readiness::Ready);
                        let mut stream = resp.into_inner();
                        loop {
                            tokio::select! {
                                biased;
                                _ = child.cancelled() => {
                                    info!(task_id = %tid, "Reasoner cancelled");
                                    // Send Interrupt
                                    let interrupt = proto::InterruptRequest {
                                        session: Some(scope.to_proto()),
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
                                            if event.task_id != tid {
                                                connection.set_readiness(Readiness::Degraded);
                                                let _ = p3_tx.send(InputEvent::ReasonerError {
                                                    task_id: tid.clone(),
                                                    message: "Reasoner response task differs from its stream".into(),
                                                    code: -1,
                                                }).await;
                                                break;
                                            }
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
                                        Ok(None) => {
                                            connection.set_readiness(Readiness::Degraded);
                                            let _ = p3_tx.send(InputEvent::ReasonerError {
                                                task_id: tid.clone(),
                                                message: "Reasoner stream ended without a terminal result".into(),
                                                code: -1,
                                            }).await;
                                            break;
                                        },
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
                connection.set_readiness(Readiness::Degraded);
                warn!(task_id = %tid, "Reasoner unavailable after connection attempts");
                let _ = p3_tx.send(InputEvent::ReasonerError {
                    task_id: tid.clone(),
                    message: "Reasoner unavailable after connection attempts".into(),
                    code: -1,
                }).await;
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

    fn test_scope() -> SessionScope {
        SessionScope::parse(
            &uuid::Uuid::new_v4().to_string(),
            &uuid::Uuid::new_v4().to_string(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn unavailable_reasoner_reports_failure_without_simulated_progress_or_success() {
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
            test_scope(),
        );
        let (p3_tx, mut p3_rx) = mpsc::channel(4);

        reasoner
            .start("task-1".into(), "test unavailable reasoner".into(), p3_tx)
            .await;
        let event = tokio::time::timeout(Duration::from_secs(1), p3_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(event, InputEvent::ReasonerError { task_id, message, .. }
            if task_id == "task-1" && message.contains("unavailable"))
        );
        assert_eq!(connection.readiness(), Readiness::Degraded);
        assert!(
            p3_rx.recv().await.is_none(),
            "failure must be the only callback"
        );
        assert!(reasoner.active_tasks().await.is_empty());
    }

    struct RecordingReasoner {
        calls: mpsc::UnboundedSender<(String, SessionScope, String)>,
        response_task_id: Option<String>,
        close_after_step: bool,
        pending_headers: Option<Arc<tokio::sync::Notify>>,
    }

    #[tokio::test]
    async fn cancellation_before_connection_does_not_report_a_stale_failure() {
        let lifecycle = LifecycleSupervisor::new();
        let reasoner = ReasonerManager::new(
            "not a valid uri".into(),
            lifecycle.spawner(),
            lifecycle.register_connection("reasoner"),
            test_scope(),
        );
        let (tx, mut rx) = mpsc::channel(2);
        reasoner
            .start("task-1".into(), "cancelled request".into(), tx)
            .await;
        reasoner.cancel_all().await;
        assert!(tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .is_none());
        assert!(reasoner.active_tasks().await.is_empty());
    }

    #[tonic::async_trait]
    impl kaguya_protocol::proto::reasoner_service_server::ReasonerService for RecordingReasoner {
        type DelegateStream = ReceiverStream<Result<proto::DelegateOutput, tonic::Status>>;
        type TelemetryStream = ReceiverStream<Result<proto::TelemetryEvent, tonic::Status>>;

        async fn delegate(
            &self,
            request: tonic::Request<tonic::Streaming<proto::DelegateInput>>,
        ) -> Result<tonic::Response<Self::DelegateStream>, tonic::Status> {
            let first = request.into_inner().message().await?.unwrap();
            let Some(proto::delegate_input::Payload::StartTask(task)) = first.payload else {
                return Err(tonic::Status::invalid_argument(
                    "task must start the stream",
                ));
            };
            let session = SessionScope::from_proto(task.session.as_ref())
                .map_err(|error| tonic::Status::invalid_argument(error.to_string()))?;
            self.calls
                .send(("delegate".into(), session, task.task_id.clone()))
                .unwrap();
            if let Some(gate) = &self.pending_headers {
                gate.notified().await;
                return Err(tonic::Status::unavailable("delayed failure"));
            }
            let (tx, rx) = mpsc::channel(2);
            tx.send(Ok(proto::DelegateOutput {
                task_id: self.response_task_id.clone().unwrap_or(task.task_id),
                timestamp_ms: 0,
                event: Some(proto::delegate_output::Event::Step(
                    proto::ReasonerIntermediateStep {
                        description: "progress".into(),
                    },
                )),
            }))
            .await
            .unwrap();
            if !self.close_after_step {
                tokio::spawn(async move {
                    std::future::pending::<()>().await;
                    drop(tx);
                });
            }
            Ok(tonic::Response::new(ReceiverStream::new(rx)))
        }

        async fn interrupt(
            &self,
            request: tonic::Request<proto::InterruptRequest>,
        ) -> Result<tonic::Response<proto::InterruptAck>, tonic::Status> {
            let request = request.into_inner();
            let session = SessionScope::from_proto(request.session.as_ref())
                .map_err(|error| tonic::Status::invalid_argument(error.to_string()))?;
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
        use kaguya_protocol::proto::reasoner_service_server::ReasonerServiceServer;
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
                    response_task_id: None,
                    close_after_step: false,
                    pending_headers: None,
                }))
                .serve_with_incoming_shutdown(TcpListenerStream::new(tcp), token.cancelled())
                .await
                .unwrap();
        });
        let lifecycle = LifecycleSupervisor::new();
        let session = test_scope();
        let reasoner = ReasonerManager::new(
            format!("http://{addr}"),
            lifecycle.spawner(),
            lifecycle.register_connection("reasoner"),
            session,
        );
        let (p3_tx, mut p3_rx) = mpsc::channel(4);
        reasoner
            .start("task-1".into(), "test scoped delegation".into(), p3_tx)
            .await;
        let started = tokio::time::timeout(Duration::from_secs(3), calls_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(started, ("delegate".into(), session, "task-1".into()));
        let event = tokio::time::timeout(Duration::from_secs(3), p3_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(event, InputEvent::ReasonerStep { task_id, .. } if task_id == "task-1"),
            "expected Reasoner progress for the delegated task"
        );
        reasoner.cancel_all().await;
        let cancelled = tokio::time::timeout(Duration::from_secs(3), calls_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cancelled, ("interrupt".into(), session, "task-1".into()));
        shutdown.cancel();
    }

    #[tokio::test]
    async fn malformed_or_incomplete_reasoner_streams_report_failure() {
        use kaguya_protocol::proto::reasoner_service_server::ReasonerServiceServer;
        use tokio_stream::wrappers::TcpListenerStream;

        for (response_task_id, cancel_open) in [
            (None, false),
            (Some("wrong-task".to_string()), false),
            (None, true),
        ] {
            let wrong_task = response_task_id.is_some();
            let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = tcp.local_addr().unwrap();
            let (calls, mut calls_rx) = mpsc::unbounded_channel();
            let pending_headers = cancel_open.then(|| Arc::new(tokio::sync::Notify::new()));
            let server_gate = pending_headers.clone();
            let shutdown = CancellationToken::new();
            let token = shutdown.clone();
            let server = tokio::spawn(async move {
                tonic::transport::Server::builder()
                    .add_service(ReasonerServiceServer::new(RecordingReasoner {
                        calls,
                        response_task_id,
                        close_after_step: true,
                        pending_headers: server_gate,
                    }))
                    .serve_with_incoming_shutdown(TcpListenerStream::new(tcp), token.cancelled())
                    .await
                    .unwrap();
            });
            let lifecycle = LifecycleSupervisor::new();
            let session = test_scope();
            let reasoner = ReasonerManager::new(
                format!("http://{addr}"),
                lifecycle.spawner(),
                lifecycle.register_connection("reasoner"),
                session,
            );
            let (tx, mut rx) = mpsc::channel(4);
            reasoner
                .start("task-1".into(), "test stream failure".into(), tx)
                .await;
            if let Some(gate) = pending_headers {
                tokio::time::timeout(Duration::from_secs(3), calls_rx.recv())
                    .await
                    .unwrap()
                    .unwrap();
                reasoner.cancel_all().await;
                gate.notify_one();
                assert!(tokio::time::timeout(Duration::from_secs(1), rx.recv())
                    .await
                    .unwrap()
                    .is_none());
            } else {
                if !wrong_task {
                    assert!(matches!(
                        rx.recv().await.unwrap(),
                        InputEvent::ReasonerStep { .. }
                    ));
                }
                let event = tokio::time::timeout(Duration::from_secs(3), rx.recv())
                    .await
                    .unwrap()
                    .unwrap();
                assert!(
                    matches!(event, InputEvent::ReasonerError { task_id, message, .. }
                if task_id == "task-1" && message.contains(if wrong_task { "differs" } else { "terminal" }))
                );
                assert!(rx.recv().await.is_none());
            }
            assert!(reasoner.active_tasks().await.is_empty());
            shutdown.cancel();
            server.await.unwrap();
        }
    }
}
