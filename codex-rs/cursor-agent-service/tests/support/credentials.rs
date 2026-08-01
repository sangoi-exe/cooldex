use codex_cursor_agent_service::AgentServiceRun;
use codex_cursor_agent_service::AgentServiceTransport;
use codex_cursor_agent_service::AgentServiceTransportError;
use codex_cursor_agent_service::CursorCredentials;
use codex_cursor_agent_service::proto::AgentRunRequest;

pub const ACCESS_TOKEN: &str = "fake-access-token";

pub fn credentials() -> CursorCredentials {
    serde_json::from_str(
        r#"{"accessToken":"fake-access-token","refreshToken":"fake-refresh-token"}"#,
    )
    .expect("test Cursor credentials should deserialize")
}

pub async fn start_run(
    transport: &mut AgentServiceTransport,
    request: AgentRunRequest,
) -> Result<AgentServiceRun, AgentServiceTransportError> {
    transport.start_run(request, &credentials()).await
}
