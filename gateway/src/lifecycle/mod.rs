//! Gateway lifecycle supervision primitives.
//!
//! This module owns Gateway task and connection lifecycle state.
//! Submodules keep each ownership concern small while preserving the public
//! `crate::lifecycle` API used by the rest of the Gateway.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

mod connection;
mod reconnect;
mod task;

pub use connection::{
    ManagedConnection, ManagedConnectionHandle, ManagedConnectionSnapshot, Readiness,
};
pub use reconnect::ReconnectPolicy;
pub use task::TaskSpawner;

use task::ManagedTask;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownReason {
    P0Shutdown,
    OsSignal,
    Fatal,
    LoopEnded,
}

#[derive(Clone)]
pub struct LifecycleSupervisor {
    shutdown: CancellationToken,
    tasks: Arc<Mutex<Vec<ManagedTask>>>,
    connections: Arc<Mutex<HashMap<String, ManagedConnection>>>,
    shutdown_grace: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LifecycleSnapshot {
    pub task_count: usize,
    pub connections: Vec<ManagedConnectionSnapshot>,
}

impl LifecycleSupervisor {
    pub fn new() -> Self {
        Self {
            shutdown: CancellationToken::new(),
            tasks: Arc::new(Mutex::new(Vec::new())),
            connections: Arc::new(Mutex::new(HashMap::new())),
            shutdown_grace: Duration::from_secs(5),
        }
    }

    pub fn with_shutdown_grace(mut self, shutdown_grace: Duration) -> Self {
        self.shutdown_grace = shutdown_grace;
        self
    }

    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    pub fn spawner(&self) -> TaskSpawner {
        TaskSpawner::new(self.shutdown.clone(), Arc::clone(&self.tasks))
    }

    pub fn register_connection(&self, name: impl Into<String>) -> ManagedConnectionHandle {
        let name = name.into();
        self.connections
            .lock()
            .expect("managed connection registry lock poisoned")
            .entry(name.clone())
            .or_insert_with(|| ManagedConnection::new(name.clone()));
        ManagedConnectionHandle::new(name, Arc::clone(&self.connections))
    }

    pub fn connection_readiness(&self, name: &str) -> Option<Readiness> {
        self.connections
            .lock()
            .expect("managed connection registry lock poisoned")
            .get(name)
            .map(ManagedConnection::readiness)
    }

    pub fn connections_snapshot(&self) -> Vec<ManagedConnection> {
        let mut connections = self
            .connections
            .lock()
            .expect("managed connection registry lock poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        connections.sort_by(|a, b| a.name().cmp(b.name()));
        connections
    }

    pub fn connection_statuses(&self) -> Vec<ManagedConnectionSnapshot> {
        let mut connections = self
            .connections
            .lock()
            .expect("managed connection registry lock poisoned")
            .values()
            .map(ManagedConnection::snapshot)
            .collect::<Vec<_>>();
        connections.sort_by(|a, b| a.name.cmp(&b.name));
        connections
    }

    pub fn snapshot(&self) -> LifecycleSnapshot {
        LifecycleSnapshot {
            task_count: self.task_count(),
            connections: self.connection_statuses(),
        }
    }

    pub fn task_count(&self) -> usize {
        self.tasks
            .lock()
            .expect("managed task registry lock poisoned")
            .len()
    }

    pub fn spawn<F>(&self, name: impl Into<String>, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.spawner().spawn(name, future);
    }

    pub async fn shutdown(&mut self, reason: ShutdownReason) {
        if self.shutdown.is_cancelled() && self.task_count() == 0 {
            return;
        }

        self.shutdown.cancel();
        self.mark_connections(Readiness::Stopped);

        let tasks = {
            let mut guard = self
                .tasks
                .lock()
                .expect("managed task registry lock poisoned");
            std::mem::take(&mut *guard)
        };

        info!(
            reason = ?reason,
            task_count = tasks.len(),
            "lifecycle shutdown requested"
        );

        for task in tasks {
            let name = task.name;
            let mut handle = task.handle;
            tokio::select! {
                result = &mut handle => {
                    match result {
                        Ok(()) => debug!(task = %name, "managed task joined"),
                        Err(e) if e.is_cancelled() => {
                            debug!(task = %name, "managed task aborted");
                        }
                        Err(e) => {
                            warn!(task = %name, "managed task failed during shutdown: {e}");
                        }
                    }
                }
                _ = tokio::time::sleep(self.shutdown_grace) => {
                    warn!(task = %name, "managed task did not stop within grace period; aborting");
                    handle.abort();
                    let _ = handle.await;
                }
            }
        }
    }

    fn mark_connections(&self, readiness: Readiness) {
        for connection in self
            .connections
            .lock()
            .expect("managed connection registry lock poisoned")
            .values_mut()
        {
            connection.set_readiness(readiness);
        }
    }
}

impl Default for LifecycleSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    use super::*;

    #[tokio::test]
    async fn shutdown_cancels_and_joins_managed_task() {
        let stopped = Arc::new(AtomicBool::new(false));
        let stopped_task = Arc::clone(&stopped);
        let mut supervisor =
            LifecycleSupervisor::new().with_shutdown_grace(Duration::from_millis(100));

        supervisor.spawn("test-task", async move {
            tokio::time::sleep(Duration::from_secs(60)).await;
            stopped_task.store(true, Ordering::SeqCst);
        });

        assert_eq!(supervisor.task_count(), 1);
        supervisor.shutdown(ShutdownReason::P0Shutdown).await;
        assert_eq!(supervisor.task_count(), 0);
        assert!(!stopped.load(Ordering::SeqCst));
    }

    #[test]
    fn managed_connection_tracks_readiness() {
        let supervisor = LifecycleSupervisor::new();
        let connection = supervisor.register_connection("talker");
        assert_eq!(connection.name(), "talker");
        assert_eq!(connection.readiness(), Readiness::Starting);
        assert_eq!(
            supervisor.connection_readiness("talker"),
            Some(Readiness::Starting)
        );

        connection.set_readiness(Readiness::Ready);
        assert_eq!(connection.readiness(), Readiness::Ready);
        assert_eq!(
            supervisor.connection_readiness("talker"),
            Some(Readiness::Ready)
        );
    }

    #[test]
    fn reconnect_policy_is_bounded_and_exponential() {
        let policy = ReconnectPolicy::bounded(
            4,
            Duration::from_millis(100),
            Duration::from_millis(250),
            Duration::from_secs(3),
        );

        assert_eq!(policy.max_attempts(), 4);
        assert_eq!(policy.attempt_timeout(), Duration::from_secs(3));
        assert_eq!(
            policy.retry_delays(),
            vec![
                Duration::from_millis(100),
                Duration::from_millis(200),
                Duration::from_millis(250),
            ]
        );
    }

    #[test]
    fn reconnect_policy_allows_at_least_one_attempt() {
        let policy = ReconnectPolicy::bounded(
            0,
            Duration::from_millis(100),
            Duration::from_millis(250),
            Duration::from_secs(3),
        );

        assert_eq!(policy.max_attempts(), 1);
        assert!(policy.retry_delays().is_empty());
    }

    #[test]
    fn reconnect_policy_reports_worst_case_elapsed_budget() {
        let policy = ReconnectPolicy::bounded(
            3,
            Duration::from_millis(250),
            Duration::from_secs(2),
            Duration::from_secs(2),
        );

        assert_eq!(policy.worst_case_elapsed(), Duration::from_millis(6_750));
    }

    #[tokio::test]
    async fn shutdown_marks_connections_stopped() {
        let mut supervisor =
            LifecycleSupervisor::new().with_shutdown_grace(Duration::from_millis(100));
        let connection = supervisor.register_connection("listener");

        connection.set_readiness(Readiness::Ready);
        supervisor.shutdown(ShutdownReason::P0Shutdown).await;

        assert_eq!(connection.readiness(), Readiness::Stopped);
    }

    #[test]
    fn duplicate_connection_handles_share_one_readiness_slot() {
        let supervisor = LifecycleSupervisor::new();
        let listener_grpc = supervisor.register_connection("listener");
        let listener_audio = supervisor.register_connection("listener");

        listener_grpc.set_readiness(Readiness::Ready);
        assert_eq!(
            supervisor.connection_readiness("listener"),
            Some(Readiness::Ready)
        );

        listener_audio.set_readiness(Readiness::Degraded);
        assert_eq!(listener_grpc.readiness(), Readiness::Degraded);
        assert_eq!(supervisor.connections_snapshot().len(), 1);
    }

    #[test]
    fn lifecycle_snapshot_reports_sorted_connection_readiness() {
        let supervisor = LifecycleSupervisor::new();
        let talker = supervisor.register_connection("talker");
        let listener = supervisor.register_connection("listener");

        talker.set_readiness(Readiness::Ready);
        listener.set_readiness(Readiness::Degraded);

        let snapshot = supervisor.snapshot();

        assert_eq!(snapshot.task_count, 0);
        assert_eq!(
            snapshot.connections,
            vec![
                ManagedConnectionSnapshot {
                    name: "listener".to_string(),
                    readiness: Readiness::Degraded,
                },
                ManagedConnectionSnapshot {
                    name: "talker".to_string(),
                    readiness: Readiness::Ready,
                },
            ]
        );
    }

    #[test]
    fn readiness_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&Readiness::Degraded).expect("readiness should serialize"),
            "\"degraded\""
        );
    }

    #[tokio::test]
    async fn task_spawned_after_shutdown_sees_cancelled_token_and_can_be_drained() {
        let ran = Arc::new(AtomicBool::new(false));
        let ran_task = Arc::clone(&ran);
        let mut supervisor =
            LifecycleSupervisor::new().with_shutdown_grace(Duration::from_millis(100));
        let spawner = supervisor.spawner();

        supervisor.shutdown(ShutdownReason::P0Shutdown).await;
        spawner.spawn("late-task", async move {
            tokio::time::sleep(Duration::from_secs(60)).await;
            ran_task.store(true, Ordering::SeqCst);
        });

        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(supervisor.task_count(), 1);
        assert!(!ran.load(Ordering::SeqCst));

        supervisor.shutdown(ShutdownReason::LoopEnded).await;
        assert_eq!(supervisor.task_count(), 0);
        assert!(!ran.load(Ordering::SeqCst));
    }
}
