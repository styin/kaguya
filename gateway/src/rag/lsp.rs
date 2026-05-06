//! LSP Client — STALE. Deferred until Kaguya has code-writing capabilities.
//! Kept as a stub for future integration. Not wired into the retriever.
//!
//! When activated, this module would connect to language servers
//! (rust-analyzer, pyright, tsserver) for workspace symbol search,
//! go-to-definition, and find-references. Phase 1 fallback is ripgrep
//! via the tools module (run_command).

use std::path::PathBuf;
use tracing::debug;

#[derive(Debug, Clone)]
pub struct LspSymbol {
    pub name: String,
    pub kind: String,
    pub location: String,
    pub container: Option<String>,
}

pub struct LspClient {
    _workspace_root: PathBuf,
}

impl LspClient {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { _workspace_root: workspace_root }
    }

    /// Stub — returns empty results. See module docs.
    pub async fn search_symbols(&self, query: &str) -> Vec<LspSymbol> {
        debug!(query = %query, "LSP search_symbols (stale stub, returning empty)");
        vec![]
    }
}
