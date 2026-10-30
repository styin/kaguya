//! Kaguya Gateway entry point.
//!
//! Initializes the async runtime and logging, then runs the application.

use kaguya_gateway::{
    config::GatewayConfig,
    deployment::{Cli, DeploymentSelection, Operation},
    instance,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "kaguya_gateway=info".into()),
        )
        .init();

    let cli = Cli::parse(std::env::args_os().skip(1))?;
    if cli.operation == Operation::Help {
        println!(
            "kaguya-gateway [run | init-instance [--dry-run] | fork-instance | validate-config | show-config] \
             [--runtime-config PATH] [--profile NAME]\n\
             init-instance --dry-run prints what init would use and creates nothing. \
             Instance operations print the new selection and never rewrite deployment configuration."
        );
        return Ok(());
    }
    let selection = DeploymentSelection::discover(cli.options)?;
    let config = GatewayConfig::load_selected(&selection)?;
    let report = match cli.operation {
        Operation::Run => return kaguya_gateway::app::run(selection, config).await,
        Operation::InitInstance if cli.dry_run => {
            instance::check_init(&selection, &config)?;
            let mut report = selection.diagnostics();
            report["operation"] = "init-instance".into();
            report["dry_run"] = true.into();
            report["instances_directory"] =
                serde_json::json!(selection.portable_root.join("instances"));
            report["initialization_sources"] = initialization_sources(&config);
            report
        }
        Operation::InitInstance | Operation::ForkInstance => {
            let (operation, (identity, manifest)) = if cli.operation == Operation::InitInstance {
                (
                    "init-instance",
                    instance::init_instance(&selection, &config)?,
                )
            } else {
                ("fork-instance", instance::fork_instance(&selection)?)
            };
            serde_json::json!({
                "operation": operation,
                "status": "created",
                "instance_id": identity.instance_id,
                "default_session_id": identity.default_session_id,
                "manifest": manifest,
                "instance_directory": manifest.parent(),
                "manifest_reference": format!("instances/{}/instance.toml", identity.instance_id),
                "selection_changed": false,
            })
        }
        Operation::ValidateConfig | Operation::ShowConfig => {
            config.server.ws_addr.parse::<std::net::SocketAddr>()?;
            config.server.grpc_addr.parse::<std::net::SocketAddr>()?;
            let (identity, _) = instance::inspect(selection.require_manifest()?)?;
            let mut report = selection.diagnostics();
            report["instance"] = serde_json::json!({
                "instance_id": identity.instance_id,
                "default_session_id": identity.default_session_id,
            });
            report
        }
        Operation::Help => unreachable!("help returns before deployment selection"),
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn initialization_sources(config: &GatewayConfig) -> serde_json::Value {
    serde_json::json!({
        "soul_path": config.files.soul_path,
        "persona_identity_path": config.files.identity_path,
    })
}
