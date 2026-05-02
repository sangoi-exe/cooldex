use codex_protocol::request_permissions::RequestPermissionsArgs;
use codex_sandboxing::policy_transforms::normalize_additional_permissions;

use crate::function_tool::FunctionCallError;
use crate::subagent_file_mutation::denied_action_message;
use crate::subagent_file_mutation::file_mutation_is_denied;
use crate::subagent_file_mutation::request_permission_profile_requests_file_system_write;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::parse_arguments_with_base_path;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

pub struct RequestPermissionsHandler;

impl ToolHandler for RequestPermissionsHandler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            cancellation_token,
            call_id,
            payload,
            ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "request_permissions handler received unsupported payload".to_string(),
                ));
            }
        };

        let mut args: RequestPermissionsArgs =
            parse_arguments_with_base_path(&arguments, &turn.cwd)?;
        args.permissions = normalize_additional_permissions(args.permissions.into())
            .map(codex_protocol::request_permissions::RequestPermissionProfile::from)
            .map_err(FunctionCallError::RespondToModel)?;
        if args.permissions.is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "request_permissions requires at least one permission".to_string(),
            ));
        }
        // Merge-safety anchor: spawn-only file-mutation denial must reject filesystem write escalations through request_permissions.
        if file_mutation_is_denied(turn.config.as_ref())
            && request_permission_profile_requests_file_system_write(&args.permissions)
        {
            return Err(FunctionCallError::RespondToModel(denied_action_message(
                "this subagent cannot request filesystem write permissions",
            )));
        }

        let response = session
            .request_permissions(&turn, call_id, args, cancellation_token)
            .await
            .map_err(FunctionCallError::RespondToModel)?;

        let content = serde_json::to_string(&response).map_err(|err| {
            FunctionCallError::Fatal(format!(
                "failed to serialize request_permissions response: {err}"
            ))
        })?;

        Ok(FunctionToolOutput::from_text(content, Some(true)))
    }
}
