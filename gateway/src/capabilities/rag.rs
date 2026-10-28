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
    async fn evaluate_and_store(
        &self,
        user_input: &str,
        assistant_response: &str,
        generation_id: &str,
    );

    /// Export the current memory state as markdown for the persona prefix.
    async fn export_memory_md(&self) -> String;

    /// Report provider readiness.
    fn readiness(&self) -> Readiness;
}
