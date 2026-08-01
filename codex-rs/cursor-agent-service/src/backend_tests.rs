#![cfg(target_os = "linux")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::proto::AgentServerMessage;
use crate::proto::InteractionUpdate;
use crate::proto::TurnEndedUpdate;
use crate::proto::agent_client_message;
use crate::proto::agent_server_message;
use crate::proto::interaction_update;
use crate::test_support::DashboardReply;
use crate::test_support::FakeCursorServices;
use crate::test_support::RunReply;
use codex_api::ResponseEvent;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use tempfile::TempDir;
use tonic::Code;

const EXPECTED_USER_ID: u64 = 390_777_501;
const EXPECTED_TEAM_ID: u64 = 12_565_657;

#[tokio::test]
async fn reopens_once_after_a_pre_created_authenticated_failure() {
    let temp_dir = TempDir::new().unwrap();
    let credential_store_path = temp_dir.path().join("auth.json");
    write_store(&credential_store_path);
    let mut services = FakeCursorServices::spawn(
        vec![
            DashboardReply::Identity {
                user_id: EXPECTED_USER_ID as i32,
                team_id: Some(EXPECTED_TEAM_ID as i32),
            },
            DashboardReply::Identity {
                user_id: EXPECTED_USER_ID as i32,
                team_id: Some(EXPECTED_TEAM_ID as i32),
            },
        ],
        vec![RunReply::Error(Code::Unauthenticated), RunReply::Accept],
    )
    .await;
    let backend = CursorAgentServiceBackend::new_for_test(
        backend_config(),
        credential_store_path,
        services.endpoint().to_string(),
        services.endpoint().to_string(),
    );
    let input = vec![user_message("Use the corporate Cursor model")];
    let mut session = backend
        .start_sampling(
            CursorSamplingRequest {
                conversation_id: "conversation-local",
                model_id: "composer-2.5",
                model_display_name: "Composer 2.5",
                base_instructions: "exact Cooldex instructions",
                input: &input,
                tools: &[],
                current_message_id: "message-local",
                synthesized_user_message: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let first_identity = services.next_dashboard_request().await;
    let first_run = services.next_run_request().await;
    let second_identity = services.next_dashboard_request().await;
    let second_run = services.next_run_request().await;
    for metadata in [first_identity, first_run, second_identity, second_run] {
        assert_eq!(
            metadata.get("authorization").unwrap().to_str().unwrap(),
            "Bearer fake-access-token"
        );
    }
    assert!(matches!(
        session.next_event().await.unwrap(),
        Ok(ResponseEvent::Created)
    ));
    let mut run = services.next_run().await;
    assert!(matches!(
        run.next_client_message().await.message,
        Some(agent_client_message::Message::RunRequest(_))
    ));
    run.send(turn_ended()).await;
    assert!(matches!(
        session.next_event().await.unwrap().unwrap(),
        ResponseEvent::Completed { .. }
    ));
    services.shutdown().await;
}

#[tokio::test]
async fn identity_mismatch_fails_before_agent_service_run() {
    let temp_dir = TempDir::new().unwrap();
    let credential_store_path = temp_dir.path().join("auth.json");
    write_store(&credential_store_path);
    let mut services = FakeCursorServices::spawn(
        vec![DashboardReply::Identity {
            user_id: EXPECTED_USER_ID as i32 + 1,
            team_id: Some(EXPECTED_TEAM_ID as i32),
        }],
        Vec::new(),
    )
    .await;
    let backend = CursorAgentServiceBackend::new_for_test(
        backend_config(),
        credential_store_path,
        services.endpoint().to_string(),
        services.endpoint().to_string(),
    );
    let input = vec![user_message("Do not cross accounts")];

    let error = match backend
        .start_sampling(
            CursorSamplingRequest {
                conversation_id: "conversation-local",
                model_id: "composer-2.5",
                model_display_name: "Composer 2.5",
                base_instructions: "exact Cooldex instructions",
                input: &input,
                tools: &[],
                current_message_id: "message-local",
                synthesized_user_message: None,
            },
            CancellationToken::new(),
        )
        .await
    {
        Ok(_) => panic!("identity mismatch unexpectedly opened a sampling session"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        CursorAgentServiceBackendError::Identity(CursorIdentityError::UserMismatch { .. })
    ));
    let _ = services.next_dashboard_request().await;
    services.shutdown().await;
}

#[tokio::test]
async fn rejects_service_origin_drift_before_opening_credentials() {
    let backend = CursorAgentServiceBackend::new(CursorAgentServiceBackendConfig {
        expected_service_origin: "https://unexpected.cursor.example".to_string(),
        ..backend_config()
    });
    let input = vec![user_message("Fail before auth")];

    let error = match backend
        .start_sampling(
            CursorSamplingRequest {
                conversation_id: "conversation-local",
                model_id: "composer-2.5",
                model_display_name: "Composer 2.5",
                base_instructions: "exact Cooldex instructions",
                input: &input,
                tools: &[],
                current_message_id: "message-local",
                synthesized_user_message: None,
            },
            CancellationToken::new(),
        )
        .await
    {
        Ok(_) => panic!("service-origin drift unexpectedly opened a sampling session"),
        Err(error) => error,
    };

    assert_eq!(
        error.to_string(),
        "Cursor AgentService origin drift: expected https://agentn.global.api5.cursor.sh, configured https://unexpected.cursor.example"
    );
}

fn backend_config() -> CursorAgentServiceBackendConfig {
    CursorAgentServiceBackendConfig {
        expected_user_id: EXPECTED_USER_ID,
        expected_team_id: EXPECTED_TEAM_ID,
        expected_service_origin: CURSOR_AGENT_SERVICE_ORIGIN.to_string(),
        context_window_tokens: 65_536,
        effective_context_window_percent: 75,
        max_pending_tool_actions: 8,
    }
}

fn user_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn turn_ended() -> AgentServerMessage {
    AgentServerMessage {
        message: Some(agent_server_message::Message::InteractionUpdate(
            InteractionUpdate {
                message: Some(interaction_update::Message::TurnEnded(TurnEndedUpdate {
                    input_tokens: Some(10),
                    output_tokens: Some(2),
                    cache_read_tokens: Some(0),
                    cache_write_tokens: Some(0),
                    reasoning_tokens: Some(0),
                })),
            },
        )),
    }
}

fn write_store(path: &std::path::Path) {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(
        br#"{"accessToken":"fake-access-token","refreshToken":"fake-refresh-token"}"#,
    )
    .unwrap();
}
