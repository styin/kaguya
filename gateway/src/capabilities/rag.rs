//! RAG capability contract.
//!
//! Any struct that implements [`RagCapability`] can serve as the Gateway's
//! memory retrieval and storage backend. The default implementation is
//! [`RagEngine`](crate::rag::RagEngine) (SQLite + BM25 + optional vector).

use crate::lifecycle::Readiness;
use crate::proto;

/// Retrieval-augmented generation — memory retrieval, storage, and export.
///
/// The executor calls `retrieve` (via the orchestrator's pre-fetch),
/// `evaluate_and_store`, and `export_memory_md` through this trait.
/// Implementations are free to use any storage backend or retrieval
/// strategy.
#[tonic::async_trait]
pub trait RagCapability: Send + Sync {
    /// Machine-readable identifier for logging and diagnostics.
    fn id(&self) -> &str;

    /// Retrieve memories relevant to a user query.
    async fn retrieve(&self, query: &str) -> Vec<proto::RetrievalResult>;

    /// Post-turn memory evaluation. Decide what to extract and persist
    /// from the (user, assistant) exchange.
    async fn evaluate_and_store(&self, user_input: &str, assistant_response: &str, turn_id: &str);

    /// Export the current memory state as markdown for the persona prefix.
    async fn export_memory_md(&self) -> String;

    /// Report provider readiness.
    fn readiness(&self) -> Readiness;
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Minimal mock for pipeline and executor tests.
    pub struct MockRagCapability {
        pub retrieve_results: Vec<proto::RetrievalResult>,
        pub memory_md: String,
        pub evaluate_call_count: AtomicUsize,
    }

    impl MockRagCapability {
        pub fn new() -> Self {
            Self {
                retrieve_results: vec![],
                memory_md: "# Mock Memory\n".into(),
                evaluate_call_count: AtomicUsize::new(0),
            }
        }

        #[allow(dead_code)]
        pub fn evaluate_calls(&self) -> usize {
            self.evaluate_call_count.load(Ordering::SeqCst)
        }
    }

    #[tonic::async_trait]
    impl RagCapability for MockRagCapability {
        fn id(&self) -> &str {
            "mock"
        }

        async fn retrieve(&self, _query: &str) -> Vec<proto::RetrievalResult> {
            self.retrieve_results.clone()
        }

        async fn evaluate_and_store(
            &self,
            _user_input: &str,
            _assistant_response: &str,
            _turn_id: &str,
        ) {
            self.evaluate_call_count.fetch_add(1, Ordering::SeqCst);
        }

        async fn export_memory_md(&self) -> String {
            self.memory_md.clone()
        }

        fn readiness(&self) -> Readiness {
            Readiness::Ready
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::MockRagCapability;
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn mock_dispatches_through_trait_object() {
        let mock = MockRagCapability::new();
        let cap: Arc<dyn RagCapability> = Arc::new(mock);

        assert_eq!(cap.id(), "mock");
        assert!(cap.retrieve("hello").await.is_empty());
        assert_eq!(cap.export_memory_md().await, "# Mock Memory\n");
        assert_eq!(cap.readiness(), Readiness::Ready);

        cap.evaluate_and_store("q", "a", "t1").await;
        cap.evaluate_and_store("q2", "a2", "t2").await;
    }
}
