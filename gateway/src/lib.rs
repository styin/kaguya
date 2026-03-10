pub mod config;
pub mod context;
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
    pub mod listener {
        tonic::include_proto!("kaguya.listener");
    }
    pub mod talker {
        tonic::include_proto!("kaguya.talker");
    }
    pub mod reasoner {
        tonic::include_proto!("kaguya.reasoner");
    }
    pub mod gateway {
        tonic::include_proto!("kaguya.gateway");
    }
}