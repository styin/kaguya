use std::time::Duration;

use crate::proto;
use crate::proto::router_control_service_client::RouterControlServiceClient;
use crate::session_scope::SessionScope;

pub async fn request_gateway_shutdown(
    endpoint: &str,
    scope: SessionScope,
    timeout: Duration,
) -> anyhow::Result<()> {
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

    tokio::time::timeout(
        timeout,
        client.send_control(proto::ControlRequest {
            session: Some(scope.to_proto()),
            signal: Some(signal),
        }),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Gateway shutdown request timed out"))??;

    Ok(())
}
