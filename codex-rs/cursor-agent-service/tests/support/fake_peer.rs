use codex_cursor_agent_service::proto::AgentClientMessage;
use codex_cursor_agent_service::proto::AgentServerMessage;
use codex_cursor_agent_service::proto::agent_service_server::AgentService;
use codex_cursor_agent_service::proto::agent_service_server::AgentServiceServer;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::Request;
use tonic::Response;
use tonic::Status;
use tonic::Streaming;
use tonic::transport::Server;

pub struct FakePeer {
    endpoint: String,
    runs: mpsc::Receiver<FakeRun>,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<Result<(), tonic::transport::Error>>,
}

impl FakePeer {
    pub async fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (run_tx, runs) = mpsc::channel(4);
        let (shutdown, shutdown_rx) = oneshot::channel();
        let service = FakeAgentService { run_tx };
        let task = tokio::spawn(
            Server::builder()
                .add_service(AgentServiceServer::new(service))
                .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                    let _ = shutdown_rx.await;
                }),
        );

        Self {
            endpoint: format!("http://{address}"),
            runs,
            shutdown: Some(shutdown),
            task,
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub async fn next_run(&mut self) -> FakeRun {
        self.runs.recv().await.expect("client did not open a Run")
    }

    pub async fn shutdown(mut self) {
        self.shutdown.take().unwrap().send(()).unwrap();
        self.task.await.unwrap().unwrap();
    }
}

pub struct FakeRun {
    inbound: Streaming<AgentClientMessage>,
    outbound: mpsc::Sender<Result<AgentServerMessage, Status>>,
}

impl FakeRun {
    pub async fn next_client_message(&mut self) -> AgentClientMessage {
        self.inbound
            .message()
            .await
            .unwrap()
            .expect("client closed the Run")
    }

    pub async fn send(&self, message: AgentServerMessage) {
        self.outbound.send(Ok(message)).await.unwrap();
    }

    pub fn close(self) {}
}

#[derive(Clone)]
struct FakeAgentService {
    run_tx: mpsc::Sender<FakeRun>,
}

#[tonic::async_trait]
impl AgentService for FakeAgentService {
    type RunStream = ReceiverStream<Result<AgentServerMessage, Status>>;

    async fn run(
        &self,
        request: Request<Streaming<AgentClientMessage>>,
    ) -> Result<Response<Self::RunStream>, Status> {
        let (outbound, outbound_rx) = mpsc::channel(16);
        self.run_tx
            .send(FakeRun {
                inbound: request.into_inner(),
                outbound,
            })
            .await
            .map_err(|_| Status::unavailable("fake peer receiver closed"))?;
        Ok(Response::new(ReceiverStream::new(outbound_rx)))
    }
}
