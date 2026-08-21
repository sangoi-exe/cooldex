use super::*;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use pretty_assertions::assert_eq;

const CURRENT_TURN_ID: &str = "turn-current";

fn append_tail(
    mut new_history: Vec<ResponseItem>,
    prompt_input: Vec<ResponseItem>,
) -> Vec<ResponseItem> {
    append_tail_with_budget(
        &mut new_history,
        &prompt_input,
        CURRENT_TURN_ID,
        /*budget_tokens*/ 1_000,
    );
    new_history
}

fn stamped(mut item: ResponseItem, turn_id: &str) -> ResponseItem {
    item.set_turn_id_if_missing(turn_id);
    item
}

fn unstamped(mut item: ResponseItem) -> ResponseItem {
    item.clear_internal_chat_message_metadata_passthrough();
    item
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

fn compaction_item() -> ResponseItem {
    ResponseItem::Compaction {
        id: None,
        encrypted_content: "compact".to_string(),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn function_call(call_id: &str) -> ResponseItem {
    stamped(
        ResponseItem::FunctionCall {
            id: None,
            name: "shell".to_string(),
            namespace: None,
            arguments: "{}".to_string(),
            encrypted_function_args: None,
            call_id: call_id.to_string(),
            internal_chat_message_metadata_passthrough: None,
        },
        CURRENT_TURN_ID,
    )
}

fn function_output(call_id: &str, output: &str) -> ResponseItem {
    stamped(
        ResponseItem::FunctionCallOutput {
            id: None,
            call_id: Some(call_id.to_string()),
            name: Some("shell".to_string()),
            namespace: None,
            output: FunctionCallOutputPayload::from_text(output.to_string()),
            internal_chat_message_metadata_passthrough: None,
        },
        CURRENT_TURN_ID,
    )
}

#[test]
fn appends_complete_parallel_batch_in_original_order() {
    let user = user_message("continue");
    let compaction = compaction_item();
    let first_call = function_call("call-1");
    let second_call = function_call("call-2");
    let second_output = function_output("call-2", "second");
    let first_output = function_output("call-1", "first");

    let history = append_tail(
        vec![user.clone(), compaction.clone()],
        vec![
            user.clone(),
            first_call.clone(),
            second_call.clone(),
            second_output.clone(),
            first_output.clone(),
        ],
    );

    assert_eq!(
        history,
        vec![
            user,
            compaction,
            first_call,
            second_call,
            second_output,
            first_output,
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_entry_omits_unprovenanced_trailing_batch() {
    let (_session, turn_context) = crate::session::tests::make_session_and_context().await;
    let user = user_message("continue");
    let compaction = compaction_item();
    let call = unstamped(function_call("call-1"));
    let output = unstamped(function_output("call-1", "ok"));

    let mut history = vec![user.clone(), compaction.clone()];
    append_remote_v2_mid_turn_continuity_tail(
        &mut history,
        &[user.clone(), call, output],
        &turn_context,
    );

    assert_eq!(history, vec![user, compaction]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_entry_accepts_explicit_current_turn_provenance() {
    let (_session, turn_context) = crate::session::tests::make_session_and_context().await;
    let current_turn_id = turn_context.sub_id.clone();
    let user = user_message("continue");
    let compaction = compaction_item();
    let call = stamped(unstamped(function_call("call-1")), current_turn_id.as_str());
    let output = stamped(
        unstamped(function_output("call-1", "ok")),
        current_turn_id.as_str(),
    );

    let mut history = vec![user.clone(), compaction.clone()];
    append_remote_v2_mid_turn_continuity_tail(
        &mut history,
        &[user.clone(), call.clone(), output.clone()],
        &turn_context,
    );

    assert_eq!(history, vec![user, compaction, call, output]);
}

#[test]
fn omits_incomplete_or_asymmetric_batch() {
    let user = user_message("continue");
    let compaction = compaction_item();

    let history = append_tail(
        vec![user.clone(), compaction.clone()],
        vec![
            user.clone(),
            function_call("call-1"),
            function_call("call-2"),
            function_output("call-1", "only one output"),
        ],
    );

    assert_eq!(history, vec![user, compaction]);
}

#[test]
fn omits_batch_from_another_turn() {
    let user = user_message("continue");
    let compaction = compaction_item();
    let call = stamped(unstamped(function_call("call-1")), "turn-other");
    let output = stamped(unstamped(function_output("call-1", "ok")), "turn-other");

    let history = append_tail(
        vec![user.clone(), compaction.clone()],
        vec![user.clone(), call, output],
    );

    assert_eq!(history, vec![user, compaction]);
}

#[test]
fn truncates_output_without_orphaning_the_call() {
    let user = user_message("continue");
    let compaction = compaction_item();
    let call = function_call("call-1");

    let mut history = vec![user.clone(), compaction];
    append_tail_with_budget(
        &mut history,
        &[
            user,
            call.clone(),
            function_output("call-1", &"x".repeat(1_000)),
        ],
        CURRENT_TURN_ID,
        /*budget_tokens*/ 220,
    );

    let [_, _, appended_call, appended_output] = history.as_slice() else {
        panic!("expected call/output tail");
    };
    assert_eq!(appended_call, &call);
    let ResponseItem::FunctionCallOutput {
        call_id, output, ..
    } = appended_output
    else {
        panic!("expected function output");
    };
    assert_eq!(call_id.as_deref(), Some("call-1"));
    assert!(
        output
            .text_content()
            .expect("output should stay text")
            .contains("tokens truncated")
    );
}
