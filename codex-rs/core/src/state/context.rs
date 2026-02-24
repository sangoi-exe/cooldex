use std::collections::BTreeMap;

/// Category used for context pruning + reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PruneCategory {
    ToolOutput,
    ToolCall,
    Reasoning,
    AssistantMessage,
    UserMessage,
    UserInstructions,
    EnvironmentContext,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextItemSummary {
    pub(crate) index: usize,
    pub(crate) category: PruneCategory,
    pub(crate) preview: String,
    pub(crate) included: bool,
    pub(crate) id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextItemsEvent {
    pub(crate) items: Vec<ContextItemSummary>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ContextOverlay {
    pub(crate) replacements_by_rid: BTreeMap<u64, String>,
    pub(crate) notes: Vec<String>,
}
