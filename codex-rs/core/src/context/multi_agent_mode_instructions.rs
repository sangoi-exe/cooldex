use super::ContextualUserFragment;
use codex_prompts::ResolvedModelMessages;
use codex_protocol::config_types::MultiAgentMode;
use codex_protocol::models::ContentItemKind;
use codex_protocol::protocol::MULTI_AGENT_MODE_CLOSE_TAG;
use codex_protocol::protocol::MULTI_AGENT_MODE_OPEN_TAG;
use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::approx_bytes_for_tokens;
use codex_utils_output_truncation::truncate_text;

const MULTI_AGENT_MODE_MAX_TOKENS: usize = 400;
const EXPLANATION_SEPARATOR: &str = "\n\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MultiAgentModeInstructions {
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

// Merge-safety anchor: built-in/custom mode instructions and optional explanation stay inside one
// tagged 400-token context budget.
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

fn built_in_mode_text(multi_agent_mode: &MultiAgentMode) -> String {
    let bundled = ResolvedModelMessages::bundled().multi_agent();
    match multi_agent_mode {
        MultiAgentMode::ExplicitRequestOnly => bundled.explicit.text().to_owned(),
        MultiAgentMode::Proactive => bundled.proactive.text().to_owned(),
        MultiAgentMode::Custom(_) => unreachable!("custom mode has no built-in instructions"),
    }
}

impl ContextualUserFragment for MultiAgentModeInstructions {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("multi_agent.mode_instructions".to_string())
    }

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
        // `effective_multi_agent_mode` selects the policy-owned bundled text.
        // Configured and catalog text is carried separately as a bounded explanation.
        match &self.multi_agent_mode {
            MultiAgentMode::Custom(hint_text) => hint_text.clone(),
            mode @ (MultiAgentMode::ExplicitRequestOnly | MultiAgentMode::Proactive) => {
                let base_text = built_in_mode_text(mode);
                match self.explanation.as_deref() {
                    Some(explanation) => format!("{base_text}{EXPLANATION_SEPARATOR}{explanation}"),
                    None => base_text,
                }
            }
        }
    }
}
