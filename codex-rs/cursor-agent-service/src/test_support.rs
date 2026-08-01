#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::proto::AgentClientMessage;
use crate::proto::AgentServerMessage;
use crate::proto::agent_service_server::AgentService;
use crate::proto::agent_service_server::AgentServiceServer;
use crate::proto::dashboard::GetMeRequest;
use crate::proto::dashboard::GetMeResponse;
use crate::proto::dashboard::dashboard_service_server::DashboardService;
use crate::proto::dashboard::dashboard_service_server::DashboardServiceServer;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::Code;
use tonic::Request;
use tonic::Response;
use tonic::Status;
use tonic::Streaming;
use tonic::metadata::MetadataMap;
use tonic::transport::Server;

#[derive(Clone, Debug)]
pub(crate) enum DashboardReply {
    Identity { user_id: i32, team_id: Option<i32> },
    Error(Code),
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum RunReply {
    Accept,
    Error(Code),
}

pub(crate) struct FakeCursorServices {
    endpoint: String,
    dashboard_requests: mpsc::Receiver<MetadataMap>,
    run_requests: mpsc::Receiver<MetadataMap>,
    runs: mpsc::Receiver<FakeRun>,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<Result<(), tonic::transport::Error>>,
}

impl FakeCursorServices {
    pub(crate) async fn spawn(
        dashboard_replies: Vec<DashboardReply>,
        run_replies: Vec<RunReply>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (dashboard_request_tx, dashboard_requests) = mpsc::channel(8);
        let (run_request_tx, run_requests) = mpsc::channel(8);
        let (run_tx, runs) = mpsc::channel(8);
        let (shutdown, shutdown_rx) = oneshot::channel();
        let dashboard_service = FakeDashboardService {
            replies: Arc::new(Mutex::new(dashboard_replies.into())),
            requests: dashboard_request_tx,
        };
        let agent_service = FakeAgentService {
            replies: Arc::new(Mutex::new(run_replies.into())),
            requests: run_request_tx,
            runs: run_tx,
        };
        let task = tokio::spawn(
            Server::builder()
                .add_service(DashboardServiceServer::new(dashboard_service))
                .add_service(AgentServiceServer::new(agent_service))
                .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                    let _ = shutdown_rx.await;
                }),
        );
        Self {
            endpoint: format!("http://{address}"),
            dashboard_requests,
            run_requests,
            runs,
            shutdown: Some(shutdown),
            task,
        }
    }

    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub(crate) async fn next_dashboard_request(&mut self) -> MetadataMap {
        self.dashboard_requests
            .recv()
            .await
            .expect("client did not call DashboardService.GetMe")
    }

    pub(crate) async fn next_run_request(&mut self) -> MetadataMap {
        self.run_requests
            .recv()
            .await
            .expect("client did not call AgentService.Run")
    }

    pub(crate) async fn next_run(&mut self) -> FakeRun {
        self.runs
            .recv()
            .await
            .expect("client did not open an accepted AgentService.Run")
    }

    pub(crate) async fn shutdown(mut self) {
        self.shutdown.take().unwrap().send(()).unwrap();
        self.task.await.unwrap().unwrap();
    }
}

pub(crate) struct FakeRun {
    inbound: Streaming<AgentClientMessage>,
    outbound: mpsc::Sender<Result<AgentServerMessage, Status>>,
}

impl FakeRun {
    pub(crate) async fn next_client_message(&mut self) -> AgentClientMessage {
        self.inbound
            .message()
            .await
            .unwrap()
            .expect("client closed AgentService.Run")
    }

    pub(crate) async fn send(&self, message: AgentServerMessage) {
        self.outbound.send(Ok(message)).await.unwrap();
    }
}

#[derive(Clone)]
struct FakeDashboardService {
    replies: Arc<Mutex<VecDeque<DashboardReply>>>,
    requests: mpsc::Sender<MetadataMap>,
}

#[tonic::async_trait]
impl DashboardService for FakeDashboardService {
    async fn get_me(
        &self,
        request: Request<GetMeRequest>,
    ) -> Result<Response<GetMeResponse>, Status> {
        self.requests
            .send(request.metadata().clone())
            .await
            .map_err(|_| Status::unavailable("fake dashboard receiver closed"))?;
        match self
            .replies
            .lock()
            .await
            .pop_front()
            .expect("missing fake dashboard reply")
        {
            DashboardReply::Identity { user_id, team_id } => {
                Ok(Response::new(GetMeResponse {
                    auth_id: "fake-auth-id".to_string(),
                    user_id,
                    email: None,
                    first_name: None,
                    last_name: None,
                    workos_id: None,
                    team_id,
                }))
            }
            DashboardReply::Error(code) => Err(Status::new(code, "fake dashboard error")),
        }
    }
}

#[derive(Clone)]
struct FakeAgentService {
    replies: Arc<Mutex<VecDeque<RunReply>>>,
    requests: mpsc::Sender<MetadataMap>,
    runs: mpsc::Sender<FakeRun>,
}

#[tonic::async_trait]
impl AgentService for FakeAgentService {
    type RunStream = ReceiverStream<Result<AgentServerMessage, Status>>;

    async fn run(
        &self,
        request: Request<Streaming<AgentClientMessage>>,
    ) -> Result<Response<Self::RunStream>, Status> {
        self.requests
            .send(request.metadata().clone())
            .await
            .map_err(|_| Status::unavailable("fake Run request receiver closed"))?;
        match self
            .replies
            .lock()
            .await
            .pop_front()
            .expect("missing fake AgentService reply")
        {
            RunReply::Accept => {
                let (outbound, outbound_rx) = mpsc::channel(16);
                self.runs
                    .send(FakeRun {
                        inbound: request.into_inner(),
                        outbound,
                    })
                    .await
                    .map_err(|_| Status::unavailable("fake Run receiver closed"))?;
                Ok(Response::new(ReceiverStream::new(outbound_rx)))
            }
            RunReply::Error(code) => Err(Status::new(code, "fake AgentService error")),
        }
    }
}
