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
    let addr = resolved.config.supervisor_addr.parse()?;
    let app = SupervisorApp::new(resolved);
    app.start_monitor();
    app.start_app().await?;

    tokio::select! {
        result = server::serve(app.clone(), addr) => result?,
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
