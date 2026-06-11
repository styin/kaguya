use std::time::Duration;

use tonic::transport::Channel;

use crate::proto;
use crate::proto::router_control_service_client::RouterControlServiceClient;

pub async fn request_gateway_shutdown(endpoint: &str, timeout: Duration) -> anyhow::Result<()> {
    let mut client = tokio::time::timeout(timeout, async {
        RouterControlServiceClient::connect(endpoint.to_string()).await
    })
    .await
    .map_err(|_| anyhow::anyhow!("Gateway control connect timed out"))??;

    let signal = proto::ControlSignal {
        signal: Some(proto::control_signal::Signal::Shutdown(
            proto::ShutdownSignal {},
        )),
    };

    tokio::time::timeout(timeout, client.send_control(signal))
        .await
        .map_err(|_| anyhow::anyhow!("Gateway shutdown request timed out"))??;

    Ok(())
}

pub async fn fetch_capability_status(endpoint: &str) -> Option<serde_json::Value> {
    let base = endpoint
        .strip_suffix("/capabilities/status")
        .unwrap_or(endpoint)
        .trim_end_matches('/');
    let url = format!("{base}/capabilities/status");
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(5))
        .build()
        .ok()?;
    let response = client.get(url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json::<serde_json::Value>().await.ok()
}

#[allow(dead_code)]
async fn _channel(endpoint: &str) -> anyhow::Result<Channel> {
    Ok(Channel::from_shared(endpoint.to_string())?
        .connect()
        .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// BUG-SCOPING: `fetch_capability_status` uses `reqwest::get` without any
    /// timeout. If the endpoint stalls (accepts TCP but never responds), the
    /// call hangs indefinitely, blocking the supervisor's status refresh path.
    #[tokio::test]
    async fn fetch_capability_status_should_not_hang_on_stalled_endpoint() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Accept connections but never respond — simulates a half-open endpoint.
        tokio::spawn(async move {
            loop {
                if let Ok((socket, _)) = listener.accept().await {
                    tokio::spawn(async move {
                        let _hold = socket;
                        tokio::time::sleep(Duration::from_secs(300)).await;
                    });
                }
            }
        });

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            fetch_capability_status(&format!("http://{addr}")),
        )
        .await;

        assert!(
            result.is_ok(),
            "fetch_capability_status hung for >5s on a stalled endpoint; \
             it should have an internal timeout"
        );
    }
}
