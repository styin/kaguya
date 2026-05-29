fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "kaguya_supervisor=info".into()),
        )
        .init();

    tracing::info!("Kaguya Supervisor package initialized");
}
