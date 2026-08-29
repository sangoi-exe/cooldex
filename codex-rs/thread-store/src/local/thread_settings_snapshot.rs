use std::path::Path;

use codex_protocol::ThreadId;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_protocol::protocol::ThreadSettingsSnapshot;
use codex_rollout::RolloutItem;

use super::LocalThreadStore;
use super::thread_rollout_resolver;
use crate::LoadThreadHistoryParams;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;

#[cfg(test)]
#[path = "thread_settings_snapshot_tests.rs"]
mod tests;

/// Loads the newest persisted thread settings snapshot across the complete paginated lineage.
pub(super) async fn load_latest_thread_settings_snapshot(
    store: &LocalThreadStore,
    params: LoadThreadHistoryParams,
) -> ThreadStoreResult<Option<ThreadSettingsSnapshot>> {
    let resolved = if params.include_archived {
        thread_rollout_resolver::resolve_current_including_archived(store, params.thread_id).await?
    } else {
        thread_rollout_resolver::resolve_current(store, params.thread_id).await?
    };
    let path =
        resolved
            .map(|resolved| resolved.path)
            .ok_or_else(|| ThreadStoreError::InvalidRequest {
                message: format!("no rollout found for thread id {}", params.thread_id),
            })?;
    let session_meta = codex_rollout::read_session_meta_line(path.as_path())
        .await
        .map_err(|err| ThreadStoreError::Internal {
            message: format!("failed to read session metadata {}: {err}", path.display()),
        })?;
    if session_meta.meta.id != params.thread_id {
        return Err(ThreadStoreError::InvalidRequest {
            message: format!(
                "rollout at {} belongs to thread {}, not {}",
                path.display(),
                session_meta.meta.id,
                params.thread_id
            ),
        });
    }
    if session_meta.meta.history_mode != ThreadHistoryMode::Paginated {
        let history = store.load_history(params).await?;
        return Ok(history.items.into_iter().rev().find_map(|item| match item {
            RolloutItem::EventMsg(EventMsg::ThreadSettingsApplied(event)) => {
                Some(event.thread_settings)
            }
            _ => None,
        }));
    }

    let lineage = store.resolve_rollout_lineage(params.thread_id).await?;
    let mut latest_snapshot = None;
    for segment in lineage.segments() {
        if let Some(snapshot) = scan_segment_for_latest_thread_settings(
            params.thread_id,
            segment.rollout_path.as_path(),
            segment.end_ordinal(),
        )
        .await?
        {
            latest_snapshot = Some(snapshot);
        }
    }
    Ok(latest_snapshot)
}

async fn scan_segment_for_latest_thread_settings(
    thread_id: ThreadId,
    rollout_path: &Path,
    end_ordinal_exclusive: Option<u64>,
) -> ThreadStoreResult<Option<ThreadSettingsSnapshot>> {
    let mut reader = codex_rollout::open_rollout_line_reader(rollout_path)
        .await
        .map_err(|err| ThreadStoreError::Internal {
            message: format!(
                "failed to open thread settings lineage segment {}: {err}",
                rollout_path.display()
            ),
        })?;
    let mut latest_snapshot = None;

    while let Some(raw_line) =
        reader
            .next_line()
            .await
            .map_err(|err| ThreadStoreError::Internal {
                message: format!(
                    "failed to read thread settings lineage segment {}: {err}",
                    rollout_path.display()
                ),
            })?
    {
        let raw_line = raw_line.trim();
        if raw_line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(raw_line) else {
            continue;
        };
        if let Some(end_ordinal_exclusive) = end_ordinal_exclusive
            && value
                .get("ordinal")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|ordinal| ordinal >= end_ordinal_exclusive)
        {
            break;
        }
        let Ok(line) = codex_rollout::decode_rollout_line(value) else {
            continue;
        };
        if let Some(end_ordinal_exclusive) = end_ordinal_exclusive {
            let ordinal = line.ordinal.ok_or_else(|| ThreadStoreError::InvalidRequest {
                message: format!(
                    "invalid paginated history lineage for {thread_id}: source rollout {} is missing an ordinal",
                    rollout_path.display(),
                ),
            })?;
            if ordinal >= end_ordinal_exclusive {
                break;
            }
        }
        if let RolloutItem::EventMsg(EventMsg::ThreadSettingsApplied(event)) = line.item {
            latest_snapshot = Some(event.thread_settings);
        }
    }

    Ok(latest_snapshot)
}
