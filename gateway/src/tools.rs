//! Tool Registry and Dispatcher
//!
//! Tool requests originate in Talker and are coordinated by Gateway.
//! Filesystem tools execute locally; `sandbox_exec` is delegated to the
//! Supervisor-owned provider. Every result returns through the same P3 path.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info};

use crate::lifecycle::TaskSpawner;
use crate::proto;
use crate::sandbox::SandboxClient;
use crate::types::InputEvent;

struct ToolMeta {
    name: String,
    description: String,
    args_schema: String,
}

pub struct ToolRegistry {
    tools: Vec<ToolMeta>,
    workspace_root: PathBuf,
    tasks: TaskSpawner,
    sandbox: Option<Arc<SandboxClient>>,
}

impl ToolRegistry {
    pub fn new(
        workspace_root: PathBuf,
        tasks: TaskSpawner,
        sandbox: Option<Arc<SandboxClient>>,
    ) -> Self {
        let mut tools = vec![
            ToolMeta {
                name: "list_files".into(),
                description: "List files in directory".into(),
                args_schema: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#.into(),
            },
            ToolMeta {
                name: "read_file".into(),
                description: "Read file contents".into(),
                args_schema: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#.into(),
            },
            ToolMeta {
                name: "write_file".into(),
                description: "Write to file".into(),
                args_schema: r#"{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}}}"#.into(),
            },
            // Direct shell execution is intentionally not registered.
            // `sandbox_exec` is the canonical supervised execution path.
        ];

        // Advertise `sandbox_exec` only after Supervisor reports that its
        // provider is enabled.
        if let Some(sb) = &sandbox {
            if let Some(def) = sb.tool_definition() {
                tools.push(ToolMeta {
                    name: def.name,
                    description: def.description,
                    args_schema: def.args_schema,
                });
            }
        }

        Self {
            tools,
            workspace_root,
            tasks,
            sandbox,
        }
    }

    pub fn definitions(&self) -> Vec<proto::ToolDefinition> {
        self.tools
            .iter()
            .map(|t| proto::ToolDefinition {
                name: t.name.clone(),
                description: t.description.clone(),
                args_schema: t.args_schema.clone(),
            })
            .collect()
    }

    pub fn has(&self, name: &str) -> bool {
        self.tools.iter().any(|t| t.name == name)
    }

    /// Comma-separated list of registered tool names, for error messages.
    pub fn name_list(&self) -> String {
        self.tools
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn dispatch(
        &self,
        conversation_id: String,
        request_id: String,
        tool_name: String,
        args_json: String,
        p3_tx: mpsc::Sender<InputEvent>,
    ) {
        // ── Sandbox route ──
        // `sandbox_exec` runs through a Supervisor-owned provider handle.
        // Files persist across calls in the conversation; the normal P3
        // ToolResult path remains unchanged.
        if tool_name == "sandbox_exec" {
            let sandbox = self.sandbox.clone();
            info!(id = %request_id, "dispatching sandbox_exec");
            self.tasks
                .spawn(format!("tool_dispatch:{tool_name}"), async move {
                    let content = match sandbox {
                        Some(sb) => sb.exec_from_json(&conversation_id, &args_json).await,
                        None => serde_json::json!({ "error": "sandbox disabled" }).to_string(),
                    };
                    let _ = p3_tx
                        .send(InputEvent::ToolResult {
                            request_id,
                            tool_name,
                            content,
                        })
                        .await;
                });
            return;
        }

        // ── Filesystem tools ──
        info!(tool = %tool_name, id = %request_id, "dispatching tool");
        let root = self.workspace_root.clone();

        self.tasks
            .spawn(format!("tool_dispatch:{tool_name}"), async move {
                let result = match tool_name.as_str() {
                    "list_files" => exec_list_files(&root, &args_json).await,
                    "read_file" => exec_read_file(&root, &args_json).await,
                    "write_file" => exec_write_file(&root, &args_json).await,
                    // DISABLED: see tool registration above
                    // "run_command"  => exec_run_command(&root, &args_json).await,
                    other => Err(format!("unknown tool: {other}")),
                };

                let content = match result {
                    Ok(o) => o,
                    Err(e) => {
                        error!(tool = %tool_name, err = %e, "tool failed");
                        serde_json::json!({ "error": e }).to_string()
                    }
                };

                let _ = p3_tx
                    .send(InputEvent::ToolResult {
                        request_id,
                        tool_name,
                        content,
                    })
                    .await;
            });
    }
}

// ── helpers ──

fn parse_arg(json: &str, key: &str) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("bad JSON: {e}"))?;
    v.get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| format!("missing: {key}"))
}

fn safe_path(root: &Path, rel: &str) -> Result<PathBuf, String> {
    for component in Path::new(rel).components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err(format!("path contains '..': {rel}"));
        }
    }
    let candidate = root.join(rel);
    let root_canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if let Ok(canon) = candidate.canonicalize() {
        if !canon.starts_with(&root_canon) {
            return Err(format!("path escapes workspace: {rel}"));
        }
    }
    Ok(candidate)
}

async fn exec_list_files(root: &Path, args: &str) -> Result<String, String> {
    let rel = parse_arg(args, "path").unwrap_or_else(|_| ".".into());
    let dir = safe_path(root, &rel)?;
    let mut rd = tokio::fs::read_dir(&dir).await.map_err(|e| e.to_string())?;
    let mut files = Vec::new();
    while let Some(entry) = rd.next_entry().await.map_err(|e| e.to_string())? {
        let is_dir = entry
            .file_type()
            .await
            .map(|ft| ft.is_dir())
            .unwrap_or(false);
        files.push(serde_json::json!({
            "name": entry.file_name().to_string_lossy(),
            "is_dir": is_dir,
        }));
    }
    Ok(serde_json::json!({ "files": files }).to_string())
}

async fn exec_read_file(root: &Path, args: &str) -> Result<String, String> {
    let rel = parse_arg(args, "path")?;
    let path = safe_path(root, &rel)?;
    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| e.to_string())?;
    let trunc = if content.len() > 8192 {
        format!("{}…[truncated, {} bytes]", &content[..8192], content.len())
    } else {
        content
    };
    Ok(serde_json::json!({ "content": trunc }).to_string())
}

async fn exec_write_file(root: &Path, args: &str) -> Result<String, String> {
    let rel = parse_arg(args, "path")?;
    let content = parse_arg(args, "content")?;
    let path = safe_path(root, &rel)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }
    tokio::fs::write(&path, &content)
        .await
        .map_err(|e| e.to_string())?;
    Ok(
        serde_json::json!({ "written": path.display().to_string(), "bytes": content.len() })
            .to_string(),
    )
}

// Legacy direct-shell implementation retained for comparison only. It is not
// registered; all model-authored code must use the Supervisor-owned provider.
#[allow(dead_code)]
async fn exec_run_command(root: &Path, args: &str) -> Result<String, String> {
    let cmd = parse_arg(args, "cmd")?;
    let blocked = [
        "rm -rf /",
        "format c:",
        "mkfs",
        "dd if=",
        ":(){",
        "shutdown",
        "reboot",
    ];
    for b in &blocked {
        if cmd.contains(b) {
            return Err(format!("blocked: {b}"));
        }
    }
    let output = tokio::process::Command::new(if cfg!(windows) { "cmd" } else { "sh" })
        .args(if cfg!(windows) {
            vec!["/C", &cmd]
        } else {
            vec!["-c", &cmd]
        })
        .current_dir(root)
        .output()
        .await
        .map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(serde_json::json!({
        "exit_code": output.status.code().unwrap_or(-1),
        "stdout": &stdout[..stdout.len().min(4096)],
        "stderr": &stderr[..stderr.len().min(2048)],
    })
    .to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use kaguya_supervisor::app::SupervisorApp;
    use kaguya_supervisor::config::{ResolvedRuntimeConfig, RuntimeConfig, SandboxConfig};
    use kaguya_supervisor::server;

    use super::*;
    use crate::lifecycle::LifecycleSupervisor;

    #[tokio::test]
    async fn sandbox_tool_dispatch_completes_full_supervisor_chain() {
        let supervisor = SupervisorApp::new(ResolvedRuntimeConfig {
            config: RuntimeConfig {
                profile: Some("test".into()),
                supervisor_addr: "127.0.0.1:0".into(),
                sandbox: SandboxConfig::default(),
                processes: BTreeMap::new(),
            },
            base_dir: ".".into(),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            server::serve_on(supervisor, listener).await.unwrap();
        });

        let sandbox = Arc::new(
            SandboxClient::connect(format!("http://{addr}"))
                .await
                .unwrap(),
        );
        let lifecycle = LifecycleSupervisor::new();
        let tools = ToolRegistry::new(
            std::env::temp_dir(),
            lifecycle.spawner(),
            Some(Arc::clone(&sandbox)),
        );
        let (result_tx, mut result_rx) = mpsc::channel(1);

        tools.dispatch(
            "full-chain-conversation".into(),
            "full-chain-request".into(),
            "sandbox_exec".into(),
            r#"{"language":"python","code":"print(21 * 2)"}"#.into(),
            result_tx,
        );

        let result = result_rx.recv().await.expect("P3 ToolResult");
        let InputEvent::ToolResult {
            request_id,
            tool_name,
            content,
        } = result
        else {
            panic!("expected ToolResult");
        };
        assert_eq!(request_id, "full-chain-request");
        assert_eq!(tool_name, "sandbox_exec");
        println!("full-chain P3 ToolResult: {content}");
        let output: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(output["exit_code"], 0, "{content}");
        assert!(
            output["stdout"].as_str().unwrap().contains("42"),
            "{content}"
        );

        sandbox.release().await;
        server_task.abort();
    }
}
