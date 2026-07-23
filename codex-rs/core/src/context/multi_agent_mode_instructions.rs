use super::ContextualUserFragment;
use codex_protocol::config_types::MultiAgentMode;
use codex_protocol::protocol::MULTI_AGENT_MODE_CLOSE_TAG;
use codex_protocol::protocol::MULTI_AGENT_MODE_OPEN_TAG;
use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::approx_bytes_for_tokens;
use codex_utils_output_truncation::truncate_text;

const EXPLICIT_REQUEST_ONLY_MULTI_AGENT_MODE_TEXT: &str = "Any earlier instruction enabling proactive multi-agent delegation no longer applies. Do not spawn sub-agents unless the user or applicable AGENTS.md/skill instructions explicitly ask for sub-agents, delegation, or parallel agent work.";
const PROACTIVE_MULTI_AGENT_MODE_TEXT: &str = "Proactive multi-agent delegation is active. Any earlier instruction requiring an explicit user request before spawning sub-agents no longer applies. Use sub-agents when parallel work would materially improve speed or quality. This mode remains active until a later multi-agent mode developer message changes it.";
const MULTI_AGENT_MODE_MAX_TOKENS: usize = 400;
const EXPLANATION_SEPARATOR: &str = "\n\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MultiAgentModeInstructions {
    multi_agent_mode: MultiAgentMode,
    explanation: Option<String>,
}

impl MultiAgentModeInstructions {
    pub(super) fn new(multi_agent_mode: MultiAgentMode, explanation: Option<&str>) -> Option<Self> {
        let (multi_agent_mode, explanation) =
            bounded_mode_and_explanation(multi_agent_mode, explanation);
        if matches!(
            &multi_agent_mode,
            MultiAgentMode::Custom(hint_text) if hint_text.is_empty()
        ) {
            return None;
        }

        Some(Self {
            multi_agent_mode,
            explanation,
        })
    }
}

pub(super) fn bounded_mode_and_explanation(
    multi_agent_mode: MultiAgentMode,
    explanation: Option<&str>,
) -> (MultiAgentMode, Option<String>) {
    let max_bytes = approx_bytes_for_tokens(MULTI_AGENT_MODE_MAX_TOKENS)
        .saturating_sub(MULTI_AGENT_MODE_OPEN_TAG.len())
        .saturating_sub(MULTI_AGENT_MODE_CLOSE_TAG.len());
    match multi_agent_mode {
        MultiAgentMode::Custom(hint_text) => (
            MultiAgentMode::Custom(bound_text(hint_text.as_str(), max_bytes)),
            None,
        ),
        mode @ (MultiAgentMode::ExplicitRequestOnly | MultiAgentMode::Proactive) => {
            let base_text = built_in_mode_text(&mode);
            let explanation_max_bytes = max_bytes
                .saturating_sub(base_text.len())
                .saturating_sub(EXPLANATION_SEPARATOR.len());
            let explanation = explanation
                .map(str::trim)
                .filter(|explanation| !explanation.is_empty())
                .map(|explanation| bound_text(explanation, explanation_max_bytes))
                .filter(|explanation| !explanation.is_empty());
            (mode, explanation)
        }
    }
}

fn bound_text(text: &str, max_bytes: usize) -> String {
    let mut bounded = truncate_text(text, TruncationPolicy::Bytes(max_bytes));
    while bounded.len() > max_bytes {
        bounded.pop();
    }
    bounded
}

fn built_in_mode_text(multi_agent_mode: &MultiAgentMode) -> &'static str {
    match multi_agent_mode {
        MultiAgentMode::ExplicitRequestOnly => EXPLICIT_REQUEST_ONLY_MULTI_AGENT_MODE_TEXT,
        MultiAgentMode::Proactive => PROACTIVE_MULTI_AGENT_MODE_TEXT,
        MultiAgentMode::Custom(_) => unreachable!("custom mode has no built-in instructions"),
    }
}

impl ContextualUserFragment for MultiAgentModeInstructions {
    fn role(&self) -> &'static str {
        "developer"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        (MULTI_AGENT_MODE_OPEN_TAG, MULTI_AGENT_MODE_CLOSE_TAG)
    }

    fn body(&self) -> String {
        match &self.multi_agent_mode {
            MultiAgentMode::Custom(hint_text) => hint_text.clone(),
            mode @ (MultiAgentMode::ExplicitRequestOnly | MultiAgentMode::Proactive) => {
                let base_text = built_in_mode_text(mode);
                match self.explanation.as_deref() {
                    Some(explanation) => {
                        format!("{base_text}{EXPLANATION_SEPARATOR}{explanation}")
                    }
                    None => base_text.to_string(),
                }
            }
        }
    }
}
