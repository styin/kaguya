//! RouterControlService gRPC server — Accepts control signals from external systems (e.g. UI) and forwards them to P0.

use tokio::sync::mpsc;
use tonic::{Request, Response, Status};
use tracing::info;

use crate::proto;
use crate::proto::router_control_service_server::RouterControlService;
use crate::types::ControlSignal;

pub struct ControlServiceImpl {
    control_tx: mpsc::Sender<ControlSignal>,
}

impl ControlServiceImpl {
    pub fn new(control_tx: mpsc::Sender<ControlSignal>) -> Self {
        Self { control_tx }
    }
}

#[tonic::async_trait]
impl RouterControlService for ControlServiceImpl {
    async fn send_control(
        &self,
        request: Request<proto::ControlSignal>,
    ) -> Result<Response<proto::ControlAck>, Status> {
        let signal = request.into_inner();

        let internal = match signal.signal {
            Some(proto::control_signal::Signal::Stop(_)) => {
                info!("gRPC → P0: STOP");
                ControlSignal::Stop
            }
            Some(proto::control_signal::Signal::Approval(a)) => {
                info!("gRPC → P0: APPROVAL");
                ControlSignal::Approval { context: a.context }
            }
            Some(proto::control_signal::Signal::Shutdown(_)) => {
                info!("gRPC → P0: SHUTDOWN");
                ControlSignal::Shutdown
            }
            None => return Err(Status::invalid_argument("empty signal")),
        };

        self.control_tx.send(internal).await
            .map_err(|_| Status::internal("control channel closed"))?;

        Ok(Response::new(proto::ControlAck {}))
    }
}