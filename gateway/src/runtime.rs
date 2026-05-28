//! Gateway runtime orchestration helpers.
//!
//! `main.rs` declares startup order; this module owns runtime-level policy such
//! as whether Gateway may claim endpoints for managed child processes.

use crate::config::{Activation, RuntimeConfig};
use crate::lifecycle::ReconnectPolicy;

pub async fn preflight_managed_runtime_endpoints(runtime: &RuntimeConfig) -> anyhow::Result<()> {
    let conflicts = managed_runtime_endpoint_conflicts(runtime).await;
    if conflicts.is_empty() {
        return Ok(());
    }

    anyhow::bail!(
        "managed runtime endpoint conflict(s): {}. Stop the stale process or use standalone mode.",
        conflicts.join(", ")
    );
}

async fn managed_runtime_endpoint_conflicts(runtime: &RuntimeConfig) -> Vec<String> {
    let mut conflicts = Vec::new();
    for (runtime_id, spec) in &runtime.runtimes {
        if !spec.enabled || !spec.managed || spec.activation != Some(Activation::Eager) {
            continue;
        }

        for (endpoint_name, endpoint) in &spec.endpoints {
            if endpoint_is_reachable(endpoint).await {
                conflicts.push(format!("{runtime_id}.{endpoint_name}={endpoint}"));
            }
        }
    }
    conflicts
}

async fn endpoint_is_reachable(endpoint: &str) -> bool {
    let Some(addr) = endpoint_socket_addr(endpoint) else {
        return false;
    };
    tokio::time::timeout(
        ReconnectPolicy::default().attempt_timeout(),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    .is_ok_and(|result| result.is_ok())
}

fn endpoint_socket_addr(endpoint: &str) -> Option<String> {
    let without_scheme = endpoint
        .strip_prefix("http://")
        .or_else(|| endpoint.strip_prefix("https://"))
        .unwrap_or(endpoint);
    let host_port = without_scheme.split('/').next()?.trim();
    if host_port.is_empty() || !host_port.contains(':') {
        return None;
    }
    Some(host_port.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_socket_addr_extracts_host_port() {
        assert_eq!(
            endpoint_socket_addr("http://127.0.0.1:50053"),
            Some("127.0.0.1:50053".to_string())
        );
        assert_eq!(
            endpoint_socket_addr("127.0.0.1:50056"),
            Some("127.0.0.1:50056".to_string())
        );
        assert_eq!(endpoint_socket_addr("http://localhost"), None);
    }

    #[tokio::test]
    async fn preflight_reports_reachable_managed_eager_endpoint() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener should bind");
        let addr = listener
            .local_addr()
            .expect("test listener should expose local addr");

        let toml = format!(
            r#"
            manage_processes = true

            [runtimes.voice_stack]
            enabled = true
            managed = true
            activation = "eager"
            command = "test"

            [runtimes.voice_stack.endpoints]
            talker_grpc = "http://{addr}"
            "#
        );
        let runtime: RuntimeConfig = toml::from_str(&toml).expect("runtime config should parse");

        let conflicts = managed_runtime_endpoint_conflicts(&runtime).await;

        assert_eq!(
            conflicts,
            vec![format!("voice_stack.talker_grpc=http://{addr}")]
        );
    }

    #[tokio::test]
    async fn preflight_ignores_unmanaged_runtime_endpoint() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener should bind");
        let addr = listener
            .local_addr()
            .expect("test listener should expose local addr");

        let toml = format!(
            r#"
            manage_processes = true

            [runtimes.reasoner]
            enabled = true
            managed = false
            activation = "eager"

            [runtimes.reasoner.endpoints]
            grpc = "http://{addr}"
            "#
        );
        let runtime: RuntimeConfig = toml::from_str(&toml).expect("runtime config should parse");

        assert!(managed_runtime_endpoint_conflicts(&runtime)
            .await
            .is_empty());
    }
}
