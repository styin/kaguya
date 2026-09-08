pub mod app;
pub mod config;
pub mod gateway;
pub mod logs;
pub mod process;
pub mod sandbox;
pub mod server;
pub mod telemetry;

pub mod proto {
    tonic::include_proto!("kaguya.v1");
}
