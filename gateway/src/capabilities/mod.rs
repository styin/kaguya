//! Capability contracts — trait interfaces that the Gateway consumes.
//!
//! Each trait defines what the Gateway can ask of a capability, independent
//! of transport or implementation. Contracts live here; implementations live
//! in their respective modules (`rag/`, `clients/`, `tools.rs`) and add
//! `impl XxxCapability for ConcreteType` blocks.
//!
//! All capability traits include `fn readiness(&self) -> Readiness` using
//! the shared [`Readiness`](crate::lifecycle::Readiness) enum from the
//! lifecycle module.

pub mod rag;

pub use rag::RagCapability;
