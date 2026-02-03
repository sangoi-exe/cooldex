# Referência do fix watchdog de eventos

Diff completo: release/0.59.0 (local) → upstream tag rust-v0.59.0  
Gerado em 2025-11-20 (UTC) para facilitar replays futuros do patch.

```diff
diff --git a/codex-rs/app-server-protocol/src/protocol/v2.rs b/codex-rs/app-server-protocol/src/protocol/v2.rs
index b49dd3e7e..3c82d908d 100644
--- a/codex-rs/app-server-protocol/src/protocol/v2.rs
+++ b/codex-rs/app-server-protocol/src/protocol/v2.rs
@@ -11,6 +11,7 @@ use codex_protocol::items::AgentMessageContent as CoreAgentMessageContent;
 use codex_protocol::items::TurnItem as CoreTurnItem;
 use codex_protocol::models::ResponseItem;
 use codex_protocol::parse_command::ParsedCommand as CoreParsedCommand;
+use codex_protocol::protocol::CreditsSnapshot as CoreCreditsSnapshot;
 use codex_protocol::protocol::RateLimitSnapshot as CoreRateLimitSnapshot;
 use codex_protocol::protocol::RateLimitWindow as CoreRateLimitWindow;
 use codex_protocol::user_input::UserInput as CoreUserInput;
@@ -994,6 +995,7 @@ pub struct AccountRateLimitsUpdatedNotification {
 pub struct RateLimitSnapshot {
     pub primary: Option<RateLimitWindow>,
     pub secondary: Option<RateLimitWindow>,
+    pub credits: Option<CreditsSnapshot>,
 }
 
 impl From<CoreRateLimitSnapshot> for RateLimitSnapshot {
@@ -1001,6 +1003,7 @@ impl From<CoreRateLimitSnapshot> for RateLimitSnapshot {
         Self {
             primary: value.primary.map(RateLimitWindow::from),
             secondary: value.secondary.map(RateLimitWindow::from),
+            credits: value.credits.map(CreditsSnapshot::from),
         }
     }
 }
@@ -1024,6 +1027,25 @@ impl From<CoreRateLimitWindow> for RateLimitWindow {
     }
 }
 
+#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
+#[serde(rename_all = "camelCase")]
+#[ts(export_to = "v2/")]
+pub struct CreditsSnapshot {
+    pub has_credits: bool,
+    pub unlimited: bool,
+    pub balance: Option<String>,
+}
+
+impl From<CoreCreditsSnapshot> for CreditsSnapshot {
+    fn from(value: CoreCreditsSnapshot) -> Self {
+        Self {
+            has_credits: value.has_credits,
+            unlimited: value.unlimited,
+            balance: value.balance,
+        }
+    }
+}
+
 #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
 #[serde(rename_all = "camelCase")]
 #[ts(export_to = "v2/")]
diff --git a/codex-rs/app-server/src/message_processor.rs b/codex-rs/app-server/src/message_processor.rs
index 55f857351..93af87038 100644
--- a/codex-rs/app-server/src/message_processor.rs
+++ b/codex-rs/app-server/src/message_processor.rs
@@ -191,8 +191,15 @@ impl MessageProcessor {
         }
 
         // This function is stubbed out to return None on non-Windows platforms
+        let cwd = match std::env::current_dir() {
+            Ok(cwd) => cwd,
+            Err(_) => return,
+        };
         if let Some((sample_paths, extra_count, failed_scan)) =
-            codex_windows_sandbox::world_writable_warning_details(self.config.codex_home.as_path())
+            codex_windows_sandbox::world_writable_warning_details(
+                self.config.codex_home.as_path(),
+                cwd,
+            )
         {
             self.outgoing
                 .send_server_notification(ServerNotification::WindowsWorldWritableWarning(
diff --git a/codex-rs/app-server/src/outgoing_message.rs b/codex-rs/app-server/src/outgoing_message.rs
index 40260c8b9..b7f331c9d 100644
--- a/codex-rs/app-server/src/outgoing_message.rs
+++ b/codex-rs/app-server/src/outgoing_message.rs
@@ -229,6 +229,7 @@ mod tests {
                         resets_at: Some(123),
                     }),
                     secondary: None,
+                    credits: None,
                 },
             });
 
@@ -243,7 +244,8 @@ mod tests {
                             "windowDurationMins": 15,
                             "resetsAt": 123
                         },
-                        "secondary": null
+                        "secondary": null,
+                        "credits": null
                     }
                 },
             }),
diff --git a/codex-rs/app-server/tests/suite/v2/rate_limits.rs b/codex-rs/app-server/tests/suite/v2/rate_limits.rs
index d0cba8366..7ddccf7a7 100644
--- a/codex-rs/app-server/tests/suite/v2/rate_limits.rs
+++ b/codex-rs/app-server/tests/suite/v2/rate_limits.rs
@@ -152,6 +152,7 @@ async fn get_account_rate_limits_returns_snapshot() -> Result<()> {
                 window_duration_mins: Some(1440),
                 resets_at: Some(secondary_reset_timestamp),
             }),
+            credits: None,
         },
     };
     assert_eq!(received, expected);
diff --git a/codex-rs/backend-client/src/client.rs b/codex-rs/backend-client/src/client.rs
index 28a51598e..0fb627ef0 100644
--- a/codex-rs/backend-client/src/client.rs
+++ b/codex-rs/backend-client/src/client.rs
@@ -1,4 +1,5 @@
 use crate::types::CodeTaskDetailsResponse;
+use crate::types::CreditStatusDetails;
 use crate::types::PaginatedListTaskListItem;
 use crate::types::RateLimitStatusPayload;
 use crate::types::RateLimitWindowSnapshot;
@@ -6,6 +7,7 @@ use crate::types::TurnAttemptsSiblingTurnsResponse;
 use anyhow::Result;
 use codex_core::auth::CodexAuth;
 use codex_core::default_client::get_codex_user_agent;
+use codex_protocol::protocol::CreditsSnapshot;
 use codex_protocol::protocol::RateLimitSnapshot;
 use codex_protocol::protocol::RateLimitWindow;
 use reqwest::header::AUTHORIZATION;
@@ -272,19 +274,23 @@ impl Client {
 
     // rate limit helpers
     fn rate_limit_snapshot_from_payload(payload: RateLimitStatusPayload) -> RateLimitSnapshot {
-        let Some(details) = payload
+        let rate_limit_details = payload
             .rate_limit
-            .and_then(|inner| inner.map(|boxed| *boxed))
-        else {
-            return RateLimitSnapshot {
-                primary: None,
-                secondary: None,
-            };
+            .and_then(|inner| inner.map(|boxed| *boxed));
+
+        let (primary, secondary) = if let Some(details) = rate_limit_details {
+            (
+                Self::map_rate_limit_window(details.primary_window),
+                Self::map_rate_limit_window(details.secondary_window),
+            )
+        } else {
+            (None, None)
         };
 
         RateLimitSnapshot {
-            primary: Self::map_rate_limit_window(details.primary_window),
-            secondary: Self::map_rate_limit_window(details.secondary_window),
+            primary,
+            secondary,
+            credits: Self::map_credits(payload.credits),
         }
     }
 
@@ -306,6 +312,19 @@ impl Client {
         })
     }
 
+    fn map_credits(credits: Option<Option<Box<CreditStatusDetails>>>) -> Option<CreditsSnapshot> {
+        let details = match credits {
+            Some(Some(details)) => *details,
+            _ => return None,
+        };
+
+        Some(CreditsSnapshot {
+            has_credits: details.has_credits,
+            unlimited: details.unlimited,
+            balance: details.balance.and_then(|inner| inner),
+        })
+    }
+
     fn window_minutes_from_seconds(seconds: i32) -> Option<i64> {
         if seconds <= 0 {
             return None;
diff --git a/codex-rs/backend-client/src/types.rs b/codex-rs/backend-client/src/types.rs
index 9f196f9c2..afeb231a1 100644
--- a/codex-rs/backend-client/src/types.rs
+++ b/codex-rs/backend-client/src/types.rs
@@ -1,3 +1,4 @@
+pub use codex_backend_openapi_models::models::CreditStatusDetails;
 pub use codex_backend_openapi_models::models::PaginatedListTaskListItem;
 pub use codex_backend_openapi_models::models::PlanType;
 pub use codex_backend_openapi_models::models::RateLimitStatusDetails;
diff --git a/codex-rs/core/src/client.rs b/codex-rs/core/src/client.rs
index 13c277a77..68bd30f4e 100644
--- a/codex-rs/core/src/client.rs
+++ b/codex-rs/core/src/client.rs
@@ -56,6 +56,7 @@ use crate::model_family::ModelFamily;
 use crate::model_provider_info::ModelProviderInfo;
 use crate::model_provider_info::WireApi;
 use crate::openai_model_info::get_model_info;
+use crate::protocol::CreditsSnapshot;
 use crate::protocol::RateLimitSnapshot;
 use crate::protocol::RateLimitWindow;
 use crate::protocol::TokenUsage;
@@ -726,7 +727,13 @@ fn parse_rate_limit_snapshot(headers: &HeaderMap) -> Option<RateLimitSnapshot> {
         "x-codex-secondary-reset-at",
     );
 
-    Some(RateLimitSnapshot { primary, secondary })
+    let credits = parse_credits_snapshot(headers);
+
+    Some(RateLimitSnapshot {
+        primary,
+        secondary,
+        credits,
+    })
 }
 
 fn parse_rate_limit_window(
@@ -753,6 +760,20 @@ fn parse_rate_limit_window(
     })
 }
 
+fn parse_credits_snapshot(headers: &HeaderMap) -> Option<CreditsSnapshot> {
+    let has_credits = parse_header_bool(headers, "x-codex-credits-has-credits")?;
+    let unlimited = parse_header_bool(headers, "x-codex-credits-unlimited")?;
+    let balance = parse_header_str(headers, "x-codex-credits-balance")
+        .map(str::trim)
+        .filter(|value| !value.is_empty())
+        .map(std::string::ToString::to_string);
+    Some(CreditsSnapshot {
+        has_credits,
+        unlimited,
+        balance,
+    })
+}
+
 fn parse_header_f64(headers: &HeaderMap, name: &str) -> Option<f64> {
     parse_header_str(headers, name)?
         .parse::<f64>()
@@ -764,6 +785,17 @@ fn parse_header_i64(headers: &HeaderMap, name: &str) -> Option<i64> {
     parse_header_str(headers, name)?.parse::<i64>().ok()
 }
 
+fn parse_header_bool(headers: &HeaderMap, name: &str) -> Option<bool> {
+    let raw = parse_header_str(headers, name)?;
+    if raw.eq_ignore_ascii_case("true") || raw == "1" {
+        Some(true)
+    } else if raw.eq_ignore_ascii_case("false") || raw == "0" {
+        Some(false)
+    } else {
+        None
+    }
+}
+
 fn parse_header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
     headers.get(name)?.to_str().ok()
 }
diff --git a/codex-rs/core/src/config/mod.rs b/codex-rs/core/src/config/mod.rs
index 1f53b5c94..4cdb1b0b0 100644
--- a/codex-rs/core/src/config/mod.rs
+++ b/codex-rs/core/src/config/mod.rs
@@ -61,12 +61,9 @@ pub mod edit;
 pub mod profile;
 pub mod types;
 
-#[cfg(target_os = "windows")]
-pub const OPENAI_DEFAULT_MODEL: &str = "gpt-5.1-codex-max";
-#[cfg(not(target_os = "windows"))]
-pub const OPENAI_DEFAULT_MODEL: &str = "gpt-5.1-codex-max";
-const OPENAI_DEFAULT_REVIEW_MODEL: &str = "gpt-5.1-codex-max";
-pub const GPT_5_CODEX_MEDIUM_MODEL: &str = "gpt-5.1-codex-max";
+pub const OPENAI_DEFAULT_MODEL: &str = "gpt-5.1-codex";
+const OPENAI_DEFAULT_REVIEW_MODEL: &str = "gpt-5.1-codex";
+pub const GPT_5_CODEX_MEDIUM_MODEL: &str = "gpt-5.1-codex";
 
 /// Maximum number of bytes of the documentation that will be embedded. Larger
 /// files are *silently truncated* to this size so we do not take up too much of
diff --git a/codex-rs/core/src/error.rs b/codex-rs/core/src/error.rs
index 9a42ec3d1..b2027dc94 100644
--- a/codex-rs/core/src/error.rs
+++ b/codex-rs/core/src/error.rs
@@ -499,6 +499,7 @@ mod tests {
                 window_minutes: Some(120),
                 resets_at: Some(secondary_reset_at),
             }),
+            credits: None,
         }
     }
 
diff --git a/codex-rs/core/src/model_family.rs b/codex-rs/core/src/model_family.rs
index 725a998be..ef54a9584 100644
--- a/codex-rs/core/src/model_family.rs
+++ b/codex-rs/core/src/model_family.rs
@@ -173,6 +173,8 @@ pub fn find_family_for_model(slug: &str) -> Option<ModelFamily> {
             support_verbosity: true,
             truncation_policy: TruncationPolicy::Tokens(10_000),
         )
+
+    // Production models.
     } else if slug.starts_with("gpt-5.1-codex-max") {
         model_family!(
             slug, slug,
@@ -185,8 +187,6 @@ pub fn find_family_for_model(slug: &str) -> Option<ModelFamily> {
             support_verbosity: false,
             truncation_policy: TruncationPolicy::Tokens(10_000),
         )
-
-    // Production models.
     } else if slug.starts_with("gpt-5-codex")
         || slug.starts_with("gpt-5.1-codex")
         || slug.starts_with("codex-")
@@ -202,18 +202,6 @@ pub fn find_family_for_model(slug: &str) -> Option<ModelFamily> {
             support_verbosity: false,
             truncation_policy: TruncationPolicy::Tokens(10_000),
         )
-    } else if slug.starts_with("gpt-5.1-codex-max") {
-        model_family!(
-            slug, slug,
-            supports_reasoning_summaries: true,
-            reasoning_summary_format: ReasoningSummaryFormat::Experimental,
-            base_instructions: BASE_INSTRUCTIONS.to_string(),
-            apply_patch_tool_type: Some(ApplyPatchToolType::Freeform),
-            shell_type: ConfigShellToolType::ShellCommand,
-            supports_parallel_tool_calls: true,
-            support_verbosity: false,
-            truncation_policy: TruncationPolicy::Tokens(10_000),
-        )
     } else if slug.starts_with("gpt-5.1") {
         model_family!(
             slug, "gpt-5.1",
diff --git a/codex-rs/core/src/tasks/mod.rs b/codex-rs/core/src/tasks/mod.rs
index 9bda02c34..684e039f8 100644
--- a/codex-rs/core/src/tasks/mod.rs
+++ b/codex-rs/core/src/tasks/mod.rs
@@ -98,6 +98,9 @@ pub(crate) trait SessionTask: Send + Sync + 'static {
 }
 
 impl Session {
+    // Temporary experiment: disable TaskComplete emission so downstream clients rely on watchdogs.
+    const EMIT_TASK_COMPLETE_EVENT: bool = false;
+
     pub async fn spawn_task<T: SessionTask>(
         self: &Arc<Self>,
         turn_context: Arc<TurnContext>,
@@ -168,6 +171,9 @@ impl Session {
             *active = None;
         }
         drop(active);
+        if !Self::EMIT_TASK_COMPLETE_EVENT {
+            return;
+        }
         let event = EventMsg::TaskComplete(TaskCompleteEvent { last_agent_message });
         self.send_event(turn_context.as_ref(), event).await;
     }
diff --git a/codex-rs/core/tests/suite/client.rs b/codex-rs/core/tests/suite/client.rs
index e15e05a99..59b7e3417 100644
--- a/codex-rs/core/tests/suite/client.rs
+++ b/codex-rs/core/tests/suite/client.rs
@@ -1121,7 +1121,8 @@ async fn token_count_includes_rate_limits_snapshot() {
                     "used_percent": 40.0,
                     "window_minutes": 60,
                     "resets_at": 1704074400
-                }
+                },
+                "credits": null
             }
         })
     );
@@ -1168,7 +1169,8 @@ async fn token_count_includes_rate_limits_snapshot() {
                     "used_percent": 40.0,
                     "window_minutes": 60,
                     "resets_at": 1704074400
-                }
+                },
+                "credits": null
             }
         })
     );
@@ -1238,7 +1240,8 @@ async fn usage_limit_error_emits_rate_limit_event() -> anyhow::Result<()> {
             "used_percent": 87.5,
             "window_minutes": 60,
             "resets_at": null
-        }
+        },
+        "credits": null
     });
 
     let submission_id = codex
diff --git a/codex-rs/protocol/src/protocol.rs b/codex-rs/protocol/src/protocol.rs
index e3bc76199..1825d7636 100644
--- a/codex-rs/protocol/src/protocol.rs
+++ b/codex-rs/protocol/src/protocol.rs
@@ -790,6 +790,7 @@ pub struct TokenCountEvent {
 pub struct RateLimitSnapshot {
     pub primary: Option<RateLimitWindow>,
     pub secondary: Option<RateLimitWindow>,
+    pub credits: Option<CreditsSnapshot>,
 }
 
 #[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema, TS)]
@@ -804,6 +805,13 @@ pub struct RateLimitWindow {
     pub resets_at: Option<i64>,
 }
 
+#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema, TS)]
+pub struct CreditsSnapshot {
+    pub has_credits: bool,
+    pub unlimited: bool,
+    pub balance: Option<String>,
+}
+
 // Includes prompts, tools and space to call compact.
 const BASELINE_TOKENS: i64 = 12000;
 
diff --git a/codex-rs/tui/src/app.rs b/codex-rs/tui/src/app.rs
index 7c86dd3b6..cbf9c3263 100644
--- a/codex-rs/tui/src/app.rs
+++ b/codex-rs/tui/src/app.rs
@@ -499,6 +499,9 @@ impl App {
             AppEvent::CommitTick => {
                 self.chat_widget.on_commit_tick();
             }
+            AppEvent::TaskInactivityTimeout(token) => {
+                self.chat_widget.on_task_inactivity_timeout(token);
+            }
             AppEvent::CodexEvent(event) => {
                 self.chat_widget.handle_codex_event(event);
             }
diff --git a/codex-rs/tui/src/app_event.rs b/codex-rs/tui/src/app_event.rs
index cf494f57d..df72aa575 100644
--- a/codex-rs/tui/src/app_event.rs
+++ b/codex-rs/tui/src/app_event.rs
@@ -53,6 +53,8 @@ pub(crate) enum AppEvent {
     StartCommitAnimation,
     StopCommitAnimation,
     CommitTick,
+    /// Fired when no live Codex events have arrived for the configured timeout.
+    TaskInactivityTimeout(u64),
 
     /// Update the current reasoning effort in the running app and widget.
     UpdateReasoningEffort(Option<ReasoningEffort>),
diff --git a/codex-rs/tui/src/chatwidget.rs b/codex-rs/tui/src/chatwidget.rs
index 9429ce143..686f6d1a4 100644
--- a/codex-rs/tui/src/chatwidget.rs
+++ b/codex-rs/tui/src/chatwidget.rs
@@ -267,6 +267,7 @@ pub(crate) struct ChatWidget {
     running_commands: HashMap<String, RunningCommand>,
     task_complete_pending: bool,
     mcp_startup_status: Option<HashMap<String, McpStartupStatus>>,
+    task_inactivity_timer_seq: u64,
     // Queue of interruptive UI events deferred during an active write cycle
     interrupts: InterruptManager,
     // Accumulates the current reasoning block text to extract a header
@@ -332,6 +333,7 @@ fn create_initial_user_message(text: String, image_paths: Vec<PathBuf>) -> Optio
 }
 
 impl ChatWidget {
+    const TASK_INACTIVITY_TIMEOUT_SECS: u64 = 10;
     fn flush_answer_stream_with_separator(&mut self) {
         if let Some(mut controller) = self.stream_controller.take()
             && let Some(cell) = controller.finalize()
@@ -475,6 +477,7 @@ impl ChatWidget {
         self.flush_answer_stream_with_separator();
         // Mark task stopped and request redraw now that all content is in history.
         self.bottom_pane.set_task_running(false);
+        self.cancel_task_inactivity_timer();
         self.running_commands.clear();
         self.request_redraw();
 
@@ -561,6 +564,7 @@ impl ChatWidget {
         self.finalize_active_cell_as_failed();
         // Reset running state and clear streaming buffers.
         self.bottom_pane.set_task_running(false);
+        self.cancel_task_inactivity_timer();
         self.running_commands.clear();
         self.stream_controller = None;
         self.maybe_show_pending_rate_limit_prompt();
@@ -885,6 +889,54 @@ impl ChatWidget {
         self.flush_interrupt_queue();
     }
 
+    fn note_live_event_activity(&mut self, from_replay: bool) {
+        if from_replay {
+            return;
+        }
+        self.restart_task_inactivity_timer();
+    }
+
+    fn restart_task_inactivity_timer(&mut self) {
+        if !self.bottom_pane.is_task_running() {
+            return;
+        }
+        self.task_inactivity_timer_seq = self.task_inactivity_timer_seq.wrapping_add(1);
+        let token = self.task_inactivity_timer_seq;
+        let tx = self.app_event_tx.clone();
+        tokio::spawn(async move {
+            tokio::time::sleep(Duration::from_secs(
+                ChatWidget::TASK_INACTIVITY_TIMEOUT_SECS,
+            ))
+            .await;
+            tx.send(AppEvent::TaskInactivityTimeout(token));
+        });
+    }
+
+    fn cancel_task_inactivity_timer(&mut self) {
+        self.task_inactivity_timer_seq = self.task_inactivity_timer_seq.wrapping_add(1);
+    }
+
+    pub(crate) fn on_task_inactivity_timeout(&mut self, token: u64) {
+        if token != self.task_inactivity_timer_seq {
+            return;
+        }
+        self.cancel_task_inactivity_timer();
+        if !self.bottom_pane.is_task_running() {
+            return;
+        }
+        tracing::warn!(
+            "task inactivity watchdog released bottom pane after {}s with no events",
+            ChatWidget::TASK_INACTIVITY_TIMEOUT_SECS
+        );
+        self.bottom_pane.set_task_running(false);
+        self.running_commands.clear();
+        // Treat inactivity as an implicit end-of-turn: if there are queued inputs,
+        // send exactly one to start the next turn, mirroring TaskComplete.
+        self.maybe_send_next_queued_input();
+        self.maybe_show_pending_rate_limit_prompt();
+        self.request_redraw();
+    }
+
     #[inline]
     fn handle_streaming_delta(&mut self, delta: String) {
         // Before streaming agent content, flush any active exec cell group.
@@ -1139,6 +1191,7 @@ impl ChatWidget {
             running_commands: HashMap::new(),
             task_complete_pending: false,
             mcp_startup_status: None,
+            task_inactivity_timer_seq: 0,
             interrupts: InterruptManager::new(),
             reasoning_buffer: String::new(),
             full_reasoning_buffer: String::new(),
@@ -1212,6 +1265,7 @@ impl ChatWidget {
             running_commands: HashMap::new(),
             task_complete_pending: false,
             mcp_startup_status: None,
+            task_inactivity_timer_seq: 0,
             interrupts: InterruptManager::new(),
             reasoning_buffer: String::new(),
             full_reasoning_buffer: String::new(),
@@ -1687,6 +1741,8 @@ impl ChatWidget {
             | EventMsg::ReasoningContentDelta(_)
             | EventMsg::ReasoningRawContentDelta(_) => {}
         }
+
+        self.note_live_event_activity(from_replay);
     }
 
     fn on_entered_review_mode(&mut self, review: ReviewRequest) {
@@ -2300,7 +2356,11 @@ impl ChatWidget {
         {
             return None;
         }
-        codex_windows_sandbox::world_writable_warning_details(self.config.codex_home.as_path())
+        let cwd = match std::env::current_dir() {
+            Ok(cwd) => cwd,
+            Err(_) => return Some((Vec::new(), 0, true)),
+        };
+        codex_windows_sandbox::world_writable_warning_details(self.config.codex_home.as_path(), cwd)
     }
 
     #[cfg(not(target_os = "windows"))]
diff --git a/codex-rs/tui/src/chatwidget/tests.rs b/codex-rs/tui/src/chatwidget/tests.rs
index b4305bac7..0f67579b6 100644
--- a/codex-rs/tui/src/chatwidget/tests.rs
+++ b/codex-rs/tui/src/chatwidget/tests.rs
@@ -81,6 +81,7 @@ fn snapshot(percent: f64) -> RateLimitSnapshot {
             resets_at: None,
         }),
         secondary: None,
+        credits: None,
     }
 }
 
diff --git a/codex-rs/tui/src/status/card.rs b/codex-rs/tui/src/status/card.rs
index be10eb569..d77a4d494 100644
--- a/codex-rs/tui/src/status/card.rs
+++ b/codex-rs/tui/src/status/card.rs
@@ -28,6 +28,7 @@ use super::helpers::format_tokens_compact;
 use super::rate_limits::RateLimitSnapshotDisplay;
 use super::rate_limits::StatusRateLimitData;
 use super::rate_limits::StatusRateLimitRow;
+use super::rate_limits::StatusRateLimitValue;
 use super::rate_limits::compose_rate_limit_data;
 use super::rate_limits::format_status_limit_summary;
 use super::rate_limits::render_status_limit_progress_bar;
@@ -215,29 +216,44 @@ impl StatusHistoryCell {
         let mut lines = Vec::with_capacity(rows.len().saturating_mul(2));
 
         for row in rows {
-            let percent_remaining = (100.0 - row.percent_used).clamp(0.0, 100.0);
-            let value_spans = vec![
-                Span::from(render_status_limit_progress_bar(percent_remaining)),
-                Span::from(" "),
-                Span::from(format_status_limit_summary(percent_remaining)),
-            ];
-            let base_spans = formatter.full_spans(row.label.as_str(), value_spans);
-            let base_line = Line::from(base_spans.clone());
-
-            if let Some(resets_at) = row.resets_at.as_ref() {
-                let resets_span = Span::from(format!("(resets {resets_at})")).dim();
-                let mut inline_spans = base_spans.clone();
-                inline_spans.push(Span::from(" ").dim());
-                inline_spans.push(resets_span.clone());
-
-                if line_display_width(&Line::from(inline_spans.clone())) <= available_inner_width {
-                    lines.push(Line::from(inline_spans));
-                } else {
-                    lines.push(base_line);
-                    lines.push(formatter.continuation(vec![resets_span]));
+            match &row.value {
+                StatusRateLimitValue::Window {
+                    percent_used,
+                    resets_at,
+                } => {
+                    let percent_remaining = (100.0 - percent_used).clamp(0.0, 100.0);
+                    let value_spans = vec![
+                        Span::from(render_status_limit_progress_bar(percent_remaining)),
+                        Span::from(" "),
+                        Span::from(format_status_limit_summary(percent_remaining)),
+                    ];
+                    let base_spans = formatter.full_spans(row.label.as_str(), value_spans);
+                    let base_line = Line::from(base_spans.clone());
+
+                    if let Some(resets_at) = resets_at.as_ref() {
+                        let resets_span = Span::from(format!("(resets {resets_at})")).dim();
+                        let mut inline_spans = base_spans.clone();
+                        inline_spans.push(Span::from(" ").dim());
+                        inline_spans.push(resets_span.clone());
+
+                        if line_display_width(&Line::from(inline_spans.clone()))
+                            <= available_inner_width
+                        {
+                            lines.push(Line::from(inline_spans));
+                        } else {
+                            lines.push(base_line);
+                            lines.push(formatter.continuation(vec![resets_span]));
+                        }
+                    } else {
+                        lines.push(base_line);
+                    }
+                }
+                StatusRateLimitValue::Text(text) => {
+                    let label = row.label.clone();
+                    let spans =
+                        formatter.full_spans(label.as_str(), vec![Span::from(text.clone())]);
+                    lines.push(Line::from(spans));
                 }
-            } else {
-                lines.push(base_line);
             }
         }
 
diff --git a/codex-rs/tui/src/status/rate_limits.rs b/codex-rs/tui/src/status/rate_limits.rs
index 50cbd9779..e8dc689a6 100644
--- a/codex-rs/tui/src/status/rate_limits.rs
+++ b/codex-rs/tui/src/status/rate_limits.rs
@@ -5,6 +5,7 @@ use chrono::DateTime;
 use chrono::Duration as ChronoDuration;
 use chrono::Local;
 use chrono::Utc;
+use codex_core::protocol::CreditsSnapshot as CoreCreditsSnapshot;
 use codex_core::protocol::RateLimitSnapshot;
 use codex_core::protocol::RateLimitWindow;
 
@@ -15,8 +16,16 @@ const STATUS_LIMIT_BAR_EMPTY: &str = "░";
 #[derive(Debug, Clone)]
 pub(crate) struct StatusRateLimitRow {
     pub label: String,
-    pub percent_used: f64,
-    pub resets_at: Option<String>,
+    pub value: StatusRateLimitValue,
+}
+
+#[derive(Debug, Clone)]
+pub(crate) enum StatusRateLimitValue {
+    Window {
+        percent_used: f64,
+        resets_at: Option<String>,
+    },
+    Text(String),
 }
 
 #[derive(Debug, Clone)]
@@ -56,6 +65,14 @@ pub(crate) struct RateLimitSnapshotDisplay {
     pub captured_at: DateTime<Local>,
     pub primary: Option<RateLimitWindowDisplay>,
     pub secondary: Option<RateLimitWindowDisplay>,
+    pub credits: Option<CreditsSnapshotDisplay>,
+}
+
+#[derive(Debug, Clone)]
+pub(crate) struct CreditsSnapshotDisplay {
+    pub has_credits: bool,
+    pub unlimited: bool,
+    pub balance: Option<String>,
 }
 
 pub(crate) fn rate_limit_snapshot_display(
@@ -72,6 +89,17 @@ pub(crate) fn rate_limit_snapshot_display(
             .secondary
             .as_ref()
             .map(|window| RateLimitWindowDisplay::from_window(window, captured_at)),
+        credits: snapshot.credits.as_ref().map(CreditsSnapshotDisplay::from),
+    }
+}
+
+impl From<&CoreCreditsSnapshot> for CreditsSnapshotDisplay {
+    fn from(value: &CoreCreditsSnapshot) -> Self {
+        Self {
+            has_credits: value.has_credits,
+            unlimited: value.unlimited,
+            balance: value.balance.clone(),
+        }
     }
 }
 
@@ -81,7 +109,7 @@ pub(crate) fn compose_rate_limit_data(
 ) -> StatusRateLimitData {
     match snapshot {
         Some(snapshot) => {
-            let mut rows = Vec::with_capacity(2);
+            let mut rows = Vec::with_capacity(3);
 
             if let Some(primary) = snapshot.primary.as_ref() {
                 let label: String = primary
@@ -91,8 +119,10 @@ pub(crate) fn compose_rate_limit_data(
                 let label = capitalize_first(&label);
                 rows.push(StatusRateLimitRow {
                     label: format!("{label} limit"),
-                    percent_used: primary.used_percent,
-                    resets_at: primary.resets_at.clone(),
+                    value: StatusRateLimitValue::Window {
+                        percent_used: primary.used_percent,
+                        resets_at: primary.resets_at.clone(),
+                    },
                 });
             }
 
@@ -104,11 +134,19 @@ pub(crate) fn compose_rate_limit_data(
                 let label = capitalize_first(&label);
                 rows.push(StatusRateLimitRow {
                     label: format!("{label} limit"),
-                    percent_used: secondary.used_percent,
-                    resets_at: secondary.resets_at.clone(),
+                    value: StatusRateLimitValue::Window {
+                        percent_used: secondary.used_percent,
+                        resets_at: secondary.resets_at.clone(),
+                    },
                 });
             }
 
+            if let Some(credits) = snapshot.credits.as_ref()
+                && let Some(row) = credit_status_row(credits)
+            {
+                rows.push(row);
+            }
+
             let is_stale = now.signed_duration_since(snapshot.captured_at)
                 > ChronoDuration::minutes(RATE_LIMIT_STALE_THRESHOLD_MINUTES);
 
@@ -140,6 +178,50 @@ pub(crate) fn format_status_limit_summary(percent_remaining: f64) -> String {
     format!("{percent_remaining:.0}% left")
 }
 
+/// Builds a single `StatusRateLimitRow` for credits when the snapshot indicates
+/// that the account has credit tracking enabled. When credits are unlimited we
+/// show that fact explicitly; otherwise we render the rounded balance in
+/// credits. Accounts with credits = 0 skip this section entirely.
+fn credit_status_row(credits: &CreditsSnapshotDisplay) -> Option<StatusRateLimitRow> {
+    if !credits.has_credits {
+        return None;
+    }
+    if credits.unlimited {
+        return Some(StatusRateLimitRow {
+            label: "Credits".to_string(),
+            value: StatusRateLimitValue::Text("Unlimited".to_string()),
+        });
+    }
+    let balance = credits.balance.as_ref()?;
+    let display_balance = format_credit_balance(balance)?;
+    Some(StatusRateLimitRow {
+        label: "Credits".to_string(),
+        value: StatusRateLimitValue::Text(format!("{display_balance} credits")),
+    })
+}
+
+fn format_credit_balance(raw: &str) -> Option<String> {
+    let trimmed = raw.trim();
+    if trimmed.is_empty() {
+        return None;
+    }
+
+    if let Ok(int_value) = trimmed.parse::<i64>()
+        && int_value > 0
+    {
+        return Some(int_value.to_string());
+    }
+
+    if let Ok(value) = trimmed.parse::<f64>()
+        && value > 0.0
+    {
+        let rounded = value.round() as i64;
+        return Some(rounded.to_string());
+    }
+
+    None
+}
+
 fn capitalize_first(label: &str) -> String {
     let mut chars = label.chars();
     match chars.next() {
diff --git a/codex-rs/tui/src/status/snapshots/codex_tui__status__tests__status_snapshot_cached_limits_hide_credits_without_flag.snap b/codex-rs/tui/src/status/snapshots/codex_tui__status__tests__status_snapshot_cached_limits_hide_credits_without_flag.snap
new file mode 100644
index 000000000..dbb634bab
--- /dev/null
+++ b/codex-rs/tui/src/status/snapshots/codex_tui__status__tests__status_snapshot_cached_limits_hide_credits_without_flag.snap
@@ -0,0 +1,24 @@
+---
+source: tui/src/status/tests.rs
+expression: sanitized
+---
+/status
+
+╭─────────────────────────────────────────────────────────────────────╮
+│  >_ OpenAI Codex (v0.0.0)                                           │
+│                                                                     │
+│ Visit https://chatgpt.com/codex/settings/usage for up-to-date       │
+│ information on rate limits and credits                              │
+│                                                                     │
+│  Model:            gpt-5.1-codex (reasoning none, summaries auto)   │
+│  Directory: [[workspace]]                                           │
+│  Approval:         on-request                                       │
+│  Sandbox:          read-only                                        │
+│  Agents.md:        <none>                                           │
+│                                                                     │
+│  Token usage:      1.05K total  (700 input + 350 output)            │
+│  Context window:   100% left (1.45K used / 272K)                    │
+│  5h limit:         [████████░░░░░░░░░░░░] 40% left (resets 11:32)   │
+│  Weekly limit:     [█████████████░░░░░░░] 65% left (resets 11:52)   │
+│  Warning:          limits may be stale - start new turn to refresh. │
+╰─────────────────────────────────────────────────────────────────────╯
diff --git a/codex-rs/tui/src/status/snapshots/codex_tui__status__tests__status_snapshot_includes_credits_and_limits.snap b/codex-rs/tui/src/status/snapshots/codex_tui__status__tests__status_snapshot_includes_credits_and_limits.snap
new file mode 100644
index 000000000..1707a4c5f
--- /dev/null
+++ b/codex-rs/tui/src/status/snapshots/codex_tui__status__tests__status_snapshot_includes_credits_and_limits.snap
@@ -0,0 +1,24 @@
+---
+source: tui/src/status/tests.rs
+expression: sanitized
+---
+/status
+
+╭───────────────────────────────────────────────────────────────────╮
+│  >_ OpenAI Codex (v0.0.0)                                         │
+│                                                                   │
+│ Visit https://chatgpt.com/codex/settings/usage for up-to-date     │
+│ information on rate limits and credits                            │
+│                                                                   │
+│  Model:            gpt-5.1-codex (reasoning none, summaries auto) │
+│  Directory: [[workspace]]                                         │
+│  Approval:         on-request                                     │
+│  Sandbox:          read-only                                      │
+│  Agents.md:        <none>                                         │
+│                                                                   │
+│  Token usage:      2K total  (1.4K input + 600 output)            │
+│  Context window:   100% left (2.2K used / 272K)                   │
+│  5h limit:         [███████████░░░░░░░░░] 55% left (resets 09:25) │
+│  Weekly limit:     [██████████████░░░░░░] 70% left (resets 09:55) │
+│  Credits:          38 credits                                     │
+╰───────────────────────────────────────────────────────────────────╯
diff --git a/codex-rs/tui/src/status/tests.rs b/codex-rs/tui/src/status/tests.rs
index c6029bde7..ae379aae6 100644
--- a/codex-rs/tui/src/status/tests.rs
+++ b/codex-rs/tui/src/status/tests.rs
@@ -8,6 +8,7 @@ use codex_core::AuthManager;
 use codex_core::config::Config;
 use codex_core::config::ConfigOverrides;
 use codex_core::config::ConfigToml;
+use codex_core::protocol::CreditsSnapshot;
 use codex_core::protocol::RateLimitSnapshot;
 use codex_core::protocol::RateLimitWindow;
 use codex_core::protocol::SandboxPolicy;
@@ -118,6 +119,7 @@ fn status_snapshot_includes_reasoning_details() {
             window_minutes: Some(10080),
             resets_at: Some(reset_at_from(&captured_at, 1_200)),
         }),
+        credits: None,
     };
     let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);
 
@@ -168,6 +170,7 @@ fn status_snapshot_includes_monthly_limit() {
             resets_at: Some(reset_at_from(&captured_at, 86_400)),
         }),
         secondary: None,
+        credits: None,
     };
     let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);
 
@@ -190,6 +193,154 @@ fn status_snapshot_includes_monthly_limit() {
     assert_snapshot!(sanitized);
 }
 
+#[test]
+fn status_snapshot_shows_unlimited_credits() {
+    let temp_home = TempDir::new().expect("temp home");
+    let config = test_config(&temp_home);
+    let auth_manager = test_auth_manager(&config);
+    let usage = TokenUsage::default();
+    let captured_at = chrono::Local
+        .with_ymd_and_hms(2024, 2, 3, 4, 5, 6)
+        .single()
+        .expect("timestamp");
+    let snapshot = RateLimitSnapshot {
+        primary: None,
+        secondary: None,
+        credits: Some(CreditsSnapshot {
+            has_credits: true,
+            unlimited: true,
+            balance: None,
+        }),
+    };
+    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);
+    let composite = new_status_output(
+        &config,
+        &auth_manager,
+        &usage,
+        Some(&usage),
+        &None,
+        Some(&rate_display),
+        captured_at,
+    );
+    let rendered = render_lines(&composite.display_lines(120));
+    assert!(
+        rendered
+            .iter()
+            .any(|line| line.contains("Credits:") && line.contains("Unlimited")),
+        "expected Credits: Unlimited line, got {rendered:?}"
+    );
+}
+
+#[test]
+fn status_snapshot_shows_positive_credits() {
+    let temp_home = TempDir::new().expect("temp home");
+    let config = test_config(&temp_home);
+    let auth_manager = test_auth_manager(&config);
+    let usage = TokenUsage::default();
+    let captured_at = chrono::Local
+        .with_ymd_and_hms(2024, 3, 4, 5, 6, 7)
+        .single()
+        .expect("timestamp");
+    let snapshot = RateLimitSnapshot {
+        primary: None,
+        secondary: None,
+        credits: Some(CreditsSnapshot {
+            has_credits: true,
+            unlimited: false,
+            balance: Some("12.5".to_string()),
+        }),
+    };
+    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);
+    let composite = new_status_output(
+        &config,
+        &auth_manager,
+        &usage,
+        Some(&usage),
+        &None,
+        Some(&rate_display),
+        captured_at,
+    );
+    let rendered = render_lines(&composite.display_lines(120));
+    assert!(
+        rendered
+            .iter()
+            .any(|line| line.contains("Credits:") && line.contains("13 credits")),
+        "expected Credits line with rounded credits, got {rendered:?}"
+    );
+}
+
+#[test]
+fn status_snapshot_hides_zero_credits() {
+    let temp_home = TempDir::new().expect("temp home");
+    let config = test_config(&temp_home);
+    let auth_manager = test_auth_manager(&config);
+    let usage = TokenUsage::default();
+    let captured_at = chrono::Local
+        .with_ymd_and_hms(2024, 4, 5, 6, 7, 8)
+        .single()
+        .expect("timestamp");
+    let snapshot = RateLimitSnapshot {
+        primary: None,
+        secondary: None,
+        credits: Some(CreditsSnapshot {
+            has_credits: true,
+            unlimited: false,
+            balance: Some("0".to_string()),
+        }),
+    };
+    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);
+    let composite = new_status_output(
+        &config,
+        &auth_manager,
+        &usage,
+        Some(&usage),
+        &None,
+        Some(&rate_display),
+        captured_at,
+    );
+    let rendered = render_lines(&composite.display_lines(120));
+    assert!(
+        rendered.iter().all(|line| !line.contains("Credits:")),
+        "expected no Credits line, got {rendered:?}"
+    );
+}
+
+#[test]
+fn status_snapshot_hides_when_has_no_credits_flag() {
+    let temp_home = TempDir::new().expect("temp home");
+    let config = test_config(&temp_home);
+    let auth_manager = test_auth_manager(&config);
+    let usage = TokenUsage::default();
+    let captured_at = chrono::Local
+        .with_ymd_and_hms(2024, 5, 6, 7, 8, 9)
+        .single()
+        .expect("timestamp");
+    let snapshot = RateLimitSnapshot {
+        primary: None,
+        secondary: None,
+        credits: Some(CreditsSnapshot {
+            has_credits: false,
+            unlimited: true,
+            balance: None,
+        }),
+    };
+    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);
+    let composite = new_status_output(
+        &config,
+        &auth_manager,
+        &usage,
+        Some(&usage),
+        &None,
+        Some(&rate_display),
+        captured_at,
+    );
+    let rendered = render_lines(&composite.display_lines(120));
+    assert!(
+        rendered.iter().all(|line| !line.contains("Credits:")),
+        "expected no Credits line when has_credits is false, got {rendered:?}"
+    );
+}
+
 #[test]
 fn status_card_token_usage_excludes_cached_tokens() {
     let temp_home = TempDir::new().expect("temp home");
@@ -258,6 +409,7 @@ fn status_snapshot_truncates_in_narrow_terminal() {
             resets_at: Some(reset_at_from(&captured_at, 600)),
         }),
         secondary: None,
+        credits: None,
     };
     let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);
 
@@ -321,6 +473,64 @@ fn status_snapshot_shows_missing_limits_message() {
     assert_snapshot!(sanitized);
 }
 
+#[test]
+fn status_snapshot_includes_credits_and_limits() {
+    let temp_home = TempDir::new().expect("temp home");
+    let mut config = test_config(&temp_home);
+    config.model = "gpt-5.1-codex".to_string();
+    config.cwd = PathBuf::from("/workspace/tests");
+
+    let auth_manager = test_auth_manager(&config);
+    let usage = TokenUsage {
+        input_tokens: 1_500,
+        cached_input_tokens: 100,
+        output_tokens: 600,
+        reasoning_output_tokens: 0,
+        total_tokens: 2_200,
+    };
+
+    let captured_at = chrono::Local
+        .with_ymd_and_hms(2024, 7, 8, 9, 10, 11)
+        .single()
+        .expect("timestamp");
+    let snapshot = RateLimitSnapshot {
+        primary: Some(RateLimitWindow {
+            used_percent: 45.0,
+            window_minutes: Some(300),
+            resets_at: Some(reset_at_from(&captured_at, 900)),
+        }),
+        secondary: Some(RateLimitWindow {
+            used_percent: 30.0,
+            window_minutes: Some(10_080),
+            resets_at: Some(reset_at_from(&captured_at, 2_700)),
+        }),
+        credits: Some(CreditsSnapshot {
+            has_credits: true,
+            unlimited: false,
+            balance: Some("37.5".to_string()),
+        }),
+    };
+    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);
+
+    let composite = new_status_output(
+        &config,
+        &auth_manager,
+        &usage,
+        Some(&usage),
+        &None,
+        Some(&rate_display),
+        captured_at,
+    );
+    let mut rendered_lines = render_lines(&composite.display_lines(80));
+    if cfg!(windows) {
+        for line in &mut rendered_lines {
+            *line = line.replace('\\', "/");
+        }
+    }
+    let sanitized = sanitize_directory(rendered_lines).join("\n");
+    assert_snapshot!(sanitized);
+}
+
 #[test]
 fn status_snapshot_shows_empty_limits_message() {
     let temp_home = TempDir::new().expect("temp home");
@@ -340,6 +550,7 @@ fn status_snapshot_shows_empty_limits_message() {
     let snapshot = RateLimitSnapshot {
         primary: None,
         secondary: None,
+        credits: None,
     };
     let captured_at = chrono::Local
         .with_ymd_and_hms(2024, 6, 7, 8, 9, 10)
@@ -397,6 +608,66 @@ fn status_snapshot_shows_stale_limits_message() {
             window_minutes: Some(10_080),
             resets_at: Some(reset_at_from(&captured_at, 1_800)),
         }),
+        credits: None,
+    };
+    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);
+    let now = captured_at + ChronoDuration::minutes(20);
+
+    let composite = new_status_output(
+        &config,
+        &auth_manager,
+        &usage,
+        Some(&usage),
+        &None,
+        Some(&rate_display),
+        now,
+    );
+    let mut rendered_lines = render_lines(&composite.display_lines(80));
+    if cfg!(windows) {
+        for line in &mut rendered_lines {
+            *line = line.replace('\\', "/");
+        }
+    }
+    let sanitized = sanitize_directory(rendered_lines).join("\n");
+    assert_snapshot!(sanitized);
+}
+
+#[test]
+fn status_snapshot_cached_limits_hide_credits_without_flag() {
+    let temp_home = TempDir::new().expect("temp home");
+    let mut config = test_config(&temp_home);
+    config.model = "gpt-5.1-codex".to_string();
+    config.cwd = PathBuf::from("/workspace/tests");
+
+    let auth_manager = test_auth_manager(&config);
+    let usage = TokenUsage {
+        input_tokens: 900,
+        cached_input_tokens: 200,
+        output_tokens: 350,
+        reasoning_output_tokens: 0,
+        total_tokens: 1_450,
+    };
+
+    let captured_at = chrono::Local
+        .with_ymd_and_hms(2024, 9, 10, 11, 12, 13)
+        .single()
+        .expect("timestamp");
+    let snapshot = RateLimitSnapshot {
+        primary: Some(RateLimitWindow {
+            used_percent: 60.0,
+            window_minutes: Some(300),
+            resets_at: Some(reset_at_from(&captured_at, 1_200)),
+        }),
+        secondary: Some(RateLimitWindow {
+            used_percent: 35.0,
+            window_minutes: Some(10_080),
+            resets_at: Some(reset_at_from(&captured_at, 2_400)),
+        }),
+        credits: Some(CreditsSnapshot {
+            has_credits: false,
+            unlimited: false,
+            balance: Some("80".to_string()),
+        }),
     };
     let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);
     let now = captured_at + ChronoDuration::minutes(20);
diff --git a/codex-rs/windows-sandbox-rs/src/audit.rs b/codex-rs/windows-sandbox-rs/src/audit.rs
index 383c652c7..8873bc2e9 100644
--- a/codex-rs/windows-sandbox-rs/src/audit.rs
+++ b/codex-rs/windows-sandbox-rs/src/audit.rs
@@ -8,35 +8,35 @@ use std::path::Path;
 use std::path::PathBuf;
 use std::time::Duration;
 use std::time::Instant;
+use windows_sys::Win32::Foundation::CloseHandle;
 use windows_sys::Win32::Foundation::LocalFree;
 use windows_sys::Win32::Foundation::ERROR_SUCCESS;
 use windows_sys::Win32::Foundation::HLOCAL;
+use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
 use windows_sys::Win32::Security::Authorization::GetNamedSecurityInfoW;
 use windows_sys::Win32::Security::Authorization::GetSecurityInfo;
-use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
-use windows_sys::Win32::Foundation::CloseHandle;
 use windows_sys::Win32::Storage::FileSystem::CreateFileW;
+use windows_sys::Win32::Storage::FileSystem::FILE_APPEND_DATA;
 use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
+use windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_WRITE;
 use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_DELETE;
 use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
 use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE;
-use windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING;
-use windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_WRITE;
+use windows_sys::Win32::Storage::FileSystem::FILE_WRITE_ATTRIBUTES;
 use windows_sys::Win32::Storage::FileSystem::FILE_WRITE_DATA;
-use windows_sys::Win32::Storage::FileSystem::FILE_APPEND_DATA;
 use windows_sys::Win32::Storage::FileSystem::FILE_WRITE_EA;
-use windows_sys::Win32::Storage::FileSystem::FILE_WRITE_ATTRIBUTES;
+use windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING;
 const GENERIC_ALL_MASK: u32 = 0x1000_0000;
 const GENERIC_WRITE_MASK: u32 = 0x4000_0000;
-use windows_sys::Win32::Security::ACL;
-use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
-use windows_sys::Win32::Security::ACL_SIZE_INFORMATION;
 use windows_sys::Win32::Security::AclSizeInformation;
-use windows_sys::Win32::Security::GetAclInformation;
+use windows_sys::Win32::Security::EqualSid;
 use windows_sys::Win32::Security::GetAce;
+use windows_sys::Win32::Security::GetAclInformation;
 use windows_sys::Win32::Security::ACCESS_ALLOWED_ACE;
 use windows_sys::Win32::Security::ACE_HEADER;
-use windows_sys::Win32::Security::EqualSid;
+use windows_sys::Win32::Security::ACL;
+use windows_sys::Win32::Security::ACL_SIZE_INFORMATION;
+use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
 
 // Preflight scan limits
 const MAX_ITEMS_PER_DIR: i32 = 1000;
@@ -162,7 +162,9 @@ unsafe fn path_has_world_write_allow(path: &Path) -> Result<bool> {
     let psid_world = world.as_mut_ptr() as *mut c_void;
     // Very fast mask-based check for world-writable grants (includes GENERIC_*).
     if !dacl_quick_world_write_mask_allows(p_dacl, psid_world) {
-        if !p_sd.is_null() { LocalFree(p_sd as HLOCAL); }
+        if !p_sd.is_null() {
+            LocalFree(p_sd as HLOCAL);
+        }
         return Ok(false);
     }
     // Quick detector flagged a write grant for Everyone: treat as writable.
@@ -202,7 +204,9 @@ pub fn audit_everyone_writable(
             let has = unsafe { path_has_world_write_allow(&p)? };
             if has {
                 let key = normalize_path_key(&p);
-                if seen.insert(key) { flagged.push(p); }
+                if seen.insert(key) {
+                    flagged.push(p);
+                }
             }
         }
     }
@@ -218,7 +222,9 @@ pub fn audit_everyone_writable(
         let has_root = unsafe { path_has_world_write_allow(&root)? };
         if has_root {
             let key = normalize_path_key(&root);
-            if seen.insert(key) { flagged.push(root.clone()); }
+            if seen.insert(key) {
+                flagged.push(root.clone());
+            }
         }
         // one level down best-effort
         if let Ok(read) = std::fs::read_dir(&root) {
@@ -240,13 +246,17 @@ pub fn audit_everyone_writable(
                 // Skip noisy/irrelevant Windows system subdirectories
                 let pl = p.to_string_lossy().to_ascii_lowercase();
                 let norm = pl.replace('\\', "/");
-                if SKIP_DIR_SUFFIXES.iter().any(|s| norm.ends_with(s)) { continue; }
+                if SKIP_DIR_SUFFIXES.iter().any(|s| norm.ends_with(s)) {
+                    continue;
+                }
                 if ft.is_dir() {
                     checked += 1;
                     let has_child = unsafe { path_has_world_write_allow(&p)? };
                     if has_child {
                         let key = normalize_path_key(&p);
-                        if seen.insert(key) { flagged.push(p); }
+                        if seen.insert(key) {
+                            flagged.push(p);
+                        }
                     }
                 }
             }
@@ -258,20 +268,12 @@ pub fn audit_everyone_writable(
         for p in &flagged {
             list.push_str(&format!("\n - {}", p.display()));
         }
-        crate::logging::log_note(
-            &format!(
-                "AUDIT: world-writable scan FAILED; checked={checked}; duration_ms={elapsed_ms}; flagged:{}",
-                list
-            ),
-            logs_base_dir,
-        );
+
         return Ok(flagged);
     }
     // Log success once if nothing flagged
     crate::logging::log_note(
-        &format!(
-            "AUDIT: world-writable scan OK; checked={checked}; duration_ms={elapsed_ms}"
-        ),
+        &format!("AUDIT: world-writable scan OK; checked={checked}; duration_ms={elapsed_ms}"),
         logs_base_dir,
     );
     Ok(Vec::new())
@@ -284,14 +286,10 @@ fn normalize_windows_path_for_display(p: impl AsRef<Path>) -> String {
 
 pub fn world_writable_warning_details(
     codex_home: impl AsRef<Path>,
+    cwd: impl AsRef<Path>,
 ) -> Option<(Vec<String>, usize, bool)> {
-    let cwd = match std::env::current_dir() {
-        Ok(cwd) => cwd,
-        Err(_) => return Some((Vec::new(), 0, true)),
-    };
-
     let env_map: HashMap<String, String> = std::env::vars().collect();
-    match audit_everyone_writable(&cwd, &env_map, Some(codex_home.as_ref())) {
+    match audit_everyone_writable(cwd.as_ref(), &env_map, Some(codex_home.as_ref())) {
         Ok(paths) if paths.is_empty() => None,
         Ok(paths) => {
             let as_strings: Vec<String> = paths
@@ -329,16 +327,16 @@ unsafe fn dacl_quick_world_write_mask_allows(p_dacl: *mut ACL, psid_world: *mut
             continue;
         }
         let hdr = &*(p_ace as *const ACE_HEADER);
-        if hdr.AceType != 0 { // ACCESS_ALLOWED_ACE_TYPE
+        if hdr.AceType != 0 {
+            // ACCESS_ALLOWED_ACE_TYPE
             continue;
         }
         if (hdr.AceFlags & INHERIT_ONLY_ACE) != 0 {
             continue;
         }
         let base = p_ace as usize;
-        let sid_ptr = (base
-            + std::mem::size_of::<ACE_HEADER>()
-            + std::mem::size_of::<u32>()) as *mut c_void; // skip header + mask
+        let sid_ptr =
+            (base + std::mem::size_of::<ACE_HEADER>() + std::mem::size_of::<u32>()) as *mut c_void; // skip header + mask
         if EqualSid(sid_ptr, psid_world) != 0 {
             let ace = &*(p_ace as *const ACCESS_ALLOWED_ACE);
             let mask = ace.Mask;
diff --git a/codex-rs/windows-sandbox-rs/src/lib.rs b/codex-rs/windows-sandbox-rs/src/lib.rs
index 9d5e9aeae..a2b4b6a53 100644
--- a/codex-rs/windows-sandbox-rs/src/lib.rs
+++ b/codex-rs/windows-sandbox-rs/src/lib.rs
@@ -467,6 +467,7 @@ mod stub {
 
     pub fn world_writable_warning_details(
         _codex_home: impl AsRef<Path>,
+        _cwd: impl AsRef<Path>,
     ) -> Option<(Vec<String>, usize, bool)> {
         None
     }

```
