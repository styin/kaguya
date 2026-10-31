//! Kaguya wire contract shared by Gateway and Supervisor.
//!
//! Holds only what both processes speak: the generated protobuf code for
//! `proto/kaguya/v1/kaguya.proto` and the validated instance/session scope.
//! Configuration, helpers, and domain logic belong to the components.

pub mod session_scope;

pub mod proto {
    tonic::include_proto!("kaguya.v1");
}
