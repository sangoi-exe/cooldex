use super::PreviousSectionState;
use super::WorldStateHash;
use super::WorldStateSection;
use super::multi_agent_usage_hint::MultiAgentUsageHintState;
use crate::context::ContextualUserFragment;
use crate::context::multi_agent_mode_instructions::MultiAgentModeInstructions;
use crate::context::multi_agent_mode_instructions::bounded_mode_and_explanation;
use codex_protocol::config_types::MultiAgentMode;
use serde::Deserialize;
use serde::Serialize;

/// Effective built-in policy and optional explanatory text for one V2 turn.
pub(crate) struct EffectiveMultiAgentMode {
    mode: MultiAgentMode,
    explanation: Option<String>,
}

impl EffectiveMultiAgentMode {
    pub(crate) fn new(mode: MultiAgentMode, explanation: Option<String>) -> Self {
        Self { mode, explanation }
    }
}

/// Effective multi-agent mode currently visible to the model.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct MultiAgentModeState {
    mode: Option<MultiAgentMode>,
    explanation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    usage_hint_hash: Option<WorldStateHash>,
}

impl MultiAgentModeState {
    pub(crate) fn new(effective: Option<EffectiveMultiAgentMode>) -> Self {
        let Some(effective) = effective else {
            return Self {
                mode: None,
                explanation: None,
                usage_hint_hash: None,
            };
        };
        let (mode, explanation) =
            bounded_mode_and_explanation(effective.mode, effective.explanation.as_deref());
        Self {
            mode: Some(mode),
            explanation,
            usage_hint_hash: None,
        }
    }

    pub(crate) fn with_usage_hint(mut self, usage_hint: &MultiAgentUsageHintState) -> Self {
        self.usage_hint_hash = Some(usage_hint.snapshot());
        self
    }
}

impl WorldStateSection for MultiAgentModeState {
    const ID: &'static str = "multi_agent_mode";
    type Snapshot = Self;

    fn snapshot(&self) -> Self::Snapshot {
        self.clone()
    }

    fn matches_legacy_fragment(role: &str, text: &str) -> bool {
        role == "developer" && MultiAgentModeInstructions::matches_text(text)
    }

    fn has_retained_fragment_matcher() -> bool {
        true
    }

    fn matches_retained_fragment(role: &str, text: &str) -> bool {
        Self::matches_legacy_fragment(role, text)
    }

    fn render_diff(
        &self,
        previous: PreviousSectionState<'_, Self::Snapshot>,
    ) -> Option<Box<dyn ContextualUserFragment>> {
        let (mode, explanation) = match (&self.mode, previous) {
            (Some(mode), PreviousSectionState::Known(previous))
                if previous.mode.as_ref() == Some(mode)
                    && previous.explanation == self.explanation
                    && previous.usage_hint_hash == self.usage_hint_hash =>
            {
                return None;
            }
            (Some(mode), _) => (mode.clone(), self.explanation.clone()),
            (None, PreviousSectionState::Known(previous))
                if previous.mode == Some(MultiAgentMode::Proactive) =>
            {
                (MultiAgentMode::ExplicitRequestOnly, None)
            }
            (None, PreviousSectionState::Unknown) => (MultiAgentMode::ExplicitRequestOnly, None),
            (None, PreviousSectionState::Absent | PreviousSectionState::Known(_)) => return None,
        };

        MultiAgentModeInstructions::new(mode, explanation.as_deref())
            .map(|instructions| Box::new(instructions) as Box<dyn ContextualUserFragment>)
    }
}

#[cfg(test)]
#[path = "multi_agent_mode_tests.rs"]
mod tests;
