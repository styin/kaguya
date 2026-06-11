pub mod app;
pub mod config;
pub mod gateway;
pub mod logs;
pub mod process;
pub mod server;

pub mod proto {
    tonic::include_proto!("kaguya.v1");
}
