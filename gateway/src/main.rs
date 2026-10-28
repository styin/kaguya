//! Kaguya Gateway entry point.
//!
//! Initializes the async runtime and logging, then runs the application.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "kaguya_gateway=info".into()),
        )
        .init();

    use kaguya_gateway::{
        config::GatewayConfig,
        deployment::{Cli, DeploymentSelection, Operation},
        instance,
    };
    let cli = Cli::parse(std::env::args_os().skip(1))?;
    if cli.operation == Operation::Help {
        println!("kaguya-gateway [run | init-instance | rekey-instance | validate-config | show-config] [--runtime-config PATH] [--profile NAME]\nvalidate-config and show-config accept --for-init. Instance operations print the new selection and never rewrite deployment configuration.");
        return Ok(());
    }
    let selection = DeploymentSelection::discover(cli.options)?;
    let config = GatewayConfig::load_selected(&selection)?;
    match cli.operation {
        Operation::Run => kaguya_gateway::app::run(selection, config).await,
        Operation::InitInstance | Operation::RekeyInstance => {
            let (instance_identity, manifest) = if cli.operation == Operation::InitInstance {
                instance::init_instance(&selection, &config)?
            } else {
                instance::rekey_instance(&selection)?
            };
            let report = serde_json::json!({
                "operation": if cli.operation == Operation::InitInstance { "init-instance" } else { "rekey-instance" },
                "status": "created",
                "instance_id": instance_identity.instance_id,
                "default_session_id": instance_identity.default_session_id,
                "manifest": manifest,
                "instance_directory": manifest.parent(),
                "manifest_reference": format!("instances/{}/instance.toml", instance_identity.instance_id),
                "selection_changed": false,
            });
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        Operation::ValidateConfig | Operation::ShowConfig => {
            config.server.ws_addr.parse::<std::net::SocketAddr>()?;
            config.server.grpc_addr.parse::<std::net::SocketAddr>()?;
            let instance_state = instance::validate_selection(&selection, cli.for_init)?;
            if cli.for_init && instance_state.is_none() {
                instance::validate_initialization_sources(&config)?;
            }
            let mut report = selection.diagnostics();
            report["validation_mode"] = serde_json::json!(if cli.for_init {
                "initialization"
            } else {
                "startup"
            });
            report["instance"] = instance_state.as_ref().map(|(identity, _)| serde_json::json!({ "instance_id": identity.instance_id, "default_session_id": identity.default_session_id })).unwrap_or(serde_json::Value::Null);
            report["initialization_sources"] = serde_json::json!({ "soul_path": config.files.soul_path, "persona_identity_path": config.files.identity_path });
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        Operation::Help => unreachable!(),
    }
}
