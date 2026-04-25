pub mod config;
pub mod context;
pub mod control;
#[cfg(feature = "dev-console")]
pub mod endpoint;
pub mod error;
pub mod history;
pub mod input_stream;
pub mod listener;
pub mod memory;
pub mod narration;
pub mod output;
pub mod persona;
pub mod reasoner;
pub mod silence;
pub mod talker;
pub mod tools;
pub mod types;

pub mod proto {
    tonic::include_proto!("kaguya.v1");
}