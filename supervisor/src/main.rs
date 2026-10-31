use kaguya_protocol::session_scope::SessionScope;
use kaguya_supervisor::{
    app::SupervisorApp,
    config::RuntimeConfig,
    deployment::{toml_error, validate_manifest_location, Cli, DeploymentSelection, Operation},
    server,
};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct InstanceManifest {
    schema_version: u32,
    instance_id: String,
    default_session_id: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "kaguya_supervisor=info".into()),
        )
        .init();

    let cli = Cli::parse(std::env::args_os().skip(1))?;
    if matches!(cli.operation, Operation::Help) {
        println!(
            "Kaguya Supervisor\n\n\
             Usage: kaguya-supervisor [run|validate-config|show-config] \
             [--runtime-config PATH] [--profile NAME]\n\n\
             validate-config and show-config resolve deployment selection without \
             launching processes or creating instance data. Use Gateway for \
             init-instance and fork-instance."
        );
        return Ok(());
    }
    if matches!(
        cli.operation,
        Operation::InitInstance | Operation::ForkInstance
    ) {
        anyhow::bail!("init-instance and fork-instance are Gateway commands");
    }
    let selection = DeploymentSelection::discover(cli.options)?;
    let mut resolved = RuntimeConfig::load_selected(&selection)?;
    validate_gateway_settings(&selection)?;
    let scope = validate_instance(&selection)?;
    let addr: std::net::SocketAddr = resolved.config.supervisor_addr.parse()?;
    if matches!(
        cli.operation,
        Operation::ValidateConfig | Operation::ShowConfig
    ) {
        println!(
            "{}",
            serde_json::to_string_pretty(&selection.diagnostics())?
        );
        return Ok(());
    }
    resolved.config.set_instance_session(scope);
    // Bind the control plane before launching Gateway. Gateway initializes its
    // SandboxClient during boot and must never race the Supervisor listener.
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let app = SupervisorApp::new(resolved, scope)?;
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

/// Supervisor validates settings syntax; Gateway owns the full component schema.
fn validate_gateway_settings(selection: &DeploymentSelection) -> anyhow::Result<()> {
    let contents = std::fs::read_to_string(&selection.gateway_settings).map_err(|error| {
        anyhow::anyhow!(
            "cannot read selected Gateway settings {}: {error}",
            selection.gateway_settings.display()
        )
    })?;
    toml::from_str::<toml::Value>(&contents)
        .map_err(|error| toml_error("selected Gateway settings syntax", &contents, &error))?;
    Ok(())
}

/// Read-only preflight: no directories, ports, resource allocations, or leases.
fn validate_instance(selection: &DeploymentSelection) -> anyhow::Result<SessionScope> {
    let manifest_path = selection.require_manifest()?;
    let contents = std::fs::read_to_string(manifest_path)?;
    let manifest: InstanceManifest = toml::from_str(&contents)
        .map_err(|error| toml_error("instance manifest", &contents, &error))?;
    if manifest.schema_version != 1 {
        anyhow::bail!(
            "unsupported instance manifest schema version {}",
            manifest.schema_version
        );
    }
    let scope = SessionScope::parse(&manifest.instance_id, &manifest.default_session_id)?;
    let instance_dir = validate_manifest_location(manifest_path, &manifest.instance_id)?;
    for name in ["SOUL.md", "IDENTITY.md"] {
        let path = instance_dir.join(name);
        if !path.is_file() {
            anyhow::bail!(
                "required persona path is not a regular file: {}",
                path.display()
            );
        }
        std::fs::read_to_string(&path).map_err(|error| {
            anyhow::anyhow!(
                "cannot read required persona file {}: {error}",
                path.display()
            )
        })?;
    }
    let rag_db = instance_dir.join("kaguya.db");
    if rag_db.try_exists()? {
        if !rag_db.is_file() {
            anyhow::bail!(
                "instance RAG database is not a regular file: {}",
                rag_db.display()
            );
        }
        std::fs::File::open(&rag_db).map_err(|error| {
            anyhow::anyhow!(
                "cannot read instance RAG database {}: {error}",
                rag_db.display()
            )
        })?;
    }
    Ok(scope)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct DeploymentFixture {
        root: PathBuf,
        selection: DeploymentSelection,
    }

    impl DeploymentFixture {
        fn new(instance_id: Option<&str>) -> Self {
            let root = std::env::temp_dir().join(format!(
                "kaguya supervisor preflight {} with spaces",
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir(&root).unwrap();
            std::fs::write(root.join("gateway.toml"), "[server]\n").unwrap();
            let instance = instance_id
                .map(|id| format!("\n[instance]\nmanifest = \"instances/{id}/instance.toml\"\n"))
                .unwrap_or_default();
            let runtime = root.join("deployment.toml");
            std::fs::write(
                &runtime,
                format!(
                    "gateway_settings = \"gateway.toml\"\n[paths]\nmode = \"portable\"\nportable_root = \"data\"\n{instance}"
                ),
            )
            .unwrap();
            let selection = DeploymentSelection::load(&runtime, None).unwrap();
            Self { root, selection }
        }

        fn publish_instance(&self, instance_id: &str, schema_version: u32) {
            let manifest = self.selection.manifest.as_ref().unwrap();
            let dir = manifest.parent().unwrap();
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(
                manifest,
                format!(
                    "schema_version = {schema_version}\ninstance_id = \"{instance_id}\"\ndefault_session_id = \"{}\"\n",
                    uuid::Uuid::new_v4()
                ),
            )
            .unwrap();
            std::fs::write(dir.join("SOUL.md"), "soul").unwrap();
            std::fs::write(dir.join("IDENTITY.md"), "persona").unwrap();
        }
    }

    impl Drop for DeploymentFixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    fn preflight_requires_an_initialized_instance_and_writes_nothing() {
        let unset = DeploymentFixture::new(None);
        assert!(validate_gateway_settings(&unset.selection).is_ok());
        assert!(validate_instance(&unset.selection).is_err());
        assert!(!unset.selection.portable_root.exists());

        let missing = DeploymentFixture::new(Some(&uuid::Uuid::new_v4().to_string()));
        assert!(validate_instance(&missing.selection).is_err());
        assert!(!missing.selection.portable_root.exists());
    }

    #[test]
    fn preflight_checks_existing_identity_and_takes_no_lease() {
        let instance_id = uuid::Uuid::new_v4().to_string();
        let fixture = DeploymentFixture::new(Some(&instance_id));
        fixture.publish_instance(&instance_id, 1);
        let scope = validate_instance(&fixture.selection).unwrap();
        assert_eq!(scope.instance_id().to_string(), instance_id);
        let dir = fixture
            .selection
            .manifest
            .as_ref()
            .unwrap()
            .parent()
            .unwrap();
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 3);

        let rag_db = dir.join("kaguya.db");
        std::fs::create_dir(&rag_db).unwrap();
        assert!(validate_instance(&fixture.selection).is_err());
        std::fs::remove_dir(&rag_db).unwrap();
        // Preflight checks readability without opening a SQLite connection.
        std::fs::write(&rag_db, "readable database placeholder").unwrap();
        assert!(validate_instance(&fixture.selection).is_ok());
        std::fs::remove_file(rag_db).unwrap();

        let manifest = fixture.selection.manifest.as_ref().unwrap();
        let original = std::fs::read_to_string(manifest).unwrap();
        std::fs::write(
            manifest,
            format!("{original}unexpected = 'never-print-this'\n"),
        )
        .unwrap();
        let error = validate_instance(&fixture.selection).unwrap_err();
        assert!(!error.to_string().contains("never-print-this"));

        fixture.publish_instance(&instance_id, 2);
        assert!(validate_instance(&fixture.selection).is_err());
        fixture.publish_instance(&uuid::Uuid::new_v4().to_string(), 1);
        assert!(validate_instance(&fixture.selection).is_err());
        fixture.publish_instance(&instance_id, 1);
        std::fs::remove_file(dir.join("SOUL.md")).unwrap();
        assert!(validate_instance(&fixture.selection).is_err());
    }

    #[test]
    fn preflight_rejects_invalid_gateway_settings_syntax() {
        let fixture = DeploymentFixture::new(None);
        std::fs::write(
            &fixture.selection.gateway_settings,
            "token = 'never-print-this'\ninvalid = [",
        )
        .unwrap();
        let error = validate_gateway_settings(&fixture.selection).unwrap_err();
        assert!(!error.to_string().contains("never-print-this"));
    }
}
