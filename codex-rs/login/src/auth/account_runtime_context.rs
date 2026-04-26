// Merge-safety anchor: AccountManager owns the runtime account context that
// couples forced workspace selection with the linked Codex session lease.
// AuthManager may pass these inputs in, but must not become their owner.

pub(super) struct AccountRuntimeContext {
    pub(super) linked_codex_session_id: Option<String>,
    pub(super) forced_chatgpt_workspace_id: Option<String>,
}
