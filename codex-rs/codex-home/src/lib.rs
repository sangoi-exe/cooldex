mod instructions;

pub use instructions::CodexHomeUserInstructionsProvider;
// Merge-safety anchor: preserve this public global-instructions gate re-export for runtime callers.
pub use instructions::GlobalInstructionsMode;
