use thiserror::Error;

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("gRPC error: {0}")]
    Grpc(#[from] tonic::Status),
    #[error("transport error: {0}")]
    Transport(#[from] tonic::transport::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("config error: {0}")]
    Config(String),
    #[error("{0}")]
    Other(String),
}