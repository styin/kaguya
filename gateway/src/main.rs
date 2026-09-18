//! Kaguya Gateway entry point.
//!
//! Initializes the async runtime and logging, then runs the application.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "kaguya_gateway=info".into()),
        )
        .init();

    kaguya_gateway::app::run().await
}
