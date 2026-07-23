use std::collections::HashSet;
use std::fs::File;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use codex_protocol::ThreadId;
use codex_protocol::protocol::HistoryPosition;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::RolloutLine;
use codex_protocol::protocol::SessionMetaLine;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_rollout::ReverseJsonlScanner;
use codex_rollout::ScanOutcome;

use super::LocalThreadStore;
use super::read_thread;
use super::rollout_lineage::validate_cutoff_bounds;
use crate::LoadRolloutTailParams;
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
    let path = read_thread::resolve_rollout_path(store, params.thread_id, params.include_archived)
        .await?
        .ok_or_else(|| ThreadStoreError::InvalidRequest {
            message: format!("no rollout found for thread id {}", params.thread_id),
        })?;
    scan_rollout_tail(store, path, params).await
}

async fn scan_rollout_tail(
    store: &LocalThreadStore,
    initial_path: PathBuf,
    params: LoadRolloutTailParams,
) -> ThreadStoreResult<StoredRolloutTail> {
    let mut items_newest_first = Vec::new();
    let mut bytes_read = 0_u64;
    let mut records_read = 0_usize;
    let mut segments_read = 0_usize;
    let mut reached_start = false;
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
                segment_path = read_thread::resolve_rollout_path(
                    store,
                    segment_thread_id,
                    /*include_archived*/ true,
                )
                .await?
                .ok_or_else(|| invalid_lineage(segment_thread_id, "missing source rollout"))?;
            }
        }
    }

    items_newest_first.reverse();
    Ok(StoredRolloutTail {
        thread_id: params.thread_id,
        items: items_newest_first,
        reached_start,
        bytes_read,
        records_read,
        segments_read,
    })
}

struct SegmentScan {
    items_newest_first: Vec<RolloutItem>,
    session_meta: Option<SessionMetaLine>,
    reached_start: bool,
    bytes_read: u64,
    records_read: usize,
}

fn scan_rollout_segment(
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
                    format!("rejected rollout record in {}: {err}", path.display()),
                ));
            }
        };
        match line.item {
            RolloutItem::SessionMeta(meta) => {
                if session_meta.replace(meta).is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("duplicate session metadata in {}", path.display()),
                    ));
                }
            }
            _ if session_meta.is_some() => {
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
    })
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
