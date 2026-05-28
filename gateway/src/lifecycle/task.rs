use std::future::Future;
use std::sync::{Arc, Mutex};

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::debug;

pub(crate) struct ManagedTask {
    pub(crate) name: String,
    pub(crate) handle: JoinHandle<()>,
}

#[derive(Clone)]
pub struct TaskSpawner {
    shutdown: CancellationToken,
    tasks: Arc<Mutex<Vec<ManagedTask>>>,
}

impl TaskSpawner {
    pub(crate) fn new(shutdown: CancellationToken, tasks: Arc<Mutex<Vec<ManagedTask>>>) -> Self {
        Self { shutdown, tasks }
    }

    pub fn spawn<F>(&self, name: impl Into<String>, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let name = name.into();
        let child = self.shutdown.child_token();
        let task_name = name.clone();
        let handle = tokio::spawn(async move {
            tokio::select! {
                _ = child.cancelled() => {
                    debug!(task = %task_name, "managed task cancelled");
                }
                _ = future => {
                    debug!(task = %task_name, "managed task completed");
                }
            }
        });

        self.tasks
            .lock()
            .expect("managed task registry lock poisoned")
            .push(ManagedTask { name, handle });
    }
}
