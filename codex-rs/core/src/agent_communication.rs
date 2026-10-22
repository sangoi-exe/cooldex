//! Direct-message diagnostics and delivery ownership across queues and eviction.
//! Only history commitment consumes mail; dropping its last owner releases its attachments.

use codex_extension_api::ExtensionData;
use codex_extension_api::MessageLifecycleContributor;
use codex_extension_api::MessageLifecycleInput;
use codex_protocol::ResponseItemId;
use codex_protocol::ThreadId;
use codex_protocol::protocol::InterAgentCommunication;
use serde::Deserialize;
use serde::Serialize;
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;

pub(crate) static PENDING_MAILBOX_MESSAGES: codex_diagnostics::Gauge =
    codex_diagnostics::Gauge::new("core.mailbox.pending");

const AGENT_COMMUNICATION_TARGET: &str = "codex_otel.agent_communication";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentCommunicationKind {
    Spawn,
    Message,
    Followup,
    Result,
}

impl AgentCommunicationKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Spawn => "spawn",
            Self::Message => "message",
            Self::Followup => "followup",
            Self::Result => "result",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentCommunicationContext {
    kind: AgentCommunicationKind,
    pub(crate) sender_thread_id: ThreadId,
    pub(crate) terminal_error_wake: bool,
}

impl AgentCommunicationContext {
    pub(crate) fn new(kind: AgentCommunicationKind, sender_thread_id: ThreadId) -> Self {
        Self {
            kind,
            sender_thread_id,
            terminal_error_wake: false,
        }
    }
}

pub(crate) fn logging_enabled() -> bool {
    tracing::enabled!(target: AGENT_COMMUNICATION_TARGET, tracing::Level::INFO)
}

pub(crate) fn emit_agent_communication_send(
    communication_id: &str,
    context: &AgentCommunicationContext,
    communication: &InterAgentCommunication,
    receiver_thread_id: ThreadId,
) {
    tracing::info!(
        target: AGENT_COMMUNICATION_TARGET,
        {
            event.name = "codex.agent_communication",
            communication_id,
            kind = context.kind.as_str(),
            state = "send",
            sender_thread_id = %context.sender_thread_id,
            receiver_thread_id = %receiver_thread_id,
            content = communication
                .encrypted_content
                .as_deref()
                .unwrap_or("[plaintext]"),
        },
        "agent communication"
    );
}

pub(crate) fn emit_agent_communication_receive(communication_id: &str) {
    tracing::info!(
        target: AGENT_COMMUNICATION_TARGET,
        {
            event.name = "codex.agent_communication",
            communication_id,
            state = "receive",
        },
        "agent communication"
    );
}

/// Pending mail with shared ownership of its captured publication state.
/// Serialization preserves the protocol shape; restored historical mail is not republished.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PendingAgentMessage {
    communication: InterAgentCommunication,
    #[serde(skip)]
    pub(crate) delivery: Option<MessageDelivery>,
    // Merge-safety anchor: terminal-error wake eligibility stays with live mail, never on the wire or in restored history.
    #[serde(skip)]
    pub(crate) terminal_error_wake: bool,
}

impl From<InterAgentCommunication> for PendingAgentMessage {
    fn from(communication: InterAgentCommunication) -> Self {
        Self::new(
            communication,
            /*delivery*/ None,
            /*terminal_error_wake*/ false,
        )
    }
}

impl Deref for PendingAgentMessage {
    type Target = InterAgentCommunication;
    fn deref(&self) -> &Self::Target {
        &self.communication
    }
}

impl PartialEq for PendingAgentMessage {
    fn eq(&self, other: &Self) -> bool {
        self.communication == other.communication
    }
}

impl PendingAgentMessage {
    pub(crate) fn new(
        communication: InterAgentCommunication,
        delivery: Option<MessageDelivery>,
        terminal_error_wake: bool,
    ) -> Self {
        Self {
            communication,
            delivery,
            terminal_error_wake,
        }
    }
}

/// The publication state retained until this delivery is consumed or abandoned.
struct MessagePublication {
    input: MessageLifecycleInput,
    observers: Box<[Arc<dyn MessageLifecycleContributor>]>,
}

/// Clones follow one logical delivery through queues and share its consumption state.
#[derive(Clone)]
pub(crate) struct MessageDelivery(Arc<Mutex<Option<MessagePublication>>>);

impl MessageDelivery {
    pub(crate) fn publish(
        sender: ThreadId,
        message_id: ResponseItemId,
        observers: &[Arc<dyn MessageLifecycleContributor>],
    ) -> Self {
        let input = MessageLifecycleInput {
            sender,
            data: ExtensionData::new(message_id.to_string()),
            message_id,
        };
        for observer in observers {
            observer.on_publish(&input);
        }
        let publication = MessagePublication {
            input,
            observers: observers.into(),
        };
        Self(Arc::new(Mutex::new(Some(publication))))
    }

    /// Commit this delivery and release its publication state, even if clones remain.
    pub(crate) fn consumed(&self, recipient: ThreadId) {
        let publication = self.0.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(publication) = publication {
            for observer in &publication.observers {
                observer.on_consumed(&publication.input, recipient);
            }
        }
    }
}

impl fmt::Debug for MessageDelivery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MessageDelivery").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::AgentPath;

    #[test]
    fn terminal_error_wake_is_live_delivery_metadata_only() {
        let context =
            AgentCommunicationContext::new(AgentCommunicationKind::Result, ThreadId::new());
        assert!(!context.terminal_error_wake);
        let communication = InterAgentCommunication::new(
            AgentPath::try_from("/root/worker").expect("worker path"),
            AgentPath::root(),
            Vec::new(),
            "terminal error".to_string(),
            /*trigger_turn*/ false,
        );
        assert!(!PendingAgentMessage::from(communication.clone()).terminal_error_wake);
        let pending = PendingAgentMessage::new(communication.clone(), None, true);
        let serialized = serde_json::to_value(&pending).expect("serialize live mail");
        assert_eq!(serialized, serde_json::to_value(&communication).unwrap());
        let restored: PendingAgentMessage =
            serde_json::from_value(serialized).expect("restore mail");
        assert!(!restored.terminal_error_wake);
        assert!(!restored.trigger_turn);
    }
}
