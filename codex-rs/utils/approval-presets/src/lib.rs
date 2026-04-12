use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::SandboxPolicy;

/// A UI-agnostic preset pairing approval, reviewer, and sandbox state.
#[derive(Debug, Clone)]
pub struct ApprovalPreset {
    /// Stable identifier for the preset.
    pub id: &'static str,
    /// Display label shown in UIs.
    pub label: &'static str,
    /// Short human description shown next to the label in UIs.
    pub description: &'static str,
    /// Approval policy to apply.
    pub approval: AskForApproval,
    /// Approval reviewer to apply.
    pub approvals_reviewer: ApprovalsReviewer,
    /// Sandbox policy to apply.
    pub sandbox: SandboxPolicy,
}

/// Built-in list of approval presets that pair approval, reviewer, and sandbox policy.
///
/// Keep this UI-agnostic so it can be reused by both TUI and MCP server.
pub fn builtin_approval_presets() -> Vec<ApprovalPreset> {
    vec![
        ApprovalPreset {
            id: "read-only",
            label: "Read Only",
            description: "Codex can read files in the current workspace. Approval is required to edit files or access the internet.",
            approval: AskForApproval::OnRequest,
            approvals_reviewer: ApprovalsReviewer::User,
            sandbox: SandboxPolicy::new_read_only_policy(),
        },
        ApprovalPreset {
            id: "auto",
            label: "Default",
            description: "Codex can read and edit files in the current workspace, and run commands. Approval is required to access the internet or edit other files. (Identical to Agent mode)",
            approval: AskForApproval::OnRequest,
            approvals_reviewer: ApprovalsReviewer::User,
            sandbox: SandboxPolicy::new_workspace_write_policy(),
        },
        guardian_approval_preset(),
        ApprovalPreset {
            id: "full-access",
            label: "Full Access",
            description: "Codex can edit files outside this workspace and access the internet without asking for approval. Exercise caution when using.",
            approval: AskForApproval::Never,
            approvals_reviewer: ApprovalsReviewer::User,
            sandbox: SandboxPolicy::DangerFullAccess,
        },
    ]
}

/// Built-in Guardian Approvals preset.
pub fn guardian_approval_preset() -> ApprovalPreset {
    // Merge-safety anchor: Guardian Approvals is a shared preset owner, not a TUI-local pseudo-row.
    ApprovalPreset {
        id: "guardian-auto",
        label: "Guardian Approvals",
        description: "Same workspace-write permissions as Default, but eligible `on-request` approvals are routed through the guardian reviewer subagent.",
        approval: AskForApproval::OnRequest,
        approvals_reviewer: ApprovalsReviewer::GuardianSubagent,
        sandbox: SandboxPolicy::new_workspace_write_policy(),
    }
}
