//! Tool Registry and Dispatcher
//!
//! Tool dispatch initiated by Talker, executed by gateway ([TOOL:...] in prompt)
//! Results sent back to Talker via P3 channel.

use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use tracing::{info, error};

use crate::proto;
use crate::types::InputEvent;

struct ToolMeta {
    name: String,
    description: String,
    args_schema: String,
}

pub struct ToolRegistry {
    tools: Vec<ToolMeta>,
    workspace_root: PathBuf,
}

impl ToolRegistry {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            tools: vec![
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
                // DISABLED: run_command requires allowlist-based sandboxing before re-enabling.
                // See GitHub issue for scoped implementation plan.
                // ToolMeta {
                //     name: "run_command".into(),
                //     description: "Run shell command in sandbox".into(),
                //     args_schema: r#"{"type":"object","properties":{"cmd":{"type":"string"}}}"#.into(),
                // },
            ],
            workspace_root,
        }
    }

    pub fn definitions(&self) -> Vec<proto::ToolDefinition> {
        self.tools.iter().map(|t| proto::ToolDefinition {
            name: t.name.clone(),
            description: t.description.clone(),
            args_schema: t.args_schema.clone(),
        }).collect()
    }

    pub fn dispatch(
        &self,
        request_id: String,
        tool_name: String,
        args_json: String,
        p3_tx: mpsc::Sender<InputEvent>,
    ) {
        info!(tool = %tool_name, id = %request_id, "dispatching tool");
        let root = self.workspace_root.clone();

        tokio::spawn(async move {
            let result = match tool_name.as_str() {
                "list_files"   => exec_list_files(&root, &args_json).await,
                "read_file"    => exec_read_file(&root, &args_json).await,
                "write_file"   => exec_write_file(&root, &args_json).await,
                // DISABLED: see tool registration above
                // "run_command"  => exec_run_command(&root, &args_json).await,
                other          => Err(format!("unknown tool: {other}")),
            };

            let content = match result {
                Ok(o) => o,
                Err(e) => {
                    error!(tool = %tool_name, err = %e, "tool failed");
                    serde_json::json!({ "error": e }).to_string()
                }
            };

            let _ = p3_tx.send(InputEvent::ToolResult { request_id, tool_name, content }).await;
        });
    }
}

// ── helpers ──

fn parse_arg(json: &str, key: &str) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| format!("bad JSON: {e}"))?;
    v.get(key).and_then(|v| v.as_str()).map(String::from)
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
        let is_dir = entry.file_type().await.map(|ft| ft.is_dir()).unwrap_or(false);
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
    let content = tokio::fs::read_to_string(&path).await.map_err(|e| e.to_string())?;
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
        tokio::fs::create_dir_all(parent).await.map_err(|e| e.to_string())?;
    }
    tokio::fs::write(&path, &content).await.map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "written": path.display().to_string(), "bytes": content.len() }).to_string())
}

#[allow(dead_code)] // Disabled in registry pending allowlist sandbox; kept for re-enable.
async fn exec_run_command(root: &Path, args: &str) -> Result<String, String> {
    let cmd = parse_arg(args, "cmd")?;
    let blocked = ["rm -rf /", "format c:", "mkfs", "dd if=", ":(){", "shutdown", "reboot"];
    for b in &blocked {
        if cmd.contains(b) { return Err(format!("blocked: {b}")); }
    }
    let output = tokio::process::Command::new(if cfg!(windows) { "cmd" } else { "sh" })
        .args(if cfg!(windows) { vec!["/C", &cmd] } else { vec!["-c", &cmd] })
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
    }).to_string())
}