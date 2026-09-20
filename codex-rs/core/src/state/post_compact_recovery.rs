use codex_history::PostCompactRecoveryAppliedItem;
use codex_history::PostCompactRecoveryPayloadKind;

use crate::context::PostCompactRecoveryContext;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PostCompactRecoveryIdentity {
    pub(crate) compaction_window_id: String,
    pub(crate) boundary_item_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingPostCompactRecovery {
    identity: PostCompactRecoveryIdentity,
    packet: Option<PostCompactRecoveryContext>,
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
    #[error("serialization")]
    Serialization,
    #[error("packet_cap")]
    PacketCap,
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
        })
    }

    pub(crate) fn pending_with_packet(
        identity: PostCompactRecoveryIdentity,
        packet: PostCompactRecoveryContext,
    ) -> Self {
        Self::Pending(PendingPostCompactRecovery {
            identity,
            packet: Some(packet),
        })
    }

    pub(crate) fn pending_identity(&self) -> Option<&PostCompactRecoveryIdentity> {
        match self {
            Self::Pending(pending) => Some(&pending.identity),
            Self::Absent | Self::Blocked(_) => None,
        }
    }

    // Merge-safety anchor: pre-compaction synthesis may observe a cached handoff/recovery packet
    // but must not trigger packet construction, cache population, application, or state mutation.
    pub(crate) fn pending_packet_snapshot(
        &self,
    ) -> Result<
        Option<(PostCompactRecoveryIdentity, PostCompactRecoveryContext)>,
        PostCompactRecoveryFailureClass,
    > {
        match self {
            Self::Absent => Ok(None),
            Self::Blocked(failure) => Err(*failure),
            Self::Pending(pending) => Ok(pending
                .packet
                .clone()
                .map(|packet| (pending.identity.clone(), packet))),
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

    pub(crate) fn application_for_sampling_success(
        &self,
        identity: &PostCompactRecoveryIdentity,
        turn_id: &str,
    ) -> Result<PostCompactRecoveryAppliedItem, PostCompactRecoveryFailureClass> {
        // Merge-safety anchor: only a matching durable application may consume pending recovery;
        // explicit recall retains its independent thread-mismatch diagnostics.
        match self {
            Self::Blocked(failure) => Err(*failure),
            Self::Pending(pending)
                if pending.identity == *identity && !turn_id.trim().is_empty() =>
            {
                let payload_kind = match pending.packet.as_ref() {
                    Some(packet) if packet.handoff().is_some() => {
                        PostCompactRecoveryPayloadKind::HandoffAndRecovery
                    }
                    Some(_) => PostCompactRecoveryPayloadKind::RecoveryOnly,
                    None => return Err(PostCompactRecoveryFailureClass::BoundaryMismatch),
                };
                Ok(PostCompactRecoveryAppliedItem {
                    compaction_window_id: identity.compaction_window_id.clone(),
                    boundary_item_id: identity.boundary_item_id.clone(),
                    turn_id: turn_id.to_string(),
                    payload_kind,
                })
            }
            Self::Absent | Self::Pending(_) => {
                Err(PostCompactRecoveryFailureClass::BoundaryMismatch)
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
        if pending.identity.compaction_window_id != applied.compaction_window_id
            || pending.identity.boundary_item_id != applied.boundary_item_id
            || applied.turn_id.trim().is_empty()
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
