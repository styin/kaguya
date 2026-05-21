pub mod config;
pub mod error;
pub mod rag;
pub mod tools;

pub mod clients {
    pub mod listener;
    pub mod reasoner;
    pub mod talker;
}

pub mod core {
    pub mod context;
    pub mod history;
    pub mod input_stream;
    pub mod narration;
    pub mod output;
    pub mod persona;
    pub mod silence;
    pub mod types;
}

pub mod services {
    pub mod control;
    #[cfg(feature = "dev-console")]
    pub mod endpoint;
}

pub use clients::{listener, reasoner, talker};
pub use core::{context, history, input_stream, narration, output, persona, silence, types};
pub use services::control;
#[cfg(feature = "dev-console")]
pub use services::endpoint;

pub mod proto {
    tonic::include_proto!("kaguya.v1");
}
