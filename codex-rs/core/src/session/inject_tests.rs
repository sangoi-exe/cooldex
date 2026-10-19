use crate::compact_handoff::PreCompactHandoffSettings;
use crate::compact_handoff::PreCompactHandoffSource;
use crate::session::step_context::StepContext;
use crate::session::tests::make_session_and_context_with_rx;
use codex_features::Feature;
use codex_history::CodexHarnessMetadata;
use codex_history::ResponseItemEnvelope;
use codex_history::RolloutItem;
use codex_protocol::ResponseItemId;
use codex_protocol::models::ConfigurationReasoning;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::EventMsg;
use pretty_assertions::assert_eq;
use std::sync::Arc;

#[tokio::test]
async fn harness_authored_configuration_updates_preserve_metadata_and_resume() {
    let (session, turn_context, rx_event) = make_session_and_context_with_rx().await;
    assert!(!session.enabled(Feature::RetainClientDeveloperMessages));

    let mut expected = ResponseItemEnvelope {
        item: ResponseItem::ConfigurationUpdate {
            reasoning: ConfigurationReasoning {
                effort: ReasoningEffort::High,
            },
        },
        metadata: Some(CodexHarnessMetadata {
            harness_authored_configuration: true,
            ..Default::default()
        }),
    };
    session
        .record_annotated_conversation_items(
            &turn_context,
            turn_context.model_info(),
            vec![expected.clone()],
        )
        .await;

    expected.metadata.as_mut().unwrap().mcp_attribution = Some(
        session
            .services
            .executed_tool_calls
            .mcp_attribution_snapshot(),
    );

    let recorded = session.clone_history().await.into_annotated_items();
    assert_eq!(recorded, vec![expected.clone()]);
    let mut raw_items = Vec::new();
    while let Ok(event) = rx_event.try_recv() {
        if let EventMsg::RawResponseItem(event) = event.msg {
            raw_items.push(event.item);
        }
    }
    assert_eq!(raw_items, vec![expected.item]);

    let rollout_items = recorded
        .iter()
        .cloned()
        .map(RolloutItem::ResponseItem)
        .collect::<Vec<_>>();
    let reconstructed = session
        .reconstruct_history_from_rollout(&turn_context, &rollout_items)
        .await;
    assert_eq!(reconstructed.history, recorded);
}

#[tokio::test]
async fn compaction_original_observations_preserve_prepared_handoff_source() {
    let (session, turn_context, rx_event) = make_session_and_context_with_rx().await;
    session
        .record_conversation_items(
            &turn_context,
            turn_context.model_info(),
            &[ResponseItem::Message {
                status: None,
                encrypted_content: None,
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "admitted compaction source".to_string(),
                }],
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            }],
        )
        .await;
    while rx_event.try_recv().is_ok() {}

    let history_before = session.clone_history().await.into_annotated_items();
    let step_context = StepContext::for_test(Arc::clone(&turn_context));
    let settings = PreCompactHandoffSettings::from_step_context(&step_context);
    let source = PreCompactHandoffSource::from_snapshot(
        session
            .snapshot_pre_compact_handoff_input(&settings.model_info)
            .await
            .expect("capture admitted source"),
        settings.clone(),
    );
    let originals = vec![
        ResponseItem::Reasoning {
            id: Some(ResponseItemId::from_server("compact-reasoning".to_string())),
            summary: vec![ReasoningItemReasoningSummary::SummaryText {
                text: "Summarizing the conversation".to_string(),
            }],
            content: None,
            encrypted_content: None,
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::Message {
            status: None,
            encrypted_content: None,
            id: Some(ResponseItemId::from_server("compact-summary".to_string())),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "Summary of the admitted source".to_string(),
                annotations: None,
                logprobs: None,
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
    ];
    let original_ids = originals
        .iter()
        .map(|item| item.id().cloned())
        .collect::<Vec<_>>();
    session
        .record_annotated_conversation_items(
            &turn_context,
            turn_context.model_info(),
            originals
                .into_iter()
                .map(|item| ResponseItemEnvelope {
                    item,
                    metadata: Some(CodexHarnessMetadata {
                        compaction_output: true,
                        ..Default::default()
                    }),
                })
                .collect(),
        )
        .await;

    assert_eq!(
        session.clone_history().await.into_annotated_items(),
        history_before
    );
    assert!(
        source
            .is_current_for(&session, &settings)
            .await
            .expect("validate the unchanged admitted source")
    );
    let mut raw_items = Vec::new();
    while let Ok(event) = rx_event.try_recv() {
        if let EventMsg::RawResponseItem(event) = event.msg {
            raw_items.push(event.item);
        }
    }
    assert_eq!(
        raw_items
            .iter()
            .map(|item| item.id().cloned())
            .collect::<Vec<_>>(),
        original_ids
    );
    assert!(
        raw_items
            .iter()
            .all(|item| item.turn_id() == Some(turn_context.sub_id.as_str()))
    );
}
