use std::sync::Arc;

use pretty_assertions::assert_eq;
use tokio::sync::Mutex;

use super::TerminalTransitionKind;
use super::TurnSlot;
use super::TurnSlotError;
use super::TurnSlotPhase;
use super::TurnState;

#[test]
fn start_claim_rejects_an_occupied_slot() {
    let mut slot = TurnSlot::default();
    slot.claim_start("first".to_string())
        .expect("idle slot should accept first claim");

    let Err(error) = slot.claim_start("second".to_string()) else {
        panic!("occupied slot should reject a second start claim");
    };
    assert_eq!(
        error,
        TurnSlotError::Occupied {
            operation: "claim turn startup",
            actual_phase: "starting",
        }
    );
}

#[test]
fn running_install_rejects_a_stale_generation() {
    let mut slot = TurnSlot::default();
    let mut claim = slot
        .claim_start("turn".to_string())
        .expect("idle slot should accept claim");
    claim.generation = claim.generation.wrapping_add(1);

    assert_eq!(
        slot.validate_running_install(&claim),
        Err(TurnSlotError::GenerationMismatch {
            operation: "install running task",
            expected: claim.generation,
            actual: slot.generation(),
        })
    );
}

#[tokio::test]
async fn cancel_unopened_start_settles_to_idle_and_notifies_waiters() {
    let mut slot = TurnSlot::default();
    slot.claim_start("turn".to_string())
        .expect("idle slot should accept claim");

    let mut generation_rx = slot.subscribe_generation();
    let cancelled_turn_id = slot
        .cancel_unopened_start()
        .expect("starting slot should cancel");

    generation_rx
        .changed()
        .await
        .expect("cancelling a starting turn should notify generation waiters");
    assert_eq!(cancelled_turn_id, "turn");
    assert!(slot.is_idle());
}

#[tokio::test]
async fn terminal_transitions_notify_generation_waiters_when_they_settle() {
    for kind in [
        TerminalTransitionKind::Completing,
        TerminalTransitionKind::Interrupting,
        TerminalTransitionKind::Replacing,
    ] {
        let mut slot = TurnSlot {
            phase: TurnSlotPhase::Transitioning {
                retired_turn_id: "turn".to_string(),
                kind,
                intended_successor_turn_id: None,
                turn_state: Arc::new(Mutex::new(TurnState::default())),
            },
            ..TurnSlot::default()
        };
        assert!(slot.is_transitioning());
        assert!(slot.is_starting_or_transitioning());

        let transition_generation = slot.generation();
        let mut generation_rx = slot.subscribe_generation();
        slot.finish_transition_idle(transition_generation, "turn")
            .expect("terminal transition should settle to idle");
        generation_rx
            .changed()
            .await
            .expect("settled transition should notify generation waiters");
        assert!(slot.is_idle());
    }
}
