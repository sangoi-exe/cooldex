use super::ContextualUserFragment;
use codex_protocol::models::ContentItemKind;
use codex_protocol::protocol::AgentUsageHintInstructions;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MultiAgentRoleInstructions {
    text: String,
    marked: bool,
}

impl MultiAgentRoleInstructions {
    pub(crate) fn unmarked(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            marked: false,
        }
    }

    pub(crate) fn catalog(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            marked: true,
        }
    }

    // Merge-safety anchor: durable full-history hint identity crosses the protocol boundary as
    // raw text plus marker state; it must never recover either property by parsing rendered text.
    pub(crate) fn from_agent_usage_hint_instructions(
        AgentUsageHintInstructions { text, marked }: AgentUsageHintInstructions,
    ) -> Self {
        Self { text, marked }
    }

    pub(crate) fn into_agent_usage_hint_instructions(self) -> AgentUsageHintInstructions {
        AgentUsageHintInstructions {
            text: self.text,
            marked: self.marked,
        }
    }
}

impl ContextualUserFragment for MultiAgentRoleInstructions {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("multi_agent.role_instructions".to_string())
    }

    fn role(&self) -> &'static str {
        "developer"
    }

    fn requires_separate_message(&self) -> bool {
        true
    }

    fn markers(&self) -> (&'static str, &'static str) {
        if self.marked {
            Self::type_markers()
        } else {
            ("", "")
        }
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("<multi_agent_role>", "</multi_agent_role>")
    }

    fn body(&self) -> String {
        self.text.clone()
    }
}
