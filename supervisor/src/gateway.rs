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
    let response = reqwest::get(url).await.ok()?;
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
