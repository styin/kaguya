//! Endpoint probing utilities for gRPC connection readiness.
//!
//! Used by the Talker and Listener recovery loops to poll an endpoint until
//! it becomes reachable before attempting a full client connection.

use std::time::Duration;

use tonic::transport::Channel;
use tracing::{debug, info, warn};

use crate::lifecycle::{Readiness, ReconnectPolicy};

/// Block until `endpoint` accepts a gRPC connection, updating readiness via
/// `set_readiness` on each probe cycle.
///
/// For managed runtimes (`expected_runtime = true`), readiness stays
/// [`Readiness::Starting`] while probing. For unmanaged/external runtimes,
/// readiness is set to [`Readiness::Degraded`] on the first failure.
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

/// Single-shot probe: attempt to open a gRPC channel within the default
/// reconnect policy timeout.
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

/// Sleep for the default probe interval (the last retry delay in the default
/// reconnect policy).
pub async fn sleep_probe_interval() {
    let delay = ReconnectPolicy::default()
        .retry_delays()
        .last()
        .copied()
        .unwrap_or(Duration::from_secs(1));
    tokio::time::sleep(delay).await;
}
