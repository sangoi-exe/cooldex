# Guide Reapply — Recall Tool (Current Session Rollout)

> Canonical current post-auto-compact recall instruction: `recall` with empty args (`{}`); size cap comes from `recall_kbytes_limit`.

## Scope
Reapply exactly the `recall` implementation that was added as a **new tool** (without changing `manage_context.retrieve`) in Codex CLI core.

This guide includes:
- all file touchpoints
- exact commands
- exact patch/hunks (including native Codex CLI files)
- focused test commands and expected results

## Target Branch / Repo
- Repo: `/home/lucas/work/codex`
- Branch used in implementation: `reapply/accounts-20260209`

## Behavior Implemented
- Adds new function tool: `recall`.
- Source is fixed to **current session rollout JSON** via current session rollout recorder.
- Upper boundary is the latest `RolloutItem::Compacted` marker.
- Lower boundary is immediately after latest pre-compaction `EventMsg::UserMessage` (if present).
- Returns only pre-compaction:
  - reasoning text
  - assistant messages
- Excludes tool outputs/calls and user messages.
- Applies payload cap via `recall_kbytes_limit` (KiB) from `config.toml`.
- Fail-loud stop reasons:
  - `invalid_contract`
  - `unavailable`
  - `no_compaction_marker`
  - `rollout_read_error`
  - `rollout_parse_error`

## Files Added
- `codex-rs/core/src/tools/handlers/recall.rs`
- `docs/recall.md`

## Files Modified
- `.gitignore`
- `codex-rs/core/src/tools/handlers/mod.rs`
- `codex-rs/core/src/tools/spec.rs`
- `docs/manage_context.md`
- `docs/manage_context_model.md`

## Reapply Steps

1. Checkout branch and ensure clean-ish baseline (except intentional local deltas):
```bash
git checkout reapply/accounts-20260209
git status --short --branch
```

2. Apply the patch below exactly (copy into a patch file, then apply):
```bash
cat > /tmp/recall-reapply.patch <<'PATCH'
<PASTE_THE_FULL_PATCH_BLOCK_FROM_THIS_DOC>
PATCH

git apply /tmp/recall-reapply.patch
```

3. Format Rust code:
```bash
cd codex-rs
just fmt
```

4. Run focused tests (no cargo build):
```bash
cargo test -p codex-core --lib tools::handlers::recall -- --test-threads=1
cargo test -p codex-core --lib tools::spec -- --test-threads=1
cargo test -p codex-core --lib tools::handlers::manage_context -- --test-threads=1
```

5. Quick wiring checks:
```bash
rg -n "mod recall;|pub use recall::RecallHandler;" codex-rs/core/src/tools/handlers/mod.rs
rg -n "fn create_recall_tool|register_handler\(\"recall\"|recall_tool_contract_is_read_only|\"recall\",$" codex-rs/core/src/tools/spec.rs
rg -n "Pre-Compaction Recall|Related: pre-compaction recall|use `recall`" docs/recall.md docs/manage_context.md docs/manage_context_model.md
```

## Smoke Test (Runtime)
After launching Codex CLI with this code, call:
```json
{}
```
on tool `recall` and verify:
- response has `mode = "recall_pre_compact"`
- response has `source = "current_session_rollout"`
- `items[]` entries are only `kind = "reasoning" | "assistant_message"`
- no tool output payloads are returned

> Note: older `max_items` / `max_chars_per_item` references that appear below inside historical patch excerpts are obsolete and must not be used.

## Full Patch (Exact)

```diff
diff --git a/.gitignore b/.gitignore
index 8f39b7b1c..6ed3c5bd4 100644
--- a/.gitignore
+++ b/.gitignore
@@ -91,3 +91,5 @@ CHANGELOG.ignore.md
 __pycache__/
 *.pyc
 
+# Sangoi project docs / dev tooling (lives in separate repo; keep local checkout here)
+/.sangoi/
\ No newline at end of file
diff --git a/codex-rs/core/src/tools/handlers/mod.rs b/codex-rs/core/src/tools/handlers/mod.rs
index c0dd2d9f2..136a6a6c2 100644
--- a/codex-rs/core/src/tools/handlers/mod.rs
+++ b/codex-rs/core/src/tools/handlers/mod.rs
@@ -9,6 +9,7 @@ mod mcp;
 mod mcp_resource;
 mod plan;
 mod read_file;
+mod recall;
 mod request_user_input;
 mod search_tool_bm25;
 mod shell;
@@ -32,6 +33,7 @@ pub use mcp::McpHandler;
 pub use mcp_resource::McpResourceHandler;
 pub use plan::PlanHandler;
 pub use read_file::ReadFileHandler;
+pub use recall::RecallHandler;
 pub use request_user_input::RequestUserInputHandler;
 pub(crate) use request_user_input::request_user_input_tool_description;
 pub(crate) use search_tool_bm25::DEFAULT_LIMIT as SEARCH_TOOL_BM25_DEFAULT_LIMIT;
diff --git a/codex-rs/core/src/tools/spec.rs b/codex-rs/core/src/tools/spec.rs
index cd597db1f..9accfccde 100644
--- a/codex-rs/core/src/tools/spec.rs
+++ b/codex-rs/core/src/tools/spec.rs
@@ -1157,6 +1157,41 @@ fn create_manage_context_tool() -> ToolSpec {
     })
 }
 
+fn create_recall_tool() -> ToolSpec {
+    let properties = BTreeMap::from([
+        (
+            "max_items".to_string(),
+            JsonSchema::Number {
+                description: Some(
+                    "Optional maximum number of pre-compaction items to return. Defaults to 24."
+                        .to_string(),
+                ),
+            },
+        ),
+        (
+            "max_chars_per_item".to_string(),
+            JsonSchema::Number {
+                description: Some(
+                    "Optional maximum text length per returned item. Defaults to 1200.".to_string(),
+                ),
+            },
+        ),
+    ]);
+
+    ToolSpec::Function(ResponsesApiTool {
+        name: "recall".to_string(),
+        description:
+            "Recall recent reasoning and assistant messages from the current session rollout before the latest compaction marker (tool outputs excluded)."
+                .to_string(),
+        strict: false,
+        parameters: JsonSchema::Object {
+            properties,
+            required: None,
+            additional_properties: Some(false.into()),
+        },
+    })
+}
+
 fn create_list_mcp_resources_tool() -> ToolSpec {
     let properties = BTreeMap::from([
         (
@@ -1472,6 +1507,7 @@ pub(crate) fn build_specs(
     use crate::tools::handlers::McpResourceHandler;
     use crate::tools::handlers::PlanHandler;
     use crate::tools::handlers::ReadFileHandler;
+    use crate::tools::handlers::RecallHandler;
     use crate::tools::handlers::RequestUserInputHandler;
     use crate::tools::handlers::SearchToolBm25Handler;
     use crate::tools::handlers::ShellCommandHandler;
@@ -1490,6 +1526,7 @@ pub(crate) fn build_specs(
     let dynamic_tool_handler = Arc::new(DynamicToolHandler);
     let view_image_handler = Arc::new(ViewImageHandler);
     let manage_context_handler = Arc::new(ManageContextHandler);
+    let recall_handler = Arc::new(RecallHandler);
     let mcp_handler = Arc::new(McpHandler);
     let mcp_resource_handler = Arc::new(McpResourceHandler);
     let shell_command_handler = Arc::new(ShellCommandHandler);
@@ -1630,6 +1667,8 @@ pub(crate) fn build_specs(
     builder.register_handler("view_image", view_image_handler);
     builder.push_spec(create_manage_context_tool());
     builder.register_handler("manage_context", manage_context_handler);
+    builder.push_spec(create_recall_tool());
+    builder.register_handler("recall", recall_handler);
 
     if config.collab_tools {
         let collab_handler = Arc::new(CollabHandler);
@@ -1790,6 +1829,31 @@ mod tests {
         );
     }
 
+    #[test]
+    fn recall_tool_contract_is_read_only() {
+        let tool = create_recall_tool();
+        let ToolSpec::Function(ResponsesApiTool { parameters, .. }) = tool else {
+            panic!("recall must be a function tool");
+        };
+        let JsonSchema::Object {
+            properties,
+            required,
+            additional_properties,
+        } = parameters
+        else {
+            panic!("recall parameters must be object schema");
+        };
+
+        assert!(properties.contains_key("max_items"));
+        assert!(properties.contains_key("max_chars_per_item"));
+        assert!(!properties.contains_key("rollout_path"));
+        assert!(required.is_none(), "recall must not require arguments");
+        assert_eq!(
+            additional_properties,
+            Some(AdditionalProperties::Boolean(false))
+        );
+    }
+
     fn tool_name(tool: &ToolSpec) -> &str {
         match tool {
             ToolSpec::Function(ResponsesApiTool { name, .. }) => name,
@@ -1933,6 +1997,7 @@ mod tests {
             },
             create_view_image_tool(),
             create_manage_context_tool(),
+            create_recall_tool(),
         ] {
             expected.insert(tool_name(&spec).to_string(), spec);
         }
@@ -2145,6 +2210,7 @@ mod tests {
                 "web_search",
                 "view_image",
                 "manage_context",
+                "recall",
             ],
         );
     }
@@ -2168,6 +2234,7 @@ mod tests {
                 "web_search",
                 "view_image",
                 "manage_context",
+                "recall",
             ],
         );
     }
@@ -2193,6 +2260,7 @@ mod tests {
                 "web_search",
                 "view_image",
                 "manage_context",
+                "recall",
             ],
         );
     }
@@ -2218,6 +2286,7 @@ mod tests {
                 "web_search",
                 "view_image",
                 "manage_context",
+                "recall",
             ],
         );
     }
@@ -2241,6 +2310,7 @@ mod tests {
                 "web_search",
                 "view_image",
                 "manage_context",
+                "recall",
             ],
         );
     }
@@ -2264,6 +2334,7 @@ mod tests {
                 "web_search",
                 "view_image",
                 "manage_context",
+                "recall",
             ],
         );
     }
@@ -2286,6 +2357,7 @@ mod tests {
                 "web_search",
                 "view_image",
                 "manage_context",
+                "recall",
             ],
         );
     }
@@ -2309,6 +2381,7 @@ mod tests {
                 "web_search",
                 "view_image",
                 "manage_context",
+                "recall",
             ],
         );
     }
@@ -2334,6 +2407,7 @@ mod tests {
                 "web_search",
                 "view_image",
                 "manage_context",
+                "recall",
             ],
         );
     }
diff --git a/docs/manage_context.md b/docs/manage_context.md
index 6d4c4a7c7..a5bcc7598 100644
--- a/docs/manage_context.md
+++ b/docs/manage_context.md
@@ -82,3 +82,9 @@ Response includes:
 - `invalid_contract`
 
 For model-facing guidance, see `docs/manage_context_model.md`.
+
+## Related: pre-compaction recall
+
+Use `recall` when you need a clean view of recent **pre-compaction** context from the current session rollout, limited to reasoning + assistant messages and excluding tool output.
+
+See `docs/recall.md` for the contract.
diff --git a/docs/manage_context_model.md b/docs/manage_context_model.md
index 997160c95..1ab6ec3bc 100644
--- a/docs/manage_context_model.md
+++ b/docs/manage_context_model.md
@@ -2,6 +2,8 @@
 
 Use `manage_context` to sanitize heavy context with deterministic `retrieve -> apply` cycles.
 
+If the goal is to inspect recent pre-compaction history (reasoning + assistant messages only), use `recall` instead of `manage_context`.
+
 ## Hard rules
 
 - Use only v2 fields.
diff --git a/codex-rs/core/src/tools/handlers/recall.rs b/codex-rs/core/src/tools/handlers/recall.rs
new file mode 100644
index 000000000..4a787e4d3
--- /dev/null
+++ b/codex-rs/core/src/tools/handlers/recall.rs
@@ -0,0 +1,573 @@
+use crate::codex::Session;
+use crate::function_tool::FunctionCallError;
+use crate::rollout::RolloutRecorder;
+use crate::tools::context::ToolInvocation;
+use crate::tools::context::ToolOutput;
+use crate::tools::context::ToolPayload;
+use crate::tools::registry::ToolHandler;
+use crate::tools::registry::ToolKind;
+use async_trait::async_trait;
+use codex_protocol::models::ContentItem;
+use codex_protocol::models::FunctionCallOutputBody;
+use codex_protocol::models::MessagePhase;
+use codex_protocol::models::ReasoningItemContent;
+use codex_protocol::models::ReasoningItemReasoningSummary;
+use codex_protocol::models::ResponseItem;
+use codex_protocol::protocol::RolloutItem;
+use serde::Deserialize;
+use serde::Serialize;
+use serde_json::json;
+
+pub struct RecallHandler;
+
+const DEFAULT_MAX_ITEMS: usize = 24;
+const MAX_MAX_ITEMS: usize = 200;
+const DEFAULT_MAX_CHARS_PER_ITEM: usize = 1200;
+const MAX_MAX_CHARS_PER_ITEM: usize = 16_000;
+
+fn default_max_items() -> usize {
+    DEFAULT_MAX_ITEMS
+}
+
+fn default_max_chars_per_item() -> usize {
+    DEFAULT_MAX_CHARS_PER_ITEM
+}
+
+#[derive(Debug, Deserialize)]
+#[serde(deny_unknown_fields)]
+struct RecallToolArgs {
+    #[serde(default = "default_max_items")]
+    max_items: usize,
+    #[serde(default = "default_max_chars_per_item")]
+    max_chars_per_item: usize,
+}
+
+#[derive(Debug, Clone, Copy)]
+enum StopReason {
+    InvalidContract,
+    Unavailable,
+    NoCompactionMarker,
+    RolloutReadError,
+    RolloutParseError,
+}
+
+impl StopReason {
+    fn as_str(self) -> &'static str {
+        match self {
+            Self::InvalidContract => "invalid_contract",
+            Self::Unavailable => "unavailable",
+            Self::NoCompactionMarker => "no_compaction_marker",
+            Self::RolloutReadError => "rollout_read_error",
+            Self::RolloutParseError => "rollout_parse_error",
+        }
+    }
+}
+
+#[derive(Debug, Clone, Serialize)]
+struct RecallItem {
+    kind: String,
+    rollout_index: usize,
+    text: String,
+    #[serde(skip_serializing_if = "Option::is_none")]
+    phase: Option<String>,
+}
+
+#[async_trait]
+impl ToolHandler for RecallHandler {
+    fn kind(&self) -> ToolKind {
+        ToolKind::Function
+    }
+
+    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
+        let ToolInvocation {
+            session, payload, ..
+        } = invocation;
+
+        let ToolPayload::Function { arguments } = payload else {
+            return Err(contract_error(
+                StopReason::InvalidContract,
+                "recall handler received unsupported payload",
+            ));
+        };
+
+        let args: RecallToolArgs = serde_json::from_str(&arguments).map_err(|error| {
+            contract_error(
+                StopReason::InvalidContract,
+                format!("failed to parse function arguments: {error}"),
+            )
+        })?;
+
+        let response = handle_recall(session.as_ref(), &args).await?;
+        Ok(ToolOutput::Function {
+            body: FunctionCallOutputBody::Text(
+                serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string()),
+            ),
+            success: Some(true),
+        })
+    }
+}
+
+async fn handle_recall(
+    session: &Session,
+    args: &RecallToolArgs,
+) -> Result<serde_json::Value, FunctionCallError> {
+    validate_args(args)?;
+    let rollout_recorder = current_rollout_recorder(session).await?;
+    rollout_recorder.flush().await.map_err(|error| {
+        contract_error(
+            StopReason::RolloutReadError,
+            format!("failed to flush current session rollout: {error}"),
+        )
+    })?;
+    let rollout_path = rollout_recorder.rollout_path().to_path_buf();
+    let (rollout_items, _thread_id, parse_errors) =
+        RolloutRecorder::load_rollout_items(rollout_path.as_path())
+            .await
+            .map_err(|error| {
+                contract_error(
+                    StopReason::RolloutReadError,
+                    format!("failed to read current session rollout: {error}"),
+                )
+            })?;
+    if parse_errors > 0 {
+        return Err(contract_error(
+            StopReason::RolloutParseError,
+            format!("current session rollout has {parse_errors} parse error(s)"),
+        ));
+    }
+    build_recall_payload(&rollout_items, args.max_items, args.max_chars_per_item)
+}
+
+fn validate_args(args: &RecallToolArgs) -> Result<(), FunctionCallError> {
+    if args.max_items == 0 || args.max_items > MAX_MAX_ITEMS {
+        return Err(contract_error(
+            StopReason::InvalidContract,
+            format!("recall.max_items must be between 1 and {MAX_MAX_ITEMS}"),
+        ));
+    }
+    if args.max_chars_per_item == 0 || args.max_chars_per_item > MAX_MAX_CHARS_PER_ITEM {
+        return Err(contract_error(
+            StopReason::InvalidContract,
+            format!("recall.max_chars_per_item must be between 1 and {MAX_MAX_CHARS_PER_ITEM}"),
+        ));
+    }
+    Ok(())
+}
+
+async fn current_rollout_recorder(session: &Session) -> Result<RolloutRecorder, FunctionCallError> {
+    let recorder = {
+        let guard = session.services.rollout.lock().await;
+        guard.clone()
+    };
+    recorder.ok_or_else(|| {
+        contract_error(
+            StopReason::Unavailable,
+            "recall requires an active current-session rollout recorder",
+        )
+    })
+}
+
+fn build_recall_payload(
+    rollout_items: &[RolloutItem],
+    max_items: usize,
+    max_chars_per_item: usize,
+) -> Result<serde_json::Value, FunctionCallError> {
+    let compacted_markers_seen = rollout_items
+        .iter()
+        .filter(|item| matches!(item, RolloutItem::Compacted(_)))
+        .count();
+    let Some(latest_compacted_index) = rollout_items
+        .iter()
+        .enumerate()
+        .rev()
+        .find_map(|(index, item)| matches!(item, RolloutItem::Compacted(_)).then_some(index))
+    else {
+        return Err(contract_error(
+            StopReason::NoCompactionMarker,
+            "current session rollout has no compacted marker",
+        ));
+    };
+
+    let mut matching_items =
+        collect_pre_compact_items(rollout_items, latest_compacted_index, max_chars_per_item);
+    let matching_pre_compact_items = matching_items.len();
+    if matching_items.len() > max_items {
+        let split_point = matching_items.len() - max_items;
+        matching_items = matching_items.split_off(split_point);
+    }
+    let returned_items = matching_items.len();
+
+    Ok(json!({
+        "mode": "recall_pre_compact",
+        "source": "current_session_rollout",
+        "boundary": {
+            "latest_compacted_index": latest_compacted_index,
+            "compacted_markers_seen": compacted_markers_seen,
+        },
+        "filters": {
+            "include_reasoning": true,
+            "include_assistant_messages": true,
+            "exclude_tool_output": true,
+        },
+        "counts": {
+            "matching_pre_compact_items": matching_pre_compact_items,
+            "returned_items": returned_items,
+            "max_items": max_items,
+        },
+        "items": matching_items,
+    }))
+}
+
+fn collect_pre_compact_items(
+    rollout_items: &[RolloutItem],
+    latest_compacted_index: usize,
+    max_chars_per_item: usize,
+) -> Vec<RecallItem> {
+    let mut output = Vec::new();
+    for (index, rollout_item) in rollout_items
+        .iter()
+        .enumerate()
+        .take(latest_compacted_index)
+    {
+        let RolloutItem::ResponseItem(response_item) = rollout_item else {
+            continue;
+        };
+        match response_item {
+            ResponseItem::Reasoning {
+                summary, content, ..
+            } => {
+                let text = reasoning_text(summary, content);
+                let trimmed = text.trim();
+                if trimmed.is_empty() {
+                    continue;
+                }
+                output.push(RecallItem {
+                    kind: "reasoning".to_string(),
+                    rollout_index: index,
+                    text: truncate_to_char_limit(trimmed, max_chars_per_item),
+                    phase: None,
+                });
+            }
+            ResponseItem::Message {
+                role,
+                content,
+                phase,
+                ..
+            } if role == "assistant" => {
+                let text = assistant_message_text(content);
+                let trimmed = text.trim();
+                if trimmed.is_empty() {
+                    continue;
+                }
+                output.push(RecallItem {
+                    kind: "assistant_message".to_string(),
+                    rollout_index: index,
+                    text: truncate_to_char_limit(trimmed, max_chars_per_item),
+                    phase: phase
+                        .as_ref()
+                        .map(|message_phase| phase_name(message_phase).to_string()),
+                });
+            }
+            _ => {}
+        }
+    }
+    output
+}
+
+fn reasoning_text(
+    summary: &[ReasoningItemReasoningSummary],
+    content: &Option<Vec<ReasoningItemContent>>,
+) -> String {
+    let mut segments: Vec<String> = summary
+        .iter()
+        .filter_map(|summary_item| match summary_item {
+            ReasoningItemReasoningSummary::SummaryText { text } => {
+                let trimmed = text.trim();
+                (!trimmed.is_empty()).then_some(trimmed.to_string())
+            }
+        })
+        .collect();
+
+    if segments.is_empty()
+        && let Some(content_items) = content
+    {
+        for content_item in content_items {
+            match content_item {
+                ReasoningItemContent::ReasoningText { text }
+                | ReasoningItemContent::Text { text } => {
+                    let trimmed = text.trim();
+                    if !trimmed.is_empty() {
+                        segments.push(trimmed.to_string());
+                    }
+                }
+            }
+        }
+    }
+
+    segments.join("\n")
+}
+
+fn assistant_message_text(content_items: &[ContentItem]) -> String {
+    content_items
+        .iter()
+        .filter_map(|content_item| match content_item {
+            ContentItem::OutputText { text } | ContentItem::InputText { text } => {
+                let trimmed = text.trim();
+                (!trimmed.is_empty()).then_some(trimmed.to_string())
+            }
+            ContentItem::InputImage { .. } => None,
+        })
+        .collect::<Vec<String>>()
+        .join("\n")
+}
+
+fn phase_name(phase: &MessagePhase) -> &'static str {
+    match phase {
+        MessagePhase::Commentary => "commentary",
+        MessagePhase::FinalAnswer => "final_answer",
+    }
+}
+
+fn truncate_to_char_limit(text: &str, max_chars: usize) -> String {
+    let mut char_indices = text.char_indices();
+    let Some((cutoff, _)) = char_indices.nth(max_chars) else {
+        return text.to_string();
+    };
+    format!("{}…", &text[..cutoff])
+}
+
+fn contract_error(reason: StopReason, message: impl Into<String>) -> FunctionCallError {
+    FunctionCallError::RespondToModel(
+        json!({
+            "stop_reason": reason.as_str(),
+            "message": message.into(),
+        })
+        .to_string(),
+    )
+}
+
+#[cfg(test)]
+mod tests {
+    use super::*;
+    use codex_protocol::models::FunctionCallOutputPayload;
+    use codex_protocol::models::ReasoningItemReasoningSummary::SummaryText;
+    use codex_protocol::protocol::CompactedItem;
+    use pretty_assertions::assert_eq;
+    use serde_json::Value;
+
+    fn assistant_message(text: &str, phase: Option<MessagePhase>) -> RolloutItem {
+        RolloutItem::ResponseItem(ResponseItem::Message {
+            id: None,
+            role: "assistant".to_string(),
+            content: vec![ContentItem::OutputText {
+                text: text.to_string(),
+            }],
+            end_turn: None,
+            phase,
+        })
+    }
+
+    fn user_message(text: &str) -> RolloutItem {
+        RolloutItem::ResponseItem(ResponseItem::Message {
+            id: None,
+            role: "user".to_string(),
+            content: vec![ContentItem::InputText {
+                text: text.to_string(),
+            }],
+            end_turn: None,
+            phase: None,
+        })
+    }
+
+    fn reasoning(summary_text: &str) -> RolloutItem {
+        RolloutItem::ResponseItem(ResponseItem::Reasoning {
+            id: "reasoning-item".to_string(),
+            summary: vec![SummaryText {
+                text: summary_text.to_string(),
+            }],
+            content: None,
+            encrypted_content: None,
+        })
+    }
+
+    fn tool_output(call_id: &str, output: &str) -> RolloutItem {
+        RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput {
+            call_id: call_id.to_string(),
+            output: FunctionCallOutputPayload::from_text(output.to_string()),
+        })
+    }
+
+    fn compacted_marker() -> RolloutItem {
+        RolloutItem::Compacted(CompactedItem {
+            message: "auto compacted".to_string(),
+            replacement_history: None,
+        })
+    }
+
+    fn parse_error_stop_reason(error: FunctionCallError) -> String {
+        let FunctionCallError::RespondToModel(raw) = error else {
+            panic!("expected RespondToModel error");
+        };
+        let payload: Value = serde_json::from_str(&raw).expect("structured error payload");
+        payload
+            .get("stop_reason")
+            .and_then(Value::as_str)
+            .expect("stop_reason")
+            .to_string()
+    }
+
+    #[test]
+    fn recall_requires_compaction_marker() {
+        let rollout_items = vec![assistant_message("before", None), reasoning("analysis")];
+
+        let error = build_recall_payload(&rollout_items, 10, 500)
+            .expect_err("must fail when no compaction marker is present");
+
+        assert_eq!(
+            parse_error_stop_reason(error),
+            StopReason::NoCompactionMarker.as_str()
+        );
+    }
+
+    #[test]
+    fn recall_filters_to_pre_compact_assistant_and_reasoning_only() {
+        let rollout_items = vec![
+            user_message("ignored"),
+            assistant_message("first assistant", Some(MessagePhase::Commentary)),
+            tool_output("call_1", "tool output should be ignored"),
+            reasoning("reasoning before compact"),
+            compacted_marker(),
+            assistant_message("after compact should not be included", None),
+            reasoning("after compact should not be included"),
+        ];
+
+        let payload = build_recall_payload(&rollout_items, 10, 500).expect("build recall payload");
+        let items = payload
+            .get("items")
+            .and_then(Value::as_array)
+            .expect("items array");
+
+        assert_eq!(items.len(), 2);
+        assert_eq!(
+            items[0].get("kind").and_then(Value::as_str),
+            Some("assistant_message")
+        );
+        assert_eq!(
+            items[1].get("kind").and_then(Value::as_str),
+            Some("reasoning")
+        );
+        assert_eq!(
+            payload.pointer("/counts/matching_pre_compact_items"),
+            Some(&json!(2))
+        );
+        assert_eq!(payload.pointer("/counts/returned_items"), Some(&json!(2)));
+    }
+
+    #[test]
+    fn recall_returns_only_latest_max_items() {
+        let rollout_items = vec![
+            assistant_message("assistant 1", None),
+            reasoning("reasoning 1"),
+            assistant_message("assistant 2", Some(MessagePhase::FinalAnswer)),
+            compacted_marker(),
+        ];
+
+        let payload = build_recall_payload(&rollout_items, 2, 500).expect("build recall payload");
+        let items = payload
+            .get("items")
+            .and_then(Value::as_array)
+            .expect("items array");
+
+        assert_eq!(items.len(), 2);
+        assert_eq!(
+            items[0].get("rollout_index").and_then(Value::as_u64),
+            Some(1)
+        );
+        assert_eq!(
+            items[1].get("rollout_index").and_then(Value::as_u64),
+            Some(2)
+        );
+    }
+
+    #[test]
+    fn recall_truncates_text_by_char_limit() {
+        let rollout_items = vec![assistant_message("abcdefghijk", None), compacted_marker()];
+
+        let payload = build_recall_payload(&rollout_items, 10, 5).expect("build recall payload");
+        let items = payload
+            .get("items")
+            .and_then(Value::as_array)
+            .expect("items array");
+        let text = items[0].get("text").and_then(Value::as_str).expect("text");
+        assert_eq!(text, "abcde…");
+    }
+
+    #[test]
+    fn recall_uses_reasoning_content_when_summary_is_missing() {
+        let rollout_items = vec![
+            RolloutItem::ResponseItem(ResponseItem::Reasoning {
+                id: "reasoning-item".to_string(),
+                summary: Vec::new(),
+                content: Some(vec![ReasoningItemContent::ReasoningText {
+                    text: "fallback reasoning text".to_string(),
+                }]),
+                encrypted_content: None,
+            }),
+            compacted_marker(),
+        ];
+
+        let payload = build_recall_payload(&rollout_items, 10, 500).expect("build recall payload");
+        let items = payload
+            .get("items")
+            .and_then(Value::as_array)
+            .expect("items array");
+        let text = items[0].get("text").and_then(Value::as_str).expect("text");
+        assert_eq!(text, "fallback reasoning text");
+    }
+
+    #[test]
+    fn recall_rejects_invalid_argument_bounds() {
+        for args in [
+            RecallToolArgs {
+                max_items: 0,
+                max_chars_per_item: DEFAULT_MAX_CHARS_PER_ITEM,
+            },
+            RecallToolArgs {
+                max_items: MAX_MAX_ITEMS + 1,
+                max_chars_per_item: DEFAULT_MAX_CHARS_PER_ITEM,
+            },
+            RecallToolArgs {
+                max_items: DEFAULT_MAX_ITEMS,
+                max_chars_per_item: 0,
+            },
+            RecallToolArgs {
+                max_items: DEFAULT_MAX_ITEMS,
+                max_chars_per_item: MAX_MAX_CHARS_PER_ITEM + 1,
+            },
+        ] {
+            let error = validate_args(&args).expect_err("invalid args must fail");
+            assert_eq!(
+                parse_error_stop_reason(error),
+                StopReason::InvalidContract.as_str()
+            );
+        }
+    }
+
+    #[tokio::test]
+    async fn recall_fails_when_session_rollout_is_unavailable() {
+        let (session, _turn) = crate::codex::make_session_and_context().await;
+        let args = RecallToolArgs {
+            max_items: DEFAULT_MAX_ITEMS,
+            max_chars_per_item: DEFAULT_MAX_CHARS_PER_ITEM,
+        };
+
+        let error = handle_recall(&session, &args)
+            .await
+            .expect_err("must fail without rollout recorder");
+
+        assert_eq!(
+            parse_error_stop_reason(error),
+            StopReason::Unavailable.as_str()
+        );
+    }
+}
diff --git a/docs/recall.md b/docs/recall.md
new file mode 100644
index 000000000..e819a9d5e
--- /dev/null
+++ b/docs/recall.md
@@ -0,0 +1,43 @@
+## Pre-Compaction Recall (`recall`)
+
+`recall` is a read-only tool for recovering recent context from the **current session rollout JSON** before the latest compaction marker.
+
+It intentionally returns only:
+- assistant messages
+- reasoning text (summary first; falls back to reasoning content when summary is absent)
+
+It intentionally excludes:
+- tool calls
+- tool outputs
+- user messages
+
+### Contract
+
+Request fields:
+- `max_items` (optional, default `24`): maximum number of matching pre-compaction items to return.
+- `max_chars_per_item` (optional, default `1200`): per-item text truncation limit.
+
+Unknown fields are rejected.
+
+### Behavior
+
+- Source is fixed to the current session rollout recorder path (no path argument).
+- Uses the latest `RolloutItem::Compacted` marker as the boundary.
+- Returns the most recent `max_items` from pre-compaction matches.
+- If there is no compaction marker, the tool fails with `stop_reason = "no_compaction_marker"`.
+
+### Example
+
+```json
+{"max_items":20}
+```
+
+Response shape (summary):
+- `mode = "recall_pre_compact"`
+- `boundary.latest_compacted_index`
+- `counts`
+- `items[]` with:
+  - `kind = "assistant_message" | "reasoning"`
+  - `rollout_index`
+  - `text`
+  - `phase` (assistant message only, when available)
```


## Current Warning Invariant
- After auto-compaction, the warning must require `recall` before any other action.
- The warning must not include tool arguments; payload size is controlled by `recall_kbytes_limit`.

## Focused Validation (Current Contract)
```bash
cargo test -p codex-core --lib tools::handlers::recall -- --test-threads=1
cargo test -p codex-core --lib recall_kbytes_limit -- --test-threads=1
cargo test -p codex-core --test all compact_remote::remote_auto_compact_warning_is_emitted_after_each_compaction -- --test-threads=1
cargo test -p codex-core --test all compact::local_auto_compact_does_not_emit_remote_warning -- --test-threads=1
```
