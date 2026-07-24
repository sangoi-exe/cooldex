use codex_protocol::protocol::PostCompactRecoveryAppliedItem;

use crate::context::PostCompactRecoveryContext;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PostCompactRecoveryIdentity {
    pub(crate) compaction_window_id: String,
    pub(crate) boundary_item_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PostCompactRecoverySamplingProof {
    identity: PostCompactRecoveryIdentity,
    turn_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingPostCompactRecovery {
    identity: PostCompactRecoveryIdentity,
    packet: Option<PostCompactRecoveryContext>,
    sampling_proof: Option<PostCompactRecoverySamplingProof>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PostCompactRecoveryFailureClass {
    #[error("canonical_persistence_indeterminate")]
    CanonicalPersistenceIndeterminate,
    #[error("malformed_marker")]
    MalformedMarker,
    #[error("malformed_application_proof")]
    MalformedApplicationProof,
    #[error("boundary_mismatch")]
    BoundaryMismatch,
    #[error("thread_mismatch")]
    ThreadMismatch,
    #[error("recall_parse")]
    RecallParse,
    #[error("serialization")]
    Serialization,
    #[error("packet_cap")]
    PacketCap,
    #[error("unsupported_legacy_recovery")]
    UnsupportedLegacy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PostCompactRecoveryTurnOutcome {
    Successful,
    Aborted,
    UnexpectedTaskError,
    TerminalError,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum PostCompactRecoveryRuntimeState {
    #[default]
    Absent,
    Pending(PendingPostCompactRecovery),
    Blocked(PostCompactRecoveryFailureClass),
}

impl PostCompactRecoveryRuntimeState {
    pub(crate) fn pending(identity: PostCompactRecoveryIdentity) -> Self {
        Self::Pending(PendingPostCompactRecovery {
            identity,
            packet: None,
            sampling_proof: None,
        })
    }

    pub(crate) fn pending_identity(&self) -> Option<&PostCompactRecoveryIdentity> {
        match self {
            Self::Pending(pending) => Some(&pending.identity),
            Self::Absent | Self::Blocked(_) => None,
        }
    }

    pub(crate) fn blocked_failure(&self) -> Option<PostCompactRecoveryFailureClass> {
        match self {
            Self::Blocked(failure) => Some(*failure),
            Self::Absent | Self::Pending(_) => None,
        }
    }

    pub(crate) fn packet(
        &self,
        identity: &PostCompactRecoveryIdentity,
    ) -> Result<Option<PostCompactRecoveryContext>, PostCompactRecoveryFailureClass> {
        match self {
            Self::Absent => Ok(None),
            Self::Blocked(failure) => Err(*failure),
            Self::Pending(pending) if pending.identity == *identity => Ok(pending.packet.clone()),
            Self::Pending(_) => Err(PostCompactRecoveryFailureClass::BoundaryMismatch),
        }
    }

    pub(crate) fn cache_packet(
        &mut self,
        identity: &PostCompactRecoveryIdentity,
        packet: PostCompactRecoveryContext,
    ) -> Result<(), PostCompactRecoveryFailureClass> {
        match self {
            Self::Blocked(failure) => Err(*failure),
            Self::Pending(pending) if pending.identity == *identity => {
                match pending.packet.as_ref() {
                    Some(cached) if cached != &packet => {
                        Err(PostCompactRecoveryFailureClass::BoundaryMismatch)
                    }
                    Some(_) => Ok(()),
                    None => {
                        pending.packet = Some(packet);
                        Ok(())
                    }
                }
            }
            Self::Absent | Self::Pending(_) => {
                Err(PostCompactRecoveryFailureClass::BoundaryMismatch)
            }
        }
    }

    pub(crate) fn record_sampling_success(
        &mut self,
        identity: &PostCompactRecoveryIdentity,
        turn_id: &str,
    ) -> Result<(), PostCompactRecoveryFailureClass> {
        match self {
            Self::Blocked(failure) => Err(*failure),
            Self::Pending(pending) if pending.identity == *identity => {
                let proof = PostCompactRecoverySamplingProof {
                    identity: identity.clone(),
                    turn_id: turn_id.to_string(),
                };
                match pending.sampling_proof.as_ref() {
                    Some(existing) if existing != &proof => {
                        Err(PostCompactRecoveryFailureClass::BoundaryMismatch)
                    }
                    Some(_) => Ok(()),
                    None => {
                        pending.sampling_proof = Some(proof);
                        Ok(())
                    }
                }
            }
            Self::Absent | Self::Pending(_) => {
                Err(PostCompactRecoveryFailureClass::BoundaryMismatch)
            }
        }
    }

    pub(crate) fn clear_sampling_proof_for_turn(&mut self, turn_id: &str) {
        if let Self::Pending(pending) = self
            && pending
                .sampling_proof
                .as_ref()
                .is_some_and(|proof| proof.turn_id == turn_id)
        {
            pending.sampling_proof = None;
        }
    }

    pub(crate) fn application_candidate(
        &self,
        turn_id: &str,
        outcome: PostCompactRecoveryTurnOutcome,
    ) -> Result<Option<PostCompactRecoveryAppliedItem>, PostCompactRecoveryFailureClass> {
        if !matches!(outcome, PostCompactRecoveryTurnOutcome::Successful) {
            return Ok(None);
        }
        match self {
            Self::Absent => Ok(None),
            Self::Blocked(failure) => Err(*failure),
            Self::Pending(pending) => {
                let Some(proof) = pending.sampling_proof.as_ref() else {
                    return Ok(None);
                };
                if proof.identity != pending.identity || proof.turn_id != turn_id {
                    return Err(PostCompactRecoveryFailureClass::BoundaryMismatch);
                }
                Ok(Some(PostCompactRecoveryAppliedItem {
                    compaction_window_id: pending.identity.compaction_window_id.clone(),
                    boundary_item_id: pending.identity.boundary_item_id.clone(),
                    turn_id: proof.turn_id.clone(),
                }))
            }
        }
    }

    pub(crate) fn clear_after_application(
        &mut self,
        applied: &PostCompactRecoveryAppliedItem,
    ) -> Result<(), PostCompactRecoveryFailureClass> {
        let Self::Pending(pending) = self else {
            return Err(PostCompactRecoveryFailureClass::BoundaryMismatch);
        };
        let Some(proof) = pending.sampling_proof.as_ref() else {
            return Err(PostCompactRecoveryFailureClass::BoundaryMismatch);
        };
        if pending.identity.compaction_window_id != applied.compaction_window_id
            || pending.identity.boundary_item_id != applied.boundary_item_id
            || proof.identity != pending.identity
            || proof.turn_id != applied.turn_id
        {
            return Err(PostCompactRecoveryFailureClass::BoundaryMismatch);
        }
        *self = Self::Absent;
        Ok(())
    }

    pub(crate) fn block(&mut self, failure: PostCompactRecoveryFailureClass) {
        *self = Self::Blocked(failure);
    }
}

#[cfg(test)]
#[path = "post_compact_recovery_tests.rs"]
mod tests;
