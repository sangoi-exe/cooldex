use std::fmt;
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::sync::watch;

use super::RunningTask;
use super::SteerAdmission;
use super::TurnState;

/// The terminal transition currently owning the session turn slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TerminalTransitionKind {
    Completing,
    Interrupting,
    Replacing,
}

/// A generation-bound reservation for starting one turn.
#[derive(Clone)]
pub(crate) struct TurnStartClaim {
    pub(crate) generation: u64,
    pub(crate) target_turn_id: String,
    pub(crate) turn_state: Arc<Mutex<TurnState>>,
}

/// A running task removed from the slot after its terminal transition became visible.
pub(crate) struct RetiredTurn {
    pub(crate) transition_generation: u64,
    pub(crate) task: RunningTask,
    pub(crate) turn_state: Arc<Mutex<TurnState>>,
}

/// A real runtime error for an invalid turn-slot transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TurnSlotError {
    Occupied {
        operation: &'static str,
        actual_phase: &'static str,
    },
    GenerationMismatch {
        operation: &'static str,
        expected: u64,
        actual: u64,
    },
    TurnIdMismatch {
        operation: &'static str,
        expected: String,
        actual: String,
    },
    PhaseMismatch {
        operation: &'static str,
        expected: &'static str,
        actual: &'static str,
    },
}

impl fmt::Display for TurnSlotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Occupied {
                operation,
                actual_phase,
            } => write!(
                formatter,
                "cannot {operation}: turn slot is {actual_phase}, not idle"
            ),
            Self::GenerationMismatch {
                operation,
                expected,
                actual,
            } => write!(
                formatter,
                "cannot {operation}: turn-slot generation changed from {expected} to {actual}"
            ),
            Self::TurnIdMismatch {
                operation,
                expected,
                actual,
            } => write!(
                formatter,
                "cannot {operation}: expected turn {expected}, found {actual}"
            ),
            Self::PhaseMismatch {
                operation,
                expected,
                actual,
            } => write!(
                formatter,
                "cannot {operation}: expected turn-slot phase {expected}, found {actual}"
            ),
        }
    }
}

impl std::error::Error for TurnSlotError {}

pub(crate) enum TurnSlotPhase {
    Idle,
    Starting {
        target_turn_id: String,
        turn_state: Arc<Mutex<TurnState>>,
    },
    Running {
        task: RunningTask,
        turn_state: Arc<Mutex<TurnState>>,
    },
    Transitioning {
        retired_turn_id: String,
        kind: TerminalTransitionKind,
        intended_successor_turn_id: Option<String>,
        turn_state: Arc<Mutex<TurnState>>,
    },
}

/// The single session-owned state machine for turn startup, execution, and cleanup.
pub(crate) struct TurnSlot {
    generation: u64,
    phase: TurnSlotPhase,
    generation_tx: watch::Sender<u64>,
}

impl Default for TurnSlot {
    fn default() -> Self {
        let (generation_tx, _) = watch::channel(0);
        Self {
            generation: 0,
            phase: TurnSlotPhase::Idle,
            generation_tx,
        }
    }
}

impl TurnSlot {
    #[cfg(test)]
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn subscribe_generation(&self) -> watch::Receiver<u64> {
        self.generation_tx.subscribe()
    }

    pub(crate) fn is_idle(&self) -> bool {
        matches!(self.phase, TurnSlotPhase::Idle)
    }

    pub(crate) fn is_active(&self) -> bool {
        !self.is_idle()
    }

    pub(crate) fn is_transitioning(&self) -> bool {
        matches!(self.phase, TurnSlotPhase::Transitioning { .. })
    }

    pub(crate) fn is_starting_or_transitioning(&self) -> bool {
        matches!(self.phase, TurnSlotPhase::Starting { .. }) || self.is_transitioning()
    }

    pub(crate) fn running_task(&self) -> Option<&RunningTask> {
        match &self.phase {
            TurnSlotPhase::Running { task, .. } => Some(task),
            TurnSlotPhase::Idle
            | TurnSlotPhase::Starting { .. }
            | TurnSlotPhase::Transitioning { .. } => None,
        }
    }

    pub(crate) fn running_task_mut(&mut self) -> Option<&mut RunningTask> {
        match &mut self.phase {
            TurnSlotPhase::Running { task, .. } => Some(task),
            TurnSlotPhase::Idle
            | TurnSlotPhase::Starting { .. }
            | TurnSlotPhase::Transitioning { .. } => None,
        }
    }

    pub(crate) fn turn_state(&self) -> Option<&Arc<Mutex<TurnState>>> {
        match &self.phase {
            TurnSlotPhase::Starting { turn_state, .. }
            | TurnSlotPhase::Running { turn_state, .. }
            | TurnSlotPhase::Transitioning { turn_state, .. } => Some(turn_state),
            TurnSlotPhase::Idle => None,
        }
    }

    pub(crate) fn running_turn_id(&self) -> Option<&str> {
        self.running_task()
            .map(|task| task.turn_context.sub_id.as_str())
    }

    pub(crate) fn starting_turn_id(&self) -> Option<&str> {
        match &self.phase {
            TurnSlotPhase::Starting { target_turn_id, .. } => Some(target_turn_id),
            TurnSlotPhase::Idle
            | TurnSlotPhase::Running { .. }
            | TurnSlotPhase::Transitioning { .. } => None,
        }
    }

    pub(crate) fn claim_start(
        &mut self,
        target_turn_id: String,
    ) -> Result<TurnStartClaim, TurnSlotError> {
        if !self.is_idle() {
            return Err(TurnSlotError::Occupied {
                operation: "claim turn startup",
                actual_phase: self.phase_name(),
            });
        }

        self.generation = self.generation.wrapping_add(1);
        let turn_state = Arc::new(Mutex::new(TurnState::default()));
        self.phase = TurnSlotPhase::Starting {
            target_turn_id: target_turn_id.clone(),
            turn_state: Arc::clone(&turn_state),
        };
        Ok(TurnStartClaim {
            generation: self.generation,
            target_turn_id,
            turn_state,
        })
    }

    pub(crate) fn validate_running_install(
        &self,
        claim: &TurnStartClaim,
    ) -> Result<(), TurnSlotError> {
        self.require_generation("install running task", claim.generation)?;
        match &self.phase {
            TurnSlotPhase::Starting { target_turn_id, .. } => {
                if target_turn_id != &claim.target_turn_id {
                    return Err(TurnSlotError::TurnIdMismatch {
                        operation: "install running task",
                        expected: claim.target_turn_id.clone(),
                        actual: target_turn_id.clone(),
                    });
                }
                Ok(())
            }
            TurnSlotPhase::Idle
            | TurnSlotPhase::Running { .. }
            | TurnSlotPhase::Transitioning { .. } => Err(TurnSlotError::PhaseMismatch {
                operation: "install running task",
                expected: "starting",
                actual: self.phase_name(),
            }),
        }
    }

    pub(crate) fn install_running(
        &mut self,
        claim: &TurnStartClaim,
        task: RunningTask,
    ) -> Result<(), TurnSlotError> {
        self.validate_running_install(claim)?;
        self.phase = TurnSlotPhase::Running {
            task,
            turn_state: Arc::clone(&claim.turn_state),
        };
        Ok(())
    }

    pub(crate) fn cancel_start(&mut self, claim: &TurnStartClaim) -> Result<(), TurnSlotError> {
        self.require_generation("cancel turn startup", claim.generation)?;
        match &self.phase {
            TurnSlotPhase::Starting { target_turn_id, .. }
                if target_turn_id == &claim.target_turn_id => {}
            TurnSlotPhase::Running { task, .. }
                if task.turn_context.sub_id == claim.target_turn_id => {}
            TurnSlotPhase::Starting { target_turn_id, .. } => {
                return Err(TurnSlotError::TurnIdMismatch {
                    operation: "cancel turn startup",
                    expected: claim.target_turn_id.clone(),
                    actual: target_turn_id.clone(),
                });
            }
            TurnSlotPhase::Running { task, .. } => {
                return Err(TurnSlotError::TurnIdMismatch {
                    operation: "cancel turn startup",
                    expected: claim.target_turn_id.clone(),
                    actual: task.turn_context.sub_id.clone(),
                });
            }
            TurnSlotPhase::Idle | TurnSlotPhase::Transitioning { .. } => {
                return Err(TurnSlotError::PhaseMismatch {
                    operation: "cancel turn startup",
                    expected: "starting-or-running",
                    actual: self.phase_name(),
                });
            }
        }
        let phase = std::mem::replace(&mut self.phase, TurnSlotPhase::Idle);
        drop(phase);
        self.advance_and_notify();
        Ok(())
    }

    pub(crate) fn cancel_unopened_start(&mut self) -> Result<String, TurnSlotError> {
        let TurnSlotPhase::Starting { target_turn_id, .. } = &self.phase else {
            return Err(TurnSlotError::PhaseMismatch {
                operation: "cancel unopened turn startup",
                expected: "starting",
                actual: self.phase_name(),
            });
        };
        let target_turn_id = target_turn_id.clone();
        self.phase = TurnSlotPhase::Idle;
        self.advance_and_notify();
        Ok(target_turn_id)
    }

    pub(crate) fn open_running(&mut self, claim: &TurnStartClaim) -> Result<(), TurnSlotError> {
        self.require_generation("open running task", claim.generation)?;
        let actual_phase = self.phase_name();
        let Some(task) = self.running_task_mut() else {
            return Err(TurnSlotError::PhaseMismatch {
                operation: "open running task",
                expected: "running",
                actual: actual_phase,
            });
        };
        if task.turn_context.sub_id != claim.target_turn_id {
            return Err(TurnSlotError::TurnIdMismatch {
                operation: "open running task",
                expected: claim.target_turn_id.clone(),
                actual: task.turn_context.sub_id.clone(),
            });
        }
        if task.steer_admission != SteerAdmission::Starting {
            return Err(TurnSlotError::PhaseMismatch {
                operation: "open running task",
                expected: "running-starting",
                actual: "running-open-or-sealed",
            });
        }
        task.steer_admission = SteerAdmission::Open;
        self.advance_and_notify();
        Ok(())
    }

    pub(crate) fn begin_transition(
        &mut self,
        kind: TerminalTransitionKind,
        intended_successor_turn_id: Option<String>,
    ) -> Result<RetiredTurn, TurnSlotError> {
        if self
            .running_task()
            .is_some_and(|task| task.steer_admission == SteerAdmission::Starting)
        {
            return Err(TurnSlotError::PhaseMismatch {
                operation: "begin terminal transition",
                expected: "running-open-or-sealed",
                actual: "running-starting",
            });
        }
        let phase = std::mem::replace(&mut self.phase, TurnSlotPhase::Idle);
        let TurnSlotPhase::Running { task, turn_state } = phase else {
            self.phase = phase;
            return Err(TurnSlotError::PhaseMismatch {
                operation: "begin terminal transition",
                expected: "running",
                actual: self.phase_name(),
            });
        };
        let retired_turn_id = task.turn_context.sub_id.clone();
        self.phase = TurnSlotPhase::Transitioning {
            retired_turn_id,
            kind,
            intended_successor_turn_id,
            turn_state: Arc::clone(&turn_state),
        };
        self.advance_and_notify();
        Ok(RetiredTurn {
            transition_generation: self.generation,
            task,
            turn_state,
        })
    }

    pub(crate) fn prepare_successor_start(
        &mut self,
        transition_generation: u64,
        target_turn_id: String,
    ) -> Result<TurnStartClaim, TurnSlotError> {
        self.require_generation("prepare replacement successor", transition_generation)?;
        match &self.phase {
            TurnSlotPhase::Transitioning {
                kind: TerminalTransitionKind::Replacing,
                intended_successor_turn_id,
                ..
            } => {
                let intended_successor_turn_id =
                    intended_successor_turn_id.as_deref().unwrap_or_default();
                if intended_successor_turn_id != target_turn_id {
                    return Err(TurnSlotError::TurnIdMismatch {
                        operation: "prepare replacement successor",
                        expected: target_turn_id,
                        actual: intended_successor_turn_id.to_string(),
                    });
                }
            }
            TurnSlotPhase::Idle
            | TurnSlotPhase::Starting { .. }
            | TurnSlotPhase::Running { .. }
            | TurnSlotPhase::Transitioning { .. } => {
                return Err(TurnSlotError::PhaseMismatch {
                    operation: "prepare replacement successor",
                    expected: "transitioning-replacing",
                    actual: self.phase_name(),
                });
            }
        }

        let turn_state = Arc::new(Mutex::new(TurnState::default()));
        self.phase = TurnSlotPhase::Starting {
            target_turn_id: target_turn_id.clone(),
            turn_state: Arc::clone(&turn_state),
        };
        Ok(TurnStartClaim {
            generation: self.generation,
            target_turn_id,
            turn_state,
        })
    }

    pub(crate) fn finish_transition_idle(
        &mut self,
        transition_generation: u64,
        retired_turn_id: &str,
    ) -> Result<(), TurnSlotError> {
        self.require_generation("finish terminal transition", transition_generation)?;
        match &self.phase {
            TurnSlotPhase::Transitioning {
                retired_turn_id: actual_turn_id,
                ..
            } if actual_turn_id == retired_turn_id => {}
            TurnSlotPhase::Transitioning {
                retired_turn_id: actual_turn_id,
                ..
            } => {
                return Err(TurnSlotError::TurnIdMismatch {
                    operation: "finish terminal transition",
                    expected: retired_turn_id.to_string(),
                    actual: actual_turn_id.clone(),
                });
            }
            TurnSlotPhase::Idle
            | TurnSlotPhase::Starting { .. }
            | TurnSlotPhase::Running { .. } => {
                return Err(TurnSlotError::PhaseMismatch {
                    operation: "finish terminal transition",
                    expected: "transitioning",
                    actual: self.phase_name(),
                });
            }
        }

        self.phase = TurnSlotPhase::Idle;
        self.advance_and_notify();
        Ok(())
    }

    fn require_generation(
        &self,
        operation: &'static str,
        expected: u64,
    ) -> Result<(), TurnSlotError> {
        if self.generation == expected {
            Ok(())
        } else {
            Err(TurnSlotError::GenerationMismatch {
                operation,
                expected,
                actual: self.generation,
            })
        }
    }

    fn phase_name(&self) -> &'static str {
        match &self.phase {
            TurnSlotPhase::Idle => "idle",
            TurnSlotPhase::Starting { .. } => "starting",
            TurnSlotPhase::Running { .. } => "running",
            TurnSlotPhase::Transitioning { kind, .. } => match kind {
                TerminalTransitionKind::Completing => "transitioning-completing",
                TerminalTransitionKind::Interrupting => "transitioning-interrupting",
                TerminalTransitionKind::Replacing => "transitioning-replacing",
            },
        }
    }

    fn advance_and_notify(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.generation_tx.send_replace(self.generation);
    }
}

#[cfg(test)]
#[path = "turn_slot_tests.rs"]
mod tests;
