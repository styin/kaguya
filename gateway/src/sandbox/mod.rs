//! Client for the Supervisor-owned Sandbox Provider.
//!
//! The Gateway keeps only Tool Manager concerns: advertise `sandbox_exec`,
//! request opaque handles, forward execution, and release allocations.
//! Backend choice and lifecycle remain in Supervisor.

use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::instance::InstanceIdentity;
use crate::proto;

pub struct SandboxClient {
    base_url: String,
    http: reqwest::Client,
    enabled: bool,
    backend: Option<String>,
    instance_id: String,
    session_id: String,
    allocation: Mutex<Option<AllocationLease>>,
}

struct AllocationLease {
    allocation_id: String,
    handle: String,
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
    instance_id: &'a str,
    session_id: &'a str,
    allocation_id: &'a str,
}

#[derive(serde::Deserialize)]
struct AcquireResponse {
    handle: Option<String>,
    error: Option<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecuteRequest<'a> {
    instance_id: &'a str,
    session_id: &'a str,
    request_id: &'a str,
    args_json: &'a str,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ReleaseRequest<'a> {
    instance_id: &'a str,
    session_id: &'a str,
}

#[derive(serde::Deserialize)]
struct ReleaseResponse {
    ok: bool,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct ExecuteResponse {
    content: String,
}

impl SandboxClient {
    pub async fn connect(
        base_url: impl Into<String>,
        instance_context: &InstanceIdentity,
    ) -> anyhow::Result<Self> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let http = reqwest::Client::new();
        http.post(format!("{base_url}/api/owner/bind"))
            .json(&serde_json::json!({
                "instanceId": instance_context.instance_id,
                "sessionId": instance_context.default_session_id,
            }))
            .send()
            .await?
            .error_for_status()?;
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
            instance_id: instance_context.instance_id.clone(),
            session_id: instance_context.default_session_id.clone(),
            allocation: Mutex::new(None),
        })
    }

    pub fn disabled() -> Self {
        Self {
            base_url: String::new(),
            http: reqwest::Client::new(),
            enabled: false,
            backend: None,
            instance_id: String::new(),
            session_id: String::new(),
            allocation: Mutex::new(None),
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
                          the active allocation. Optional 'stdin' is fed to the program. \
                          Languages: python, node, bash."
                .into(),
            args_schema: r#"{"type":"object","properties":{"language":{"type":"string","enum":["python","node","bash"]},"code":{"type":"string"},"stdin":{"type":"string"}},"required":["language","code"]}"#
                .into(),
        })
    }

    pub async fn exec_from_json(&self, request_id: &str, args_json: &str) -> String {
        if !self.enabled {
            return error_json("sandbox provider is unavailable");
        }

        let handle = match self.ensure_handle().await {
            Ok(handle) => handle,
            Err(error) => return error_json(error),
        };
        debug!(
            session_id = %self.session_id,
            request_id = %request_id,
            handle = %handle,
            backend = self.backend.as_deref().unwrap_or("unknown"),
            "forwarding sandbox execution to Supervisor"
        );
        let response = self
            .http
            .post(format!("{}/api/sandbox/{handle}/execute", self.base_url))
            .json(&ExecuteRequest {
                instance_id: &self.instance_id,
                session_id: &self.session_id,
                request_id,
                args_json,
            })
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
        let mut current = self.allocation.lock().await;
        let Some(lease) = current.as_ref() else {
            return;
        };
        let result: Result<ReleaseResponse, String> = async {
            self.http
                .delete(format!("{}/api/sandbox/{}", self.base_url, lease.handle))
                .json(&ReleaseRequest {
                    instance_id: &self.instance_id,
                    session_id: &self.session_id,
                })
                .send()
                .await
                .map_err(|error| error.to_string())?
                .error_for_status()
                .map_err(|error| error.to_string())?
                .json::<ReleaseResponse>()
                .await
                .map_err(|error| error.to_string())
        }
        .await;
        match result {
            Ok(response) if response.ok => {
                info!(handle = %lease.handle, allocation_id = %lease.allocation_id, "released Supervisor sandbox handle");
                *current = None;
            }
            Ok(response) => {
                warn!(handle = %lease.handle, allocation_id = %lease.allocation_id, error = ?response.error, "Supervisor rejected sandbox release");
            }
            Err(error) => {
                warn!(%error, handle = %lease.handle, allocation_id = %lease.allocation_id, "failed to release Supervisor sandbox handle");
            }
        }
    }

    async fn ensure_handle(&self) -> Result<String, String> {
        let mut current = self.allocation.lock().await;
        if let Some(lease) = current.as_ref() {
            debug!(allocation_id = %lease.allocation_id, handle = %lease.handle, "reusing Supervisor sandbox handle");
            return Ok(lease.handle.clone());
        }
        let allocation_id = uuid::Uuid::new_v4().to_string();

        let response = self
            .http
            .post(format!("{}/api/sandbox/acquire", self.base_url))
            .json(&AcquireRequest {
                instance_id: &self.instance_id,
                session_id: &self.session_id,
                allocation_id: &allocation_id,
            })
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

        *current = Some(AllocationLease {
            allocation_id: allocation_id.clone(),
            handle: handle.clone(),
        });
        info!(
            %allocation_id,
            %handle,
            backend = self.backend.as_deref().unwrap_or("unknown"),
            "acquired Supervisor sandbox handle"
        );
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
    use std::time::Duration;

    use kaguya_supervisor::app::SupervisorApp;
    use kaguya_supervisor::config::{ResolvedRuntimeConfig, RuntimeConfig, SandboxConfig};
    use kaguya_supervisor::server;

    use super::*;

    fn test_instance_context() -> InstanceIdentity {
        InstanceIdentity {
            schema_version: 1,
            instance_id: uuid::Uuid::new_v4().to_string(),
            default_session_id: uuid::Uuid::new_v4().to_string(),
        }
    }

    #[tokio::test]
    async fn concurrent_acquisitions_preserve_shared_handle_and_files() {
        let app = SupervisorApp::new(ResolvedRuntimeConfig {
            config: RuntimeConfig {
                profile: Some("test".into()),
                supervisor_addr: "127.0.0.1:0".into(),
                sandbox: SandboxConfig::default(),
                processes: BTreeMap::new(),
            },
            base_dir: ".".into(),
        });
        let router = server::router(app.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let client = SandboxClient::connect(format!("http://{addr}"), &test_instance_context())
            .await
            .unwrap();
        let first_request = uuid::Uuid::new_v4().to_string();
        let second_request = uuid::Uuid::new_v4().to_string();
        let outputs = tokio::time::timeout(Duration::from_secs(10), async {
            let (first, second) = tokio::join!(
                client.exec_from_json(
                    &first_request,
                    r#"{"language":"python","code":"open('first.txt','w').write('first')"}"#,
                ),
                client.exec_from_json(
                    &second_request,
                    r#"{"language":"python","code":"open('second.txt','w').write('second')"}"#,
                ),
            );
            let third = client.exec_from_json(
                "third-request",
                r#"{"language":"python","code":"print(open('first.txt').read() + ':' + open('second.txt').read())"}"#,
            ).await;
            [first, second, third]
        }).await;
        server_task.abort();
        app.shutdown_app().await.unwrap();

        let outputs = outputs.expect("concurrent acquisition must complete");
        for output in &outputs {
            let value: serde_json::Value = serde_json::from_str(output).unwrap();
            assert_eq!(value["exit_code"], 0, "{output}");
            assert!(value["error"].is_null(), "{output}");
        }
        let third: serde_json::Value = serde_json::from_str(&outputs[2]).unwrap();
        assert_eq!(third["stdout"].as_str().unwrap().trim(), "first:second");
    }

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

        let client = SandboxClient::connect(format!("http://{addr}"), &test_instance_context())
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

    #[tokio::test]
    async fn release_reacquires_allocation_without_replacing_session() {
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

        let client = SandboxClient::connect(format!("http://{addr}"), &test_instance_context())
            .await
            .unwrap();

        let write = client
            .exec_from_json(
                "session-a",
                r#"{"language":"python","code":"open('secret.txt','w').write('from-a')"}"#,
            )
            .await;
        let write_value: serde_json::Value = serde_json::from_str(&write).unwrap();
        assert_eq!(write_value["exit_code"], 0, "{write}");
        let old_allocation_id = client
            .allocation
            .lock()
            .await
            .as_ref()
            .unwrap()
            .allocation_id
            .clone();
        client.release().await;

        let read_other = client
            .exec_from_json(
                "second-request",
                r#"{"language":"python","code":"import pathlib; print(pathlib.Path('secret.txt').exists())"}"#,
            )
            .await;
        let read_other_value: serde_json::Value = serde_json::from_str(&read_other).unwrap();
        assert_eq!(read_other_value["exit_code"], 0, "{read_other}");
        let new_allocation_id = client
            .allocation
            .lock()
            .await
            .as_ref()
            .unwrap()
            .allocation_id
            .clone();
        assert_ne!(old_allocation_id, new_allocation_id);
        assert!(
            read_other_value["stdout"]
                .as_str()
                .unwrap()
                .contains("False"),
            "{read_other}"
        );

        client.release().await;
        server_task.abort();
    }
}
