pub(crate) mod agent_resolver;
pub(crate) mod api;
pub(crate) mod child_config;
pub(crate) mod control;
// Merge-safety anchor: AgentControl remains the backend-independent api trait while
// LocalAgentControl is the local implementation and AgentIdentitySnapshot retains Cooldex V2 state.
mod identity;
mod registry;
pub(crate) mod role;
pub(crate) mod status;
pub(crate) mod types;

pub(crate) use codex_protocol::protocol::AgentStatus;
pub(crate) use control::LocalAgentControl;
pub(crate) use identity::AgentIdentitySnapshot;
pub(crate) use registry::exceeds_thread_spawn_depth_limit;
pub(crate) use registry::next_thread_spawn_depth;
pub(crate) use status::agent_status_from_event;
