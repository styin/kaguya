pub mod app;
pub mod config;
#[path = "../../shared/deployment.rs"]
pub mod deployment;
pub mod gateway;
pub mod logs;
pub mod process;
pub mod sandbox;
pub mod server;
#[path = "../../shared/session_scope.rs"]
pub mod session_scope;
pub mod telemetry;

pub mod proto {
    tonic::include_proto!("kaguya.v1");
}
