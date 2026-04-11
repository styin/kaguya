//! LSP Client — Language Server Protocol integration for precise code search
//!
//! Connects to language servers (rust-analyzer, pyright, tsserver, etc.)
//! for workspace symbol search, go-to-definition, find-references.
//! This replaces fuzzy embedding search for code-specific queries.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

#[derive(Debug, Clone)]
pub struct LspSymbol {
    pub name: String,
    pub kind: String,       // "function", "class", "variable", etc.
    pub location: String,   // file:line
    pub container: Option<String>,
}

pub struct LspClient {
    processes: Mutex<HashMap<String, LspProcess>>,
    workspace_root: PathBuf,
}

struct LspProcess {
    child: Child,
    stdin: tokio::process::ChildStdin,
    stdout_reader: BufReader<tokio::process::ChildStdout>,
    next_id: i64,
}

impl LspClient {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            processes: Mutex::new(HashMap::new()),
            workspace_root,
        }
    }

    /// Search workspace symbols (workspace/symbol request)
    /// This is the primary code search method — precise, fast, no embedding needed
    pub async fn search_symbols(&self, query: &str) -> Vec<LspSymbol> {
        let mut results = Vec::new();
        let procs = self.processes.lock().await;

        for (lang, _proc) in procs.iter() {
            // In practice, we'd send a JSON-RPC request here.
            // Simplified for illustration:
            debug!(lang = %lang, query = %query, "LSP symbol search");
        }

        // Phase 1 fallback: ripgrep-based symbol search
        // Works without any LSP server running
        if results.is_empty() {
            results = self.ripgrep_fallback(query).await;
        }

        results
    }

    /// Fallback: ripgrep for exact text search in workspace
    /// Fast, works everywhere, no language server needed
    async fn ripgrep_fallback(&self, query: &str) -> Vec<LspSymbol> {
        let output = Command::new("rg")
            .args([
                "--json", "--max-count", "10",
                "--type-add", "code:*.{rs,py,ts,js,go,java,c,cpp,h}",
                "--type", "code",
                query,
            ])
            .current_dir(&self.workspace_root)
            .output()
            .await;

        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                stdout.lines()
                    .filter_map(|line| {
                        let v: serde_json::Value = serde_json::from_str(line).ok()?;
                        if v["type"].as_str()? != "match" { return None; }
                        let data = &v["data"];
                        Some(LspSymbol {
                            name: data["lines"]["text"].as_str()?.trim().to_string(),
                            kind: "match".into(),
                            location: format!("{}:{}",
                                data["path"]["text"].as_str().unwrap_or("?"),
                                data["line_number"].as_i64().unwrap_or(0),
                            ),
                            container: None,
                        })
                    })
                    .collect()
            }
            Err(e) => {
                debug!("ripgrep not available: {e}");
                Vec::new()
            }
        }
    }

    /// Start an LSP server for a language
    pub async fn start_server(&self, lang: &str, cmd: &str, args: &[&str]) -> anyhow::Result<()> {
        let mut child = Command::new(cmd)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();

        self.processes.lock().await.insert(lang.to_string(), LspProcess {
            child,
            stdin,
            stdout_reader: BufReader::new(stdout),
            next_id: 1,
        });

        info!(lang = %lang, cmd = %cmd, "LSP server started");
        Ok(())
    }
}