// Module declarations for the app-server protocol namespace.
// Exposes protocol pieces used by `lib.rs` via `pub use protocol::common::*;`.
// Merge-safety anchor: protocol exports here must keep config keyPath helpers
// shared between TUI writers and core config parsing.

pub mod common;
pub mod config_key_path;
pub mod item_builders;
mod mappers;
mod serde_helpers;
pub mod thread_history;
pub mod v1;
pub mod v2;
