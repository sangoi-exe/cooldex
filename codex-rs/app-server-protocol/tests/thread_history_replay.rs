use codex_app_server_protocol::CommandAction;
use codex_app_server_protocol::CommandExecutionSource;
use codex_app_server_protocol::CommandExecutionStatus;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::build_turns_from_rollout_items;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::protocol::TurnStartedEvent;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

// Merge-safety anchor: replayed shell/function-call rows must not look
// completed unless persisted rollout evidence still proves completion.

#[test]
fn replayed_shell_function_output_without_persisted_success_stays_in_progress() {
    let turns = build_turns_from_rollout_items(&[
        RolloutItem::EventMsg(EventMsg::TurnStarted(TurnStartedEvent {
            turn_id: "turn-a".into(),
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        })),
        RolloutItem::ResponseItem(ResponseItem::FunctionCall {
            id: None,
            name: "shell".into(),
            namespace: None,
            arguments: r#"{"command":["cat","runner.py"],"workdir":"/tmp/workspace"}"#.into(),
            call_id: "call-shell-1".into(),
        }),
        RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput {
            call_id: "call-shell-1".into(),
            output: FunctionCallOutputPayload::from_text("permission denied".into()),
        }),
        RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: "turn-a".into(),
            completed_at: None,
            duration_ms: None,
            last_agent_message: None,
        })),
    ]);

    assert_eq!(turns.len(), 1);
    assert_eq!(
        turns[0].items,
        vec![ThreadItem::CommandExecution {
            id: "call-shell-1".into(),
            command: "cat runner.py".into(),
            cwd: AbsolutePathBuf::from_absolute_path("/tmp/workspace").expect("absolute cwd"),
            process_id: None,
            source: CommandExecutionSource::Agent,
            status: CommandExecutionStatus::InProgress,
            command_actions: vec![CommandAction::Read {
                command: "cat runner.py".into(),
                name: "runner.py".into(),
                path: AbsolutePathBuf::from_absolute_path("/tmp/workspace/runner.py")
                    .expect("absolute read path"),
            }],
            aggregated_output: Some("permission denied".into()),
            exit_code: None,
            duration_ms: None,
        }]
    );
}
