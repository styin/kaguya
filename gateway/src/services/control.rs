//! RouterControlService gRPC server — Accepts control signals from external systems (e.g. UI) and forwards them to P0.

use tokio::sync::mpsc;
use tonic::{Request, Response, Status};
use tracing::info;

use crate::proto;
use crate::proto::router_control_service_server::RouterControlService;
use crate::proto::SessionRef;
use crate::types::ControlSignal;

pub struct ControlServiceImpl {
    control_tx: mpsc::Sender<ControlSignal>,
    session: SessionRef,
}

impl ControlServiceImpl {
    pub fn new(control_tx: mpsc::Sender<ControlSignal>, session: SessionRef) -> Self {
        Self {
            control_tx,
            session,
        }
    }
}

#[tonic::async_trait]
impl RouterControlService for ControlServiceImpl {
    async fn send_control(
        &self,
        request: Request<proto::ControlRequest>,
    ) -> Result<Response<proto::ControlAck>, Status> {
        let request = request.into_inner();
        crate::instance::validate_session_ref(request.session.as_ref())
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        if request.session.as_ref() != Some(&self.session) {
            return Err(Status::failed_precondition(
                "control request instance/session mismatch",
            ));
        }
        let signal = request
            .signal
            .ok_or_else(|| Status::invalid_argument("missing control signal"))?;

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

        self.control_tx
            .send(internal)
            .await
            .map_err(|_| Status::internal("control channel closed"))?;

        Ok(Response::new(proto::ControlAck {}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> SessionRef {
        SessionRef {
            instance_id: Some(crate::proto::InstanceId {
                value: uuid::Uuid::new_v4().to_string(),
            }),
            session_id: Some(crate::proto::SessionId {
                value: uuid::Uuid::new_v4().to_string(),
            }),
        }
    }

    fn stop_request(scope: Option<proto::SessionRef>) -> proto::ControlRequest {
        proto::ControlRequest {
            session: scope,
            signal: Some(proto::ControlSignal {
                signal: Some(proto::control_signal::Signal::Stop(proto::StopSignal {})),
            }),
        }
    }

    #[tokio::test]
    async fn rejects_missing_invalid_or_mismatched_scope_before_p0_dispatch() {
        let (tx, mut rx) = mpsc::channel(4);
        let expected = session();
        let service = ControlServiceImpl::new(tx, expected.clone());
        let mut wrong_instance = expected.clone();
        wrong_instance.instance_id = session().instance_id;
        let mut wrong_session = expected.clone();
        wrong_session.session_id = session().session_id;
        let mut missing_id = expected.clone();
        missing_id.session_id = None;
        let mut equal_ids = expected.clone();
        equal_ids.session_id.as_mut().unwrap().value =
            expected.instance_id.as_ref().unwrap().value.clone();
        for (scope, code) in [
            (None, tonic::Code::InvalidArgument),
            (
                Some(proto::SessionRef::default()),
                tonic::Code::InvalidArgument,
            ),
            (Some(missing_id), tonic::Code::InvalidArgument),
            (Some(equal_ids), tonic::Code::InvalidArgument),
            (Some(wrong_instance), tonic::Code::FailedPrecondition),
            (Some(wrong_session), tonic::Code::FailedPrecondition),
        ] {
            let error = service
                .send_control(Request::new(stop_request(scope)))
                .await
                .unwrap_err();
            assert_eq!(error.code(), code);
            assert!(
                rx.try_recv().is_err(),
                "invalid control must never enter P0"
            );
        }
        let missing_signal = proto::ControlRequest {
            session: Some(expected.clone()),
            signal: None,
        };
        assert_eq!(
            service
                .send_control(Request::new(missing_signal))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::InvalidArgument
        );
        assert!(rx.try_recv().is_err());
        service
            .send_control(Request::new(stop_request(Some(expected.clone()))))
            .await
            .unwrap();
        assert!(matches!(rx.recv().await, Some(ControlSignal::Stop)));
    }
}
