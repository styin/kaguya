//! Client for the Supervisor-owned Sandbox Provider.
//!
//! The Gateway keeps only Tool Manager concerns: advertise `sandbox_exec`,
//! request an opaque handle, forward execution, and release the handle when
//! its conversation ends. Backend choice and lifecycle remain in Supervisor.

use tokio::sync::Mutex;

use crate::proto;

pub struct SandboxClient {
    base_url: String,
    http: reqwest::Client,
    enabled: bool,
    backend: Option<String>,
    handle: Mutex<Option<String>>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct StatusResponse {
    enabled: bool,
    backend: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AcquireRequest<'a> {
    session_id: &'a str,
}

#[derive(serde::Deserialize)]
struct AcquireResponse {
    handle: Option<String>,
    error: Option<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecuteRequest<'a> {
    args_json: &'a str,
}

#[derive(serde::Deserialize)]
struct ExecuteResponse {
    content: String,
}

impl SandboxClient {
    pub async fn connect(base_url: impl Into<String>) -> anyhow::Result<Self> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let http = reqwest::Client::new();
        let status = http
            .get(format!("{base_url}/api/sandbox/status"))
            .send()
            .await?
            .error_for_status()?
            .json::<StatusResponse>()
            .await?;

        Ok(Self {
            base_url,
            http,
            enabled: status.enabled,
            backend: Some(status.backend),
            handle: Mutex::new(None),
        })
    }

    pub fn disabled() -> Self {
        Self {
            base_url: String::new(),
            http: reqwest::Client::new(),
            enabled: false,
            backend: None,
            handle: Mutex::new(None),
        }
    }

    pub fn backend(&self) -> Option<&str> {
        self.backend.as_deref()
    }

    pub fn tool_definition(&self) -> Option<proto::ToolDefinition> {
        self.enabled.then(|| proto::ToolDefinition {
            name: "sandbox_exec".into(),
            description: "Execute code through the Supervisor-managed runtime provider and \
                          return {stdout, stderr, exit_code}. Files persist across calls in \
                          this conversation. Optional 'stdin' is fed to the program. \
                          Languages: python, node, bash."
                .into(),
            args_schema: r#"{"type":"object","properties":{"language":{"type":"string","enum":["python","node","bash"]},"code":{"type":"string"},"stdin":{"type":"string"}},"required":["language","code"]}"#
                .into(),
        })
    }

    pub async fn exec_from_json(&self, session_id: &str, args_json: &str) -> String {
        if !self.enabled {
            return error_json("sandbox provider is unavailable");
        }

        let handle = match self.ensure_handle(session_id).await {
            Ok(handle) => handle,
            Err(error) => return error_json(error),
        };
        let response = self
            .http
            .post(format!("{}/api/sandbox/{handle}/execute", self.base_url))
            .json(&ExecuteRequest { args_json })
            .send()
            .await;

        match response {
            Ok(response) => match response.error_for_status() {
                Ok(response) => match response.json::<ExecuteResponse>().await {
                    Ok(response) => response.content,
                    Err(error) => error_json(format!("invalid Supervisor response: {error}")),
                },
                Err(error) => error_json(format!("Supervisor rejected execution: {error}")),
            },
            Err(error) => error_json(format!("Supervisor execution request failed: {error}")),
        }
    }

    pub async fn release(&self) {
        let Some(handle) = self.handle.lock().await.take() else {
            return;
        };
        if let Err(error) = self
            .http
            .delete(format!("{}/api/sandbox/{handle}", self.base_url))
            .send()
            .await
        {
            tracing::warn!(%error, %handle, "failed to release Supervisor sandbox handle");
        }
    }

    async fn ensure_handle(&self, session_id: &str) -> Result<String, String> {
        let mut current = self.handle.lock().await;
        if let Some(handle) = current.as_ref() {
            return Ok(handle.clone());
        }

        let response = self
            .http
            .post(format!("{}/api/sandbox/acquire", self.base_url))
            .json(&AcquireRequest { session_id })
            .send()
            .await
            .map_err(|error| format!("failed to acquire sandbox: {error}"))?
            .error_for_status()
            .map_err(|error| format!("Supervisor rejected sandbox acquisition: {error}"))?
            .json::<AcquireResponse>()
            .await
            .map_err(|error| format!("invalid Supervisor response: {error}"))?;

        let handle = response.handle.ok_or_else(|| {
            response
                .error
                .unwrap_or_else(|| "sandbox unavailable".into())
        })?;
        *current = Some(handle.clone());
        Ok(handle)
    }
}

fn error_json(error: impl Into<String>) -> String {
    serde_json::json!({
        "stdout": "",
        "stderr": "",
        "exit_code": -1,
        "timed_out": false,
        "truncated": false,
        "error": error.into(),
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use kaguya_supervisor::app::SupervisorApp;
    use kaguya_supervisor::config::{ResolvedRuntimeConfig, RuntimeConfig, SandboxConfig};
    use kaguya_supervisor::server;

    use super::*;

    #[tokio::test]
    async fn client_uses_supervisor_handle_contract_end_to_end() {
        let app = SupervisorApp::new(ResolvedRuntimeConfig {
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
            server::serve_on(app, listener).await.unwrap();
        });

        let client = SandboxClient::connect(format!("http://{addr}"))
            .await
            .unwrap();
        assert_eq!(client.backend(), Some("native"));
        assert!(client.tool_definition().is_some());

        let output = client
            .exec_from_json(
                "gateway-client-contract",
                r#"{"language":"python","code":"print('gateway-client')"}"#,
            )
            .await;
        let value: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["exit_code"], 0, "{output}");
        assert!(value["stdout"].as_str().unwrap().contains("gateway-client"));

        client.release().await;
        server_task.abort();
    }
}
