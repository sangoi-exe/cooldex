use crate::agent::types::ResolvedMultiAgentV2UsageHints;
use crate::config::MultiAgentV2Config;
use crate::context::ContextualUserFragment;
use crate::context::MultiAgentRoleInstructions;
use crate::context::world_state::EffectiveMultiAgentMode;
use crate::session::step_context::StepContext;
use crate::session::turn_context::TurnContext;
use codex_features::MultiAgentV2Policy;
use codex_prompts::ResolvedMessage;
use codex_prompts::ResolvedModelMessages;
use codex_prompts::ResolvedMultiAgentMessages;
use codex_protocol::config_types::MultiAgentMode;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::protocol::AgentUsageHintBinding;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;

pub(crate) const MAX_MULTI_AGENT_USAGE_HINT_TOKENS: usize = 10_000;

// Merge-safety anchor: full-history V2 identity capture consumes this exact step snapshot so
// active-only settings cannot be replaced by future thread defaults or a later catalog lookup.
/// Uses the step's captured model messages for model-context assembly.
pub(super) fn usage_hint_text(
    step_context: &StepContext,
) -> CodexResult<Option<MultiAgentRoleInstructions>> {
    let turn_context = step_context.turn.as_ref();
    usage_hint_text_for_source(
        turn_context,
        &turn_context.session_source,
        ResolvedModelMessages::from_model(&step_context.settings.model_info).multi_agent(),
    )
}

/// Captures the invoking step's hint identity for a full-history child. Spawn admission validates
/// the resulting binding before it can be persisted.
pub(super) fn usage_hint_text_for_identity_snapshot(
    step_context: &StepContext,
) -> Option<MultiAgentRoleInstructions> {
    let turn_context = step_context.turn.as_ref();
    usage_hint_text_for_source_unchecked(
        turn_context,
        &turn_context.session_source,
        ResolvedModelMessages::from_model(&step_context.settings.model_info).multi_agent(),
    )
}

/// Uses the turn's frozen model identity only when a direct AgentControl caller has no invoking
/// step context. Public V2 spawn uses `usage_hint_text` above.
pub(crate) fn usage_hint_text_for_turn(
    turn_context: &TurnContext,
) -> CodexResult<Option<MultiAgentRoleInstructions>> {
    usage_hint_text_for_source(
        turn_context,
        &turn_context.session_source,
        ResolvedModelMessages::from_model(turn_context.model_info()).multi_agent(),
    )
}

// Merge-safety anchor: V2 usage hints retain source filtering and captured-binding precedence;
// inherited text/marker state is never re-resolved through mutable config or catalog data.
fn usage_hint_text_for_source(
    turn_context: &TurnContext,
    session_source: &SessionSource,
    multi_agent_messages: ResolvedMultiAgentMessages<'_>,
) -> CodexResult<Option<MultiAgentRoleInstructions>> {
    if turn_context.multi_agent_version != MultiAgentVersion::V2 {
        return Ok(None);
    }

    let subagent = match session_source {
        SessionSource::SubAgent(SubAgentSource::ThreadSpawn { .. }) => true,
        SessionSource::Cli
        | SessionSource::VSCode
        | SessionSource::Exec
        | SessionSource::Mcp
        | SessionSource::Custom(_)
        | SessionSource::Unknown => false,
        SessionSource::Internal(_) | SessionSource::SubAgent(_) => return Ok(None),
    };

    if let AgentUsageHintBinding::Inherited { instructions } =
        &turn_context.config.agent_usage_hint_binding
    {
        return instructions
            .clone()
            .map(MultiAgentRoleInstructions::from_agent_usage_hint_instructions)
            .map(|instructions| {
                validate_usage_hint_rendered("agent_usage_hint_binding", &instructions.render())?;
                Ok(instructions)
            })
            .transpose();
    }

    let snapshot = resolve_usage_hints(
        &turn_context.config.multi_agent_v2,
        multi_agent_messages,
        !turn_context.config.update_plan_enabled && turn_context.config.model_catalog.is_none(),
    )?;
    Ok(if subagent {
        snapshot.subagent
    } else {
        snapshot.root
    })
}

fn usage_hint_text_for_source_unchecked(
    turn_context: &TurnContext,
    session_source: &SessionSource,
    multi_agent_messages: ResolvedMultiAgentMessages<'_>,
) -> Option<MultiAgentRoleInstructions> {
    if turn_context.multi_agent_version != MultiAgentVersion::V2 {
        return None;
    }

    let subagent = match session_source {
        SessionSource::SubAgent(SubAgentSource::ThreadSpawn { .. }) => true,
        SessionSource::Cli
        | SessionSource::VSCode
        | SessionSource::Exec
        | SessionSource::Mcp
        | SessionSource::Custom(_)
        | SessionSource::Unknown => false,
        SessionSource::Internal(_) | SessionSource::SubAgent(_) => return None,
    };

    if let AgentUsageHintBinding::Inherited { instructions } =
        &turn_context.config.agent_usage_hint_binding
    {
        return instructions
            .clone()
            .map(MultiAgentRoleInstructions::from_agent_usage_hint_instructions);
    }

    let snapshot = resolve_usage_hints_unchecked(
        &turn_context.config.multi_agent_v2,
        multi_agent_messages,
        !turn_context.config.update_plan_enabled && turn_context.config.model_catalog.is_none(),
    );
    if subagent {
        snapshot.subagent
    } else {
        snapshot.root
    }
}

// Merge-safety anchor: full-history capture freezes the typed effective hint at the parent turn;
// later child turns must use this binding rather than resolve current config or model catalog data.
pub(crate) fn full_history_usage_hint_binding(
    turn_context: &TurnContext,
) -> CodexResult<AgentUsageHintBinding> {
    Ok(AgentUsageHintBinding::Inherited {
        instructions: usage_hint_text_for_turn(turn_context)?
            .map(MultiAgentRoleInstructions::into_agent_usage_hint_instructions),
    })
}

pub(crate) fn resolve_usage_hints(
    config: &MultiAgentV2Config,
    multi_agent_messages: ResolvedMultiAgentMessages<'_>,
    omit_update_plan_instructions: bool,
) -> CodexResult<ResolvedMultiAgentV2UsageHints> {
    let resolve_role = |configured: Option<&str>,
                        configured_source: &str,
                        message: ResolvedMessage<'_>,
                        message_source: &str|
     -> CodexResult<Option<MultiAgentRoleInstructions>> {
        // Configured roles take precedence; empty configured or catalog roles suppress fallback.
        if let Some(configured) = configured {
            let instructions = (!configured.is_empty())
                .then(|| MultiAgentRoleInstructions::Configured(configured.to_owned()));
            return instructions
                .map(|instructions| {
                    validate_usage_hint_rendered(configured_source, &instructions.render())?;
                    Ok(instructions)
                })
                .transpose();
        }

        let base = message.text();
        if base.is_empty() {
            return Ok(None);
        }
        let instructions = MultiAgentRoleInstructions::Composed {
            base: base.to_owned(),
            marked: message.catalog_override().is_some(),
            omit_update_plan_instructions,
            max_concurrency: config.max_concurrent_threads_per_session,
            wait_agent_enabled: config.wait_agent_enabled,
            expose_model_overrides: config.expose_spawn_agent_model_overrides,
        };
        validate_usage_hint_rendered(message_source, &instructions.render())?;
        Ok(Some(instructions))
    };

    Ok(ResolvedMultiAgentV2UsageHints {
        root: resolve_role(
            config.root_agent_usage_hint_text.as_deref(),
            "features.multi_agent_v2.root_agent_usage_hint_text",
            multi_agent_messages.root,
            "model multi-agent root usage hint",
        )?,
        subagent: resolve_role(
            config.subagent_usage_hint_text.as_deref(),
            "features.multi_agent_v2.subagent_usage_hint_text",
            multi_agent_messages.subagent,
            "model multi-agent subagent usage hint",
        )?,
    })
}

fn resolve_usage_hints_unchecked(
    config: &MultiAgentV2Config,
    multi_agent_messages: ResolvedMultiAgentMessages<'_>,
    omit_update_plan_instructions: bool,
) -> ResolvedMultiAgentV2UsageHints {
    let resolve_role = |configured: Option<&str>, message: ResolvedMessage<'_>| {
        if let Some(configured) = configured {
            return (!configured.is_empty())
                .then(|| MultiAgentRoleInstructions::Configured(configured.to_owned()));
        }

        let base = message.text();
        (!base.is_empty()).then(|| MultiAgentRoleInstructions::Composed {
            base: base.to_owned(),
            marked: message.catalog_override().is_some(),
            omit_update_plan_instructions,
            max_concurrency: config.max_concurrent_threads_per_session,
            wait_agent_enabled: config.wait_agent_enabled,
            expose_model_overrides: config.expose_spawn_agent_model_overrides,
        })
    };

    ResolvedMultiAgentV2UsageHints {
        root: resolve_role(
            config.root_agent_usage_hint_text.as_deref(),
            multi_agent_messages.root,
        ),
        subagent: resolve_role(
            config.subagent_usage_hint_text.as_deref(),
            multi_agent_messages.subagent,
        ),
    }
}

pub(crate) fn validate_usage_hint_binding(
    binding: &AgentUsageHintBinding,
    source: &str,
) -> CodexResult<()> {
    let AgentUsageHintBinding::Inherited {
        instructions: Some(instructions),
    } = binding
    else {
        return Ok(());
    };
    let instructions =
        MultiAgentRoleInstructions::from_agent_usage_hint_instructions(instructions.clone());
    validate_usage_hint_rendered(source, &instructions.render())
}

pub(crate) fn validate_usage_hint_rendered(source: &str, rendered: &str) -> CodexResult<()> {
    let estimated_tokens = codex_utils_string::approx_token_count(rendered);
    if estimated_tokens > MAX_MULTI_AGENT_USAGE_HINT_TOKENS {
        return Err(CodexErr::InvalidRequest(format!(
            "{source} exceeds the {MAX_MULTI_AGENT_USAGE_HINT_TOKENS}-token limit after rendering ({estimated_tokens} estimated tokens)"
        )));
    }
    Ok(())
}

// Merge-safety anchor: effective V2 mode follows explicit config policy, never reasoning effort,
// and remains absent for internal or non-thread-spawn subagent sources.
pub(crate) fn effective_multi_agent_mode(
    step_context: &StepContext,
) -> Option<EffectiveMultiAgentMode> {
    let turn_context = step_context.turn.as_ref();
    if turn_context.multi_agent_version != MultiAgentVersion::V2 {
        return None;
    }

    match &turn_context.session_source {
        SessionSource::SubAgent(SubAgentSource::ThreadSpawn { .. })
        | SessionSource::Cli
        | SessionSource::VSCode
        | SessionSource::Exec
        | SessionSource::Mcp
        | SessionSource::Custom(_)
        | SessionSource::Unknown => {}
        SessionSource::Internal(_) | SessionSource::SubAgent(_) => return None,
    }

    let messages =
        ResolvedModelMessages::from_model(&step_context.settings.model_info).multi_agent();
    let config = &turn_context.config.multi_agent_v2;
    let explanation = config
        .multi_agent_mode_hint_text
        .clone()
        .or_else(|| messages.hint.map(str::to_owned))
        .or_else(|| match config.policy {
            MultiAgentV2Policy::ExplicitRequestOnly => {
                messages.explicit.catalog_override().map(str::to_owned)
            }
            MultiAgentV2Policy::Proactive => {
                messages.proactive.catalog_override().map(str::to_owned)
            }
        });
    let mode = match config.policy {
        MultiAgentV2Policy::ExplicitRequestOnly => MultiAgentMode::ExplicitRequestOnly,
        MultiAgentV2Policy::Proactive => MultiAgentMode::Proactive,
    };
    Some(EffectiveMultiAgentMode::new(mode, explanation))
}
