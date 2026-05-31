use std::time::Duration;

use tonic::transport::Channel;
use tracing::{debug, info, warn};

use crate::lifecycle::{Readiness, ReconnectPolicy};

pub async fn wait_for_grpc_endpoint<F>(
    name: &str,
    endpoint: &str,
    expected_runtime: bool,
    mut set_readiness: F,
) where
    F: FnMut(Readiness),
{
    let mut warned_unmanaged = false;
    loop {
        if expected_runtime {
            set_readiness(Readiness::Starting);
        }
        match probe_grpc_endpoint(endpoint).await {
            Ok(()) => {
                set_readiness(Readiness::Ready);
                info!(runtime = name, endpoint, "runtime endpoint is ready");
                return;
            }
            Err(e) => {
                if expected_runtime {
                    debug!(
                        runtime = name,
                        endpoint, "runtime endpoint still starting: {e}"
                    );
                } else {
                    set_readiness(Readiness::Degraded);
                    if !warned_unmanaged {
                        warned_unmanaged = true;
                        warn!(
                            runtime = name,
                            endpoint, "unmanaged runtime endpoint unavailable: {e}"
                        );
                    }
                }
            }
        }
        sleep_probe_interval().await;
    }
}

pub async fn probe_grpc_endpoint(endpoint: &str) -> anyhow::Result<()> {
    let timeout = ReconnectPolicy::default().attempt_timeout();
    tokio::time::timeout(timeout, async {
        Channel::from_shared(endpoint.to_string())?
            .connect()
            .await?;
        anyhow::Ok(())
    })
    .await
    .map_err(|_| anyhow::anyhow!("probe timed out after {}ms", timeout.as_millis()))?
}

pub async fn sleep_probe_interval() {
    let delay = ReconnectPolicy::default()
        .retry_delays()
        .last()
        .copied()
        .unwrap_or(Duration::from_secs(1));
    tokio::time::sleep(delay).await;
}
