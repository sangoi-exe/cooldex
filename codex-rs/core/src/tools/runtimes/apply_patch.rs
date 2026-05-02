//! Apply Patch runtime: executes verified patches under the orchestrator.
//!
//! Assumes `apply_patch` verification/approval happened upstream. Reuses that
//! decision to avoid re-prompting, applies through the remote filesystem when
//! the turn uses a remote environment, or builds the self-invocation command
//! for `codex --codex-run-as-apply-patch` and runs it under the current
//! `SandboxAttempt` with a minimal environment for local turns.
// Merge-safety anchor: remote apply_patch execution must mutate only the injected
// executor filesystem; local self-invocation remains the host-local atomic owner.
use crate::exec::ExecCapturePolicy;
use crate::guardian::GuardianApprovalRequest;
use crate::guardian::review_approval_request;
use crate::sandboxing::ExecOptions;
use crate::sandboxing::execute_env;
use crate::tools::hook_names::HookToolName;
use crate::tools::sandboxing::Approvable;
use crate::tools::sandboxing::ApprovalCtx;
use crate::tools::sandboxing::ExecApprovalRequirement;
use crate::tools::sandboxing::PermissionRequestPayload;
use crate::tools::sandboxing::SandboxAttempt;
use crate::tools::sandboxing::Sandboxable;
use crate::tools::sandboxing::ToolCtx;
use crate::tools::sandboxing::ToolError;
use crate::tools::sandboxing::ToolRuntime;
use crate::tools::sandboxing::with_cached_approval;
use codex_apply_patch::ApplyPatchAction;
use codex_apply_patch::CODEX_CORE_APPLY_PATCH_ARG1;
use codex_exec_server::ExecutorFileSystem;
use codex_exec_server::FileSystemSandboxContext;
use codex_protocol::exec_output::ExecToolCallOutput;
use codex_protocol::exec_output::StreamOutput;
use codex_protocol::models::AdditionalPermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::FileChange;
use codex_protocol::protocol::ReviewDecision;
use codex_sandboxing::SandboxCommand;
use codex_sandboxing::SandboxType;
use codex_sandboxing::SandboxablePreference;
use codex_sandboxing::policy_transforms::effective_permission_profile;
use codex_utils_absolute_path::AbsolutePathBuf;
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

#[derive(Debug)]
pub struct ApplyPatchRequest {
    pub action: ApplyPatchAction,
    pub file_paths: Vec<AbsolutePathBuf>,
    pub changes: std::collections::HashMap<PathBuf, FileChange>,
    pub exec_approval_requirement: ExecApprovalRequirement,
    pub additional_permissions: Option<AdditionalPermissionProfile>,
    pub permissions_preapproved: bool,
}

#[derive(Default)]
pub struct ApplyPatchRuntime;

impl ApplyPatchRuntime {
    pub fn new() -> Self {
        Self
    }

    fn build_guardian_review_request(
        req: &ApplyPatchRequest,
        call_id: &str,
    ) -> GuardianApprovalRequest {
        GuardianApprovalRequest::ApplyPatch {
            id: call_id.to_string(),
            cwd: req.action.cwd.clone(),
            files: req.file_paths.clone(),
            patch: req.action.patch.clone(),
        }
    }

    #[cfg(target_os = "windows")]
    fn build_sandbox_command(
        req: &ApplyPatchRequest,
        codex_home: &std::path::Path,
    ) -> Result<SandboxCommand, ToolError> {
        Ok(Self::build_sandbox_command_with_program(
            req,
            codex_windows_sandbox::resolve_current_exe_for_launch(codex_home, "codex.exe"),
        ))
    }

    #[cfg(not(target_os = "windows"))]
    fn build_sandbox_command(
        req: &ApplyPatchRequest,
        codex_self_exe: Option<&PathBuf>,
    ) -> Result<SandboxCommand, ToolError> {
        let exe = Self::resolve_apply_patch_program(codex_self_exe)?;
        Ok(Self::build_sandbox_command_with_program(req, exe))
    }

    #[cfg(not(target_os = "windows"))]
    fn resolve_apply_patch_program(codex_self_exe: Option<&PathBuf>) -> Result<PathBuf, ToolError> {
        if let Some(path) = codex_self_exe {
            return Ok(path.clone());
        }

        std::env::current_exe()
            .map_err(|error| ToolError::Rejected(format!("failed to determine codex exe: {error}")))
    }

    fn build_sandbox_command_with_program(req: &ApplyPatchRequest, exe: PathBuf) -> SandboxCommand {
        SandboxCommand {
            program: exe.into_os_string(),
            args: vec![
                CODEX_CORE_APPLY_PATCH_ARG1.to_string(),
                req.action.patch.clone(),
            ],
            cwd: req.action.cwd.clone(),
            env: HashMap::new(),
            additional_permissions: req.additional_permissions.clone(),
        }
    }

    fn stdout_stream(ctx: &ToolCtx) -> Option<crate::exec::StdoutStream> {
        Some(crate::exec::StdoutStream {
            sub_id: ctx.turn.sub_id.clone(),
            call_id: ctx.call_id.clone(),
            tx_event: ctx.session.get_tx_event(),
        })
    }

    fn file_system_sandbox_context_for_attempt(
        req: &ApplyPatchRequest,
        attempt: &SandboxAttempt<'_>,
    ) -> Result<Option<FileSystemSandboxContext>, ToolError> {
        if attempt.sandbox == SandboxType::None {
            return Ok(None);
        }

        let permissions =
            effective_permission_profile(attempt.permissions, req.additional_permissions.as_ref());
        Ok(Some(FileSystemSandboxContext {
            permissions,
            cwd: Some(attempt.sandbox_cwd.clone()),
            windows_sandbox_level: attempt.windows_sandbox_level,
            windows_sandbox_private_desktop: attempt.windows_sandbox_private_desktop,
            use_legacy_landlock: attempt.use_legacy_landlock,
            additional_permissions: None,
        }))
    }

    async fn run_with_executor_filesystem(
        req: &ApplyPatchRequest,
        fs: &dyn ExecutorFileSystem,
        sandbox: Option<&FileSystemSandboxContext>,
    ) -> ExecToolCallOutput {
        let started_at = Instant::now();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = codex_apply_patch::apply_patch_via_executor_fs(
            &req.action.patch,
            &req.action.cwd,
            &mut stdout,
            &mut stderr,
            fs,
            sandbox,
        )
        .await;
        let stdout = String::from_utf8_lossy(&stdout).into_owned();
        let stderr = String::from_utf8_lossy(&stderr).into_owned();
        let exit_code = if result.is_ok() { 0 } else { 1 };
        ExecToolCallOutput {
            exit_code,
            stdout: StreamOutput::new(stdout.clone()),
            stderr: StreamOutput::new(stderr.clone()),
            aggregated_output: StreamOutput::new(format!("{stdout}{stderr}")),
            duration: started_at.elapsed(),
            timed_out: false,
        }
    }
}

impl Sandboxable for ApplyPatchRuntime {
    fn sandbox_preference(&self) -> SandboxablePreference {
        SandboxablePreference::Auto
    }
    fn escalate_on_failure(&self) -> bool {
        true
    }
}

impl Approvable<ApplyPatchRequest> for ApplyPatchRuntime {
    type ApprovalKey = AbsolutePathBuf;

    fn approval_keys(&self, req: &ApplyPatchRequest) -> Vec<Self::ApprovalKey> {
        req.file_paths.clone()
    }

    fn start_approval_async<'a>(
        &'a mut self,
        req: &'a ApplyPatchRequest,
        ctx: ApprovalCtx<'a>,
    ) -> BoxFuture<'a, ReviewDecision> {
        let session = ctx.session;
        let turn = ctx.turn;
        let call_id = ctx.call_id.to_string();
        let retry_reason = ctx.retry_reason.clone();
        let approval_keys = self.approval_keys(req);
        let changes = req.changes.clone();
        let guardian_review_id = ctx.guardian_review_id.clone();
        Box::pin(async move {
            if let Some(review_id) = guardian_review_id {
                let action = ApplyPatchRuntime::build_guardian_review_request(req, ctx.call_id);
                return review_approval_request(session, turn, review_id, action, retry_reason)
                    .await;
            }
            if req.permissions_preapproved && retry_reason.is_none() {
                return ReviewDecision::Approved;
            }
            if let Some(reason) = retry_reason {
                let rx_approve = session
                    .request_patch_approval(
                        turn,
                        call_id,
                        changes.clone(),
                        Some(reason),
                        /*grant_root*/ None,
                    )
                    .await;
                return rx_approve.await.unwrap_or_default();
            }

            with_cached_approval(
                &session.services,
                "apply_patch",
                approval_keys,
                || async move {
                    let rx_approve = session
                        .request_patch_approval(
                            turn, call_id, changes, /*reason*/ None, /*grant_root*/ None,
                        )
                        .await;
                    rx_approve.await.unwrap_or_default()
                },
            )
            .await
        })
    }

    fn wants_no_sandbox_approval(&self, policy: AskForApproval) -> bool {
        match policy {
            AskForApproval::Never => false,
            AskForApproval::Granular(granular_config) => granular_config.allows_sandbox_approval(),
            AskForApproval::OnFailure => true,
            AskForApproval::OnRequest => true,
            AskForApproval::UnlessTrusted => true,
        }
    }

    // apply_patch approvals are decided upstream by assess_patch_safety.
    //
    // This override ensures the orchestrator runs the patch approval flow when required instead
    // of falling back to the global exec approval policy.
    fn exec_approval_requirement(
        &self,
        req: &ApplyPatchRequest,
    ) -> Option<ExecApprovalRequirement> {
        Some(req.exec_approval_requirement.clone())
    }

    fn permission_request_payload(
        &self,
        req: &ApplyPatchRequest,
    ) -> Option<PermissionRequestPayload> {
        Some(PermissionRequestPayload {
            tool_name: HookToolName::apply_patch(),
            tool_input: serde_json::json!({ "command": req.action.patch }),
        })
    }
}

impl ToolRuntime<ApplyPatchRequest, ExecToolCallOutput> for ApplyPatchRuntime {
    async fn run(
        &mut self,
        req: &ApplyPatchRequest,
        attempt: &SandboxAttempt<'_>,
        ctx: &ToolCtx,
    ) -> Result<ExecToolCallOutput, ToolError> {
        let environment = ctx.turn.environment.as_ref().ok_or_else(|| {
            ToolError::Rejected("apply_patch is unavailable in this session".to_string())
        })?;
        if environment.is_remote() {
            let fs = environment.get_filesystem();
            let sandbox = Self::file_system_sandbox_context_for_attempt(req, attempt)?;
            return Ok(
                Self::run_with_executor_filesystem(req, fs.as_ref(), sandbox.as_ref()).await,
            );
        }

        #[cfg(target_os = "windows")]
        let command = Self::build_sandbox_command(req, &ctx.turn.config.codex_home)?;
        #[cfg(not(target_os = "windows"))]
        let command = Self::build_sandbox_command(req, ctx.turn.codex_self_exe.as_ref())?;
        let options = ExecOptions {
            expiration: None.into(),
            capture_policy: ExecCapturePolicy::ShellTool,
        };
        let env = attempt
            .env_for(command, options, /*network*/ None)
            .map_err(|err| ToolError::Codex(err.into()))?;
        let out = execute_env(env, Self::stdout_stream(ctx))
            .await
            .map_err(ToolError::Codex)?;
        Ok(out)
    }
}

#[cfg(test)]
#[path = "apply_patch_tests.rs"]
mod tests;
