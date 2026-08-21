use std::collections::HashSet;
use std::fs::File;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use codex_history::RolloutItem;
use codex_history::RolloutLine;
use codex_protocol::ThreadId;
use codex_protocol::protocol::HistoryPosition;
use codex_protocol::protocol::SessionMetaLine;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_rollout::ReverseJsonlScanner;
use codex_rollout::ScanOutcome;
use serde_json::Value;

use super::LocalThreadStore;
use super::rollout_lineage::validate_cutoff_bounds;
use super::thread_rollout_resolver;
use crate::LoadRolloutTailParams;
use crate::RecallRolloutSourceIssue;
use crate::RecallRolloutSourceIssueKind;
use crate::StoredRecallRolloutTail;
use crate::StoredRolloutTail;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;

#[cfg(test)]
#[path = "rollout_tail_tests.rs"]
mod tests;

pub(super) async fn load_rollout_tail(
    store: &LocalThreadStore,
    params: LoadRolloutTailParams,
) -> ThreadStoreResult<StoredRolloutTail> {
    let tail = load_rollout_tail_with_projection(store, params, TailProjection::Strict).await?;
    if let Some(issue) = tail.source_issue {
        return Err(ThreadStoreError::Internal {
            message: format!(
                "strict rollout reader unexpectedly returned a projected source issue: {}",
                issue.message
            ),
        });
    }
    Ok(StoredRolloutTail {
        thread_id: tail.thread_id,
        items: tail.items,
        reached_start: tail.reached_start,
        bytes_read: tail.bytes_read,
        records_read: tail.records_read,
        segments_read: tail.segments_read,
    })
}

pub(super) async fn load_recall_rollout_tail(
    store: &LocalThreadStore,
    params: LoadRolloutTailParams,
) -> ThreadStoreResult<StoredRecallRolloutTail> {
    load_rollout_tail_with_projection(store, params, TailProjection::Recall).await
}

async fn load_rollout_tail_with_projection(
    store: &LocalThreadStore,
    params: LoadRolloutTailParams,
    projection: TailProjection,
) -> ThreadStoreResult<StoredRecallRolloutTail> {
    let path = resolve_rollout_path(store, params.thread_id, params.include_archived)
        .await?
        .ok_or_else(|| ThreadStoreError::InvalidRequest {
            message: format!("no rollout found for thread id {}", params.thread_id),
        })?;
    scan_rollout_tail(store, path, params, projection).await
}

async fn scan_rollout_tail(
    store: &LocalThreadStore,
    initial_path: PathBuf,
    params: LoadRolloutTailParams,
    projection: TailProjection,
) -> ThreadStoreResult<StoredRecallRolloutTail> {
    let mut items_newest_first = Vec::new();
    let mut bytes_read = 0_u64;
    let mut records_read = 0_usize;
    let mut segments_read = 0_usize;
    let mut reached_start = false;
    let mut source_issue = None;
    let mut seen = HashSet::new();
    let mut segment_thread_id = params.thread_id;
    let mut segment_path = initial_path;
    let mut segment_end = None;

    loop {
        if !seen.insert(segment_thread_id) {
            return Err(invalid_lineage(params.thread_id, "cycle detected"));
        }
        if is_compressed_rollout(segment_path.as_path()) {
            break;
        }
        segments_read += 1;
        if let Some(end) = segment_end {
            validate_cutoff_bounds(params.thread_id, segment_path.as_path(), &end).await?;
        }
        let remaining_bytes = params.max_bytes.saturating_sub(bytes_read);
        let remaining_records = params.max_records.saturating_sub(records_read);
        let path = segment_path.clone();
        let scan = tokio::task::spawn_blocking(move || {
            scan_rollout_segment(
                path.as_path(),
                segment_end,
                remaining_bytes,
                remaining_records,
                projection,
            )
        })
        .await
        .map_err(|err| ThreadStoreError::Internal {
            message: format!("failed to join bounded rollout scan: {err}"),
        })?
        .map_err(|err| ThreadStoreError::Internal {
            message: format!("failed to scan bounded rollout tail: {err}"),
        })?;
        bytes_read = bytes_read.saturating_add(scan.bytes_read);
        records_read = records_read.saturating_add(scan.records_read);
        items_newest_first.extend(scan.items_newest_first);
        if let Some(issue) = scan.source_issue {
            source_issue = Some(issue);
            break;
        }

        if !scan.reached_start {
            break;
        }
        let session_meta = scan
            .session_meta
            .ok_or_else(|| ThreadStoreError::Internal {
                message: format!(
                    "rollout at {} is missing session metadata",
                    segment_path.display()
                ),
            })?;
        if session_meta.meta.id != segment_thread_id {
            return Err(invalid_lineage(
                params.thread_id,
                "source rollout belongs to another thread",
            ));
        }

        match session_meta.meta.history_mode {
            ThreadHistoryMode::Legacy => {
                if segment_thread_id != params.thread_id || session_meta.meta.history_base.is_some()
                {
                    return Err(invalid_lineage(
                        params.thread_id,
                        "legacy rollout cannot be a paginated lineage segment",
                    ));
                }
                reached_start = true;
                break;
            }
            ThreadHistoryMode::Paginated => {
                let Some(base) = session_meta.meta.history_base else {
                    reached_start = true;
                    break;
                };
                segment_thread_id = base.thread_id;
                segment_end = Some(base);
                segment_path =
                    resolve_rollout_path(store, segment_thread_id, /*include_archived*/ true)
                        .await?
                        .ok_or_else(|| {
                            invalid_lineage(segment_thread_id, "missing source rollout")
                        })?;
            }
        }
    }

    items_newest_first.reverse();
    Ok(StoredRecallRolloutTail {
        thread_id: params.thread_id,
        items: items_newest_first,
        reached_start,
        bytes_read,
        records_read,
        segments_read,
        source_issue,
    })
}

async fn resolve_rollout_path(
    store: &LocalThreadStore,
    thread_id: ThreadId,
    include_archived: bool,
) -> ThreadStoreResult<Option<PathBuf>> {
    let resolved = if include_archived {
        thread_rollout_resolver::resolve_current_including_archived(store, thread_id).await?
    } else {
        thread_rollout_resolver::resolve_current(store, thread_id).await?
    };
    Ok(resolved.map(|resolved| resolved.path))
}

#[derive(Clone, Copy)]
enum TailProjection {
    Strict,
    Recall,
}

struct SegmentScan {
    items_newest_first: Vec<RolloutItem>,
    session_meta: Option<SessionMetaLine>,
    reached_start: bool,
    bytes_read: u64,
    records_read: usize,
    source_issue: Option<RecallRolloutSourceIssue>,
}

fn scan_rollout_segment(
    path: &Path,
    end: Option<HistoryPosition>,
    max_bytes: u64,
    max_records: usize,
    projection: TailProjection,
) -> io::Result<SegmentScan> {
    match projection {
        TailProjection::Strict => scan_strict_rollout_segment(path, end, max_bytes, max_records),
        TailProjection::Recall => scan_recall_rollout_segment(path, end, max_bytes, max_records),
    }
}

fn scan_strict_rollout_segment(
    path: &Path,
    end: Option<HistoryPosition>,
    max_bytes: u64,
    max_records: usize,
) -> io::Result<SegmentScan> {
    let file = File::open(path)?;
    let end_byte_offset = match end {
        Some(end) => end.end_byte_offset,
        None => file.metadata()?.len(),
    };
    let mut scanner =
        ReverseJsonlScanner::new_at_with_byte_limit(file, end_byte_offset, max_bytes)?;
    let mut items_newest_first = Vec::new();
    let mut session_meta = None;
    let mut records_read = 0_usize;

    while records_read < max_records {
        let Some(outcome) = scanner.scan_next::<RolloutLine>()? else {
            break;
        };
        records_read += 1;
        let line = match outcome {
            ScanOutcome::Parsed(line) => line,
            ScanOutcome::Rejected(err) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "rejected rollout record in {} at byte offset {}: {err}",
                        path.display(),
                        scanner
                            .last_record_start_offset()
                            .map_or_else(|| "unknown".to_string(), |offset| offset.to_string())
                    ),
                ));
            }
        };
        let reached_start = scanner.reached_start();
        match line.item {
            RolloutItem::SessionMeta(meta) if reached_start => session_meta = Some(meta),
            RolloutItem::SessionMeta(_) => {}
            _ if reached_start => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "session metadata is not the first record in {}",
                        path.display()
                    ),
                ));
            }
            item => items_newest_first.push(item),
        }
    }

    Ok(SegmentScan {
        items_newest_first,
        session_meta,
        reached_start: scanner.reached_start(),
        bytes_read: scanner.bytes_read(),
        records_read,
        source_issue: None,
    })
}

fn scan_recall_rollout_segment(
    path: &Path,
    end: Option<HistoryPosition>,
    max_bytes: u64,
    max_records: usize,
) -> io::Result<SegmentScan> {
    let file = File::open(path)?;
    let end_byte_offset = match end {
        Some(end) => end.end_byte_offset,
        None => file.metadata()?.len(),
    };
    let mut scanner =
        ReverseJsonlScanner::new_at_with_byte_limit(file, end_byte_offset, max_bytes)?;
    let mut items_newest_first = Vec::new();
    let mut session_meta = None;
    let mut records_read = 0_usize;
    let mut source_issue = None;

    while records_read < max_records {
        let Some(outcome) = scanner.scan_next::<Value>()? else {
            break;
        };
        records_read += 1;
        let byte_offset = scanner.last_record_start_offset();
        let value = match outcome {
            ScanOutcome::Parsed(value) => value,
            ScanOutcome::Rejected(error) => {
                source_issue = Some(recall_source_issue(
                    RecallRolloutSourceIssueKind::SourceError,
                    path,
                    byte_offset,
                    /*ordinal*/ None,
                    /*record_type*/ None,
                    /*event_type*/ None,
                    format!("invalid JSONL record: {error}"),
                ));
                break;
            }
        };
        let projection = match project_recall_rollout_line(path, byte_offset, value) {
            Ok(projection) => projection,
            Err(issue) => {
                source_issue = Some(issue);
                break;
            }
        };
        let reached_start = scanner.reached_start();
        let Some(line) = projection.line else {
            if reached_start {
                source_issue = Some(recall_source_issue(
                    RecallRolloutSourceIssueKind::SourceError,
                    path,
                    byte_offset,
                    projection.ordinal,
                    Some(projection.record_type),
                    projection.event_type,
                    "session metadata is not the first record".to_string(),
                ));
            }
            if source_issue.is_some() {
                break;
            }
            continue;
        };
        match line.item {
            RolloutItem::SessionMeta(meta) if reached_start => session_meta = Some(meta),
            RolloutItem::SessionMeta(_) => {}
            _ if reached_start => {
                source_issue = Some(recall_source_issue(
                    RecallRolloutSourceIssueKind::SourceError,
                    path,
                    byte_offset,
                    projection.ordinal,
                    Some(projection.record_type),
                    projection.event_type,
                    "session metadata is not the first record".to_string(),
                ));
                break;
            }
            item => items_newest_first.push(item),
        }
    }

    Ok(SegmentScan {
        items_newest_first,
        session_meta,
        reached_start: scanner.reached_start(),
        bytes_read: scanner.bytes_read(),
        records_read,
        source_issue,
    })
}

struct RecallLineProjection {
    line: Option<RolloutLine>,
    ordinal: Option<u64>,
    record_type: String,
    event_type: Option<String>,
}

fn project_recall_rollout_line(
    path: &Path,
    byte_offset: Option<u64>,
    value: Value,
) -> Result<RecallLineProjection, RecallRolloutSourceIssue> {
    let ordinal = value.get("ordinal").and_then(Value::as_u64);
    let record_type = value
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            recall_source_issue(
                RecallRolloutSourceIssueKind::SourceError,
                path,
                byte_offset,
                ordinal,
                /*record_type*/ None,
                /*event_type*/ None,
                "rollout record is missing a string type discriminator".to_string(),
            )
        })?;
    let event_type = (record_type == "event_msg")
        .then(|| {
            value
                .get("payload")
                .and_then(|payload| payload.get("type"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .flatten();
    if record_type == "event_msg" && event_type.is_none() {
        return Err(recall_source_issue(
            RecallRolloutSourceIssueKind::SourceError,
            path,
            byte_offset,
            ordinal,
            Some(record_type),
            /*event_type*/ None,
            "event rollout record is missing a string payload type discriminator".to_string(),
        ));
    }
    if !is_recall_reconstruction_record(&record_type, event_type.as_deref()) {
        return Ok(RecallLineProjection {
            line: None,
            ordinal,
            record_type,
            event_type,
        });
    }
    let line = serde_json::from_value(value).map_err(|error| {
        recall_source_issue(
            RecallRolloutSourceIssueKind::UnsupportedSchema,
            path,
            byte_offset,
            ordinal,
            Some(record_type.clone()),
            event_type.clone(),
            format!("reconstruction-relevant rollout record uses an unsupported schema: {error}"),
        )
    })?;
    Ok(RecallLineProjection {
        line: Some(line),
        ordinal,
        record_type,
        event_type,
    })
}

fn is_recall_reconstruction_record(record_type: &str, event_type: Option<&str>) -> bool {
    match record_type {
        "session_meta"
        | "response_item"
        | "inter_agent_communication"
        | "compacted"
        | "post_compact_recovery_applied"
        | "turn_context"
        | "world_state" => true,
        "event_msg" => matches!(
            event_type,
            Some(
                "thread_rolled_back"
                    | "task_started"
                    | "turn_started"
                    | "task_complete"
                    | "turn_complete"
                    | "turn_aborted"
                    | "user_message"
            )
        ),
        _ => false,
    }
}

fn recall_source_issue(
    kind: RecallRolloutSourceIssueKind,
    path: &Path,
    byte_offset: Option<u64>,
    ordinal: Option<u64>,
    record_type: Option<String>,
    event_type: Option<String>,
    message: String,
) -> RecallRolloutSourceIssue {
    RecallRolloutSourceIssue {
        kind,
        path: Some(path.to_path_buf()),
        line: None,
        byte_offset,
        ordinal,
        record_type,
        event_type,
        message,
    }
}

fn is_compressed_rollout(path: &std::path::Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".jsonl.zst"))
}

fn invalid_lineage(thread_id: ThreadId, detail: &str) -> ThreadStoreError {
    ThreadStoreError::InvalidRequest {
        message: format!("invalid paginated history lineage for {thread_id}: {detail}"),
    }
}
