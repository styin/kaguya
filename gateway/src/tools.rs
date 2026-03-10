//! 工具注册表 + 调度。
//!
//! 工具调用由 Talker 发起（[TOOL:...]），不是 Gateway。
//! Gateway 只负责：维护注册表、执行调度、将结果作为 P3 事件回流。

use tokio::sync::mpsc;
use tracing::info;
use crate::types::*;

pub struct ToolRegistry {
    tools: Vec<ToolInfo>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: vec![
                ToolInfo {
                    name: "list_files".into(),
                    description: "List files in directory".into(),
                    parameters_schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"}}}),
                },
                ToolInfo {
                    name: "read_file".into(),
                    description: "Read file contents".into(),
                    parameters_schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"}}}),
                },
                ToolInfo {
                    name: "write_file".into(),
                    description: "Write to file".into(),
                    parameters_schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}}}),
                },
                ToolInfo {
                    name: "run_command".into(),
                    description: "Run shell command in sandbox".into(),
                    parameters_schema: serde_json::json!({"type":"object","properties":{"cmd":{"type":"string"}}}),
                },
            ],
        }
    }

    pub fn list(&self) -> Vec<ToolInfo> {
        self.tools.clone()
    }

    /// 异步调度工具。结果作为 P3 事件回流到 Input Stream。
    pub fn dispatch(
        &self,
        call_id: String,
        tool_name: String,
        _params: serde_json::Value,
        p3_tx: mpsc::Sender<InputEvent>,
    ) {
        info!(tool = %tool_name, call_id = %call_id, "dispatching tool");
        tokio::spawn(async move {
            // TODO: 在沙箱 TypeScript 进程中执行
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let result = serde_json::json!({"output": "stub", "tool": tool_name});
            let _ = p3_tx
                .send(InputEvent::ToolResult {
                    call_id,
                    tool_name,
                    result,
                    success: true,
                    error: None,
                })
                .await;
        });
    }
}