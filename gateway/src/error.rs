use thiserror::Error;

#[derive(Error, Debug)]
pub enum GatewayError {
    #[error("channel closed: {0}")]
    ChannelClosed(String),
    #[error("file error: {0}")]
    File(#[from] std::io::Error),
    #[error("config error: {0}")]
    Config(String),
    #[error("gRPC: {0}")]
    Grpc(#[from] tonic::Status),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}