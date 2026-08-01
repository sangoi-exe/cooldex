use super::SameStreamTools;
use crate::stream_events_utils::InFlightFuture;
use codex_protocol::error::CodexErr;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseInputItem;
use pretty_assertions::assert_eq;
use std::time::Duration;

#[tokio::test]
async fn observes_completion_order_and_releases_history_in_emission_order() {
    let mut tools = SameStreamTools::new();
    tools.push(delayed_result("first", Duration::from_millis(40)));
    tools.push(delayed_result("second", Duration::from_millis(1)));

    let second = tools
        .next_completed()
        .await
        .expect("second tool should complete first");
    let (second_sequence, second_result) = second.into_parts();
    assert_eq!(second_sequence, 1);
    let second_result = second_result.expect("second tool should succeed");
    assert_eq!(call_id(&second_result), "second");
    assert_eq!(
        tools
            .record_sent(second_sequence, second_result)
            .expect("second completion should buffer"),
        Vec::<ResponseInputItem>::new()
    );

    let first = tools
        .next_completed()
        .await
        .expect("first tool should eventually complete");
    let (first_sequence, first_result) = first.into_parts();
    assert_eq!(first_sequence, 0);
    let first_result = first_result.expect("first tool should succeed");
    let ready = tools
        .record_sent(first_sequence, first_result)
        .expect("complete prefix should release");
    assert_eq!(
        ready.iter().map(call_id).collect::<Vec<_>>(),
        vec!["first", "second"]
    );
    assert!(tools.is_empty());
}

#[tokio::test]
async fn surfaces_a_tool_failure_without_fabricating_a_history_result() {
    let mut tools = SameStreamTools::new();
    tools.push(Box::pin(async {
        Err(CodexErr::Fatal("tool failed".to_string()))
    }));

    let completion = tools
        .next_completed()
        .await
        .expect("failed tool should still complete");
    let (sequence, result) = completion.into_parts();
    assert_eq!(sequence, 0);
    assert!(matches!(result, Err(CodexErr::Fatal(message)) if message == "tool failed"));
    assert!(tools.is_empty());
}

fn delayed_result(call_id: &str, delay: Duration) -> InFlightFuture<'static> {
    let call_id = call_id.to_string();
    Box::pin(async move {
        tokio::time::sleep(delay).await;
        Ok(ResponseInputItem::FunctionCallOutput {
            call_id,
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("done".to_string()),
                success: Some(true),
            },
        })
    })
}

fn call_id(item: &ResponseInputItem) -> &str {
    match item {
        ResponseInputItem::FunctionCallOutput { call_id, .. }
        | ResponseInputItem::McpToolCallOutput { call_id, .. }
        | ResponseInputItem::CustomToolCallOutput { call_id, .. }
        | ResponseInputItem::ToolSearchOutput { call_id, .. } => call_id,
        ResponseInputItem::Message { .. } => panic!("expected tool result"),
    }
}
