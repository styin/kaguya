//! Kaguya Gateway crate — deterministic orchestrator for the voice-first AI
//! Chief of Staff.
//!
//! The Gateway owns the main event loop, conversation state, priority-ordered
//! input stream, and gRPC/WebSocket client connections to adjacent processes
//! (Talker, Listener, Reasoner). It never touches the filesystem directly and
//! never runs an LLM — those responsibilities belong to the processes it
//! connects to.
//!
//! # Module layout
//!
//! - [`clients`] — gRPC/TCP client wrappers and connection recovery loops.
//! - [`core`] — Conversation state, priority input stream, and output routing.
//! - [`lifecycle`] — Task supervision, connection readiness, and reconnect policies.
//! - [`services`] — Inbound gRPC/WebSocket servers the Gateway exposes.
//! - [`config`] — Gateway-local and runtime topology configuration.
//! - [`rag`] — Retrieval-augmented generation (embedding, retrieval, memory store).
//! - [`tools`] — Tool registry and dispatch for the Talker's tool-use protocol.

pub mod config;
pub mod error;
pub mod lifecycle;
pub mod rag;
pub mod tools;

/// gRPC/TCP client wrappers for adjacent processes.
pub mod clients {
    pub mod audio_sink;
    pub mod listener;
    pub mod probe;
    pub mod reasoner;
    pub mod talker;
}

/// Conversation state, priority input stream, and output routing.
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

/// Inbound servers the Gateway exposes (gRPC control, WebSocket endpoint).
pub mod services {
    pub mod control;
    #[cfg(feature = "dev-console")]
    pub mod endpoint;
}

pub use clients::{audio_sink, listener, probe, reasoner, talker};
pub use core::{context, history, input_stream, narration, output, persona, silence, types};
pub use services::control;
#[cfg(feature = "dev-console")]
pub use services::endpoint;

pub mod proto {
    tonic::include_proto!("kaguya.v1");
}
