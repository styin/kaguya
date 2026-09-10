use kaguya_supervisor::{app::SupervisorApp, config::RuntimeConfig, server};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "kaguya_supervisor=info".into()),
        )
        .init();

    let resolved = RuntimeConfig::load_discover()?;
    let addr: std::net::SocketAddr = resolved.config.supervisor_addr.parse()?;
    // Bind the control plane before launching Gateway. Gateway initializes its
    // SandboxClient during boot and must never race the Supervisor listener.
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let app = SupervisorApp::new(resolved);
    app.prewarm_sandbox().await;
    app.start_monitor();
    if supervisor_autostart() {
        app.start_app().await?;
    }

    tokio::select! {
        result = server::serve_on(app.clone(), listener) => result?,
        signal = tokio::signal::ctrl_c() => {
            match signal {
                Ok(()) => tracing::info!("OS shutdown signal received"),
                Err(e) => tracing::warn!("failed to listen for OS shutdown signal: {e}"),
            }
            app.shutdown_app().await?;
        }
    }

    Ok(())
}

fn supervisor_autostart() -> bool {
    std::env::var("KAGUYA_SUPERVISOR_AUTOSTART")
        .map(|value| {
            !matches!(
                value.to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(true)
}
