use std::sync::Arc;

use crate::bottom_pane::ChatGptAddAccountSharedState;
use crate::bottom_pane::FeedbackAudience;
use crate::status::StatusAccountDisplay;
use codex_protocol::account::PlanType;
use codex_protocol::openai_models::ModelPreset;

// Merge-safety anchor: remote app-server account projection owns visible TUI
// account state in remote mode; keep these owner/follower types out of app.rs so
// new account runtime work does not keep growing the central dispatcher.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LiveAccountStateOwner {
    AppServerProjection,
    AuthManager,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AccountProjectionRefreshTrigger {
    AccountUpdate,
    AuthTokenRefresh,
    ManualSetActiveAccount,
    ManualAddAccount,
    #[cfg(test)]
    ManualRemoveActiveAccount,
    #[cfg(test)]
    ManualRemoveLastAccount,
}

impl AccountProjectionRefreshTrigger {
    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::AccountUpdate => "account update",
            Self::AuthTokenRefresh => "auth token refresh",
            Self::ManualSetActiveAccount => "manual account selection",
            Self::ManualAddAccount => "adding ChatGPT account",
            #[cfg(test)]
            Self::ManualRemoveActiveAccount => "removing the active account",
            #[cfg(test)]
            Self::ManualRemoveLastAccount => "removing the last account",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct VisibleAccountProjectionFollowers {
    pub(crate) active_store_account_id: Option<String>,
    pub(crate) status_account_display: Option<StatusAccountDisplay>,
    pub(crate) plan_type: Option<PlanType>,
    pub(crate) has_chatgpt_account: bool,
    pub(crate) feedback_audience: FeedbackAudience,
    pub(crate) default_model: String,
    pub(crate) available_models: Vec<ModelPreset>,
}

pub(crate) struct PendingRemoteChatGptAddAccount {
    pub(crate) login_id: String,
    pub(crate) shared_state: Arc<ChatGptAddAccountSharedState>,
}

pub(crate) struct PendingLocalChatGptAddAccountCompletion {
    pub(crate) projection_request_id: u64,
    pub(crate) shared_state: Arc<ChatGptAddAccountSharedState>,
    pub(crate) active_account_display: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AccountProjectionRefreshExpectation {
    AcceptBaselineAfterRetries,
    RequireChangeFromBaseline,
}
