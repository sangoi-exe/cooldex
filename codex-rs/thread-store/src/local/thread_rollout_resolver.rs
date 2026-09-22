//! Resolves a thread ID to the rollout file the thread currently uses.
//!
//! Most threads have one rollout file. `thread/revert` keeps the thread ID stable while switching
//! the thread to a new rollout file, so callers cannot infer the selected path from the thread ID
//! alone. This module centralizes live-writer, SQLite, and filesystem fallback resolution.

use std::io;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;

use codex_protocol::ThreadId;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_rollout::SourceByteLimitedSeekableReader;
use codex_rollout::find_archived_thread_path_by_id_str;
use codex_rollout::find_thread_path_by_id_str;

use super::LocalThreadStore;
use super::helpers::rollout_path_is_archived;
use super::live_writer;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;

/// One thread resolved to the concrete rollout file it currently uses.
///
/// For ordinary threads, `thread_id` and `rollout_id` are the same. After `thread/revert`,
/// `thread_id` stays stable while `rollout_id` identifies the new immutable rollout file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ResolvedThreadRollout {
    pub(super) thread_id: ThreadId,
    pub(super) rollout_id: ThreadId,
    pub(super) path: PathBuf,
    pub(super) location: RolloutLocation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RolloutLocation {
    Unarchived,
    Archived,
}

pub(super) async fn resolve_current(
    store: &LocalThreadStore,
    thread_id: ThreadId,
) -> ThreadStoreResult<Option<ResolvedThreadRollout>> {
    resolve(store, thread_id, LookupScope::ExcludeArchived).await
}

pub(super) async fn resolve_current_including_archived(
    store: &LocalThreadStore,
    thread_id: ThreadId,
) -> ThreadStoreResult<Option<ResolvedThreadRollout>> {
    resolve(store, thread_id, LookupScope::IncludeArchived).await
}

/// Mutable operation-level accounting for rollout metadata reads performed during resolution.
///
/// Tail readers share this with their later scanners so resolver-time reads consume the same
/// physical-byte and nonblank-record limits as the rest of the operation.
#[derive(Default)]
pub(super) struct RolloutReadBudget {
    max_bytes: u64,
    max_records: usize,
    bytes_read: u64,
    records_read: usize,
}

impl RolloutReadBudget {
    pub(super) fn new(max_bytes: u64, max_records: usize) -> Self {
        Self {
            max_bytes,
            max_records,
            ..Self::default()
        }
    }

    pub(super) fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    pub(super) fn records_read(&self) -> usize {
        self.records_read
    }

    fn remaining_bytes(&self) -> u64 {
        self.max_bytes.saturating_sub(self.bytes_read)
    }

    fn remaining_records(&self) -> usize {
        self.max_records.saturating_sub(self.records_read)
    }

    fn record(&mut self, accounting: MetadataReadAccounting) {
        self.bytes_read = self.bytes_read.saturating_add(accounting.bytes_read);
        self.records_read = self.records_read.saturating_add(accounting.records_read);
    }
}

/// Resolves a current rollout while charging every resolver metadata read to `budget`.
///
/// The ordinary resolver APIs retain their existing unbounded behavior for their direct callers.
pub(super) async fn resolve_current_with_read_budget(
    store: &LocalThreadStore,
    thread_id: ThreadId,
    include_archived: bool,
    budget: &mut RolloutReadBudget,
) -> ThreadStoreResult<Option<ResolvedThreadRollout>> {
    let scope = if include_archived {
        LookupScope::IncludeArchived
    } else {
        LookupScope::ExcludeArchived
    };
    resolve_with_budget(store, thread_id, scope, budget).await
}

#[derive(Clone, Copy)]
enum LookupScope {
    ExcludeArchived,
    IncludeArchived,
}

impl LookupScope {
    fn accepts(self, location: RolloutLocation) -> bool {
        match self {
            Self::ExcludeArchived => location == RolloutLocation::Unarchived,
            Self::IncludeArchived => true,
        }
    }
}

/// Resolves a thread's selected rollout in this order:
///
/// 1. The live writer, when one exists.
/// 2. SQLite's selected rollout path.
/// 3. Filesystem fallback for threads SQLite does not identify as paginated.
/// 4. Archived filesystem fallback, when requested.
async fn resolve(
    store: &LocalThreadStore,
    thread_id: ThreadId,
    scope: LookupScope,
) -> ThreadStoreResult<Option<ResolvedThreadRollout>> {
    resolve_inner(store, thread_id, scope, /*budget*/ None).await
}

async fn resolve_with_budget(
    store: &LocalThreadStore,
    thread_id: ThreadId,
    scope: LookupScope,
    budget: &mut RolloutReadBudget,
) -> ThreadStoreResult<Option<ResolvedThreadRollout>> {
    resolve_inner(store, thread_id, scope, Some(budget)).await
}

async fn resolve_inner(
    store: &LocalThreadStore,
    thread_id: ThreadId,
    scope: LookupScope,
    mut budget: Option<&mut RolloutReadBudget>,
) -> ThreadStoreResult<Option<ResolvedThreadRollout>> {
    if let Ok(path) = live_writer::rollout_path(store, thread_id).await
        && codex_rollout::existing_rollout_path(path.as_path())
            .await
            .is_some()
        && let Some(resolved) =
            resolve_path_in_scope(store, thread_id, path, scope, budget.as_deref_mut()).await?
    {
        return Ok(Some(resolved));
    }

    let state_db_ctx = store.state_db().await;
    if let Some(state_db_ctx) = state_db_ctx.as_deref() {
        match state_db_ctx.get_thread(thread_id).await {
            Ok(Some(metadata)) => {
                // Once SQLite identifies a thread as paginated, its rollout path is
                // authoritative: after `thread/revert`, a scan could find an older immutable
                // rollout for the same thread. Filesystem fallback remains available when SQLite
                // has no row or identifies the thread as legacy.
                if let Some(path) =
                    codex_rollout::existing_rollout_path(metadata.rollout_path.as_path()).await
                {
                    let belongs_to_thread =
                        match read_session_meta_line(path.as_path(), budget.as_deref_mut()).await {
                            Ok(session_meta) => session_meta.meta.id == thread_id,
                            Err(_) => true,
                        };
                    if belongs_to_thread {
                        return resolve_path_in_scope(
                            store,
                            thread_id,
                            path,
                            scope,
                            budget.as_deref_mut(),
                        )
                        .await;
                    }
                }
                if metadata.history_mode == ThreadHistoryMode::Paginated {
                    return Ok(None);
                }
            }
            Ok(None) => {}
            Err(err) => {
                return Err(ThreadStoreError::Internal {
                    message: format!("failed to read thread metadata for {thread_id}: {err}"),
                });
            }
        }
    }
    if let Some(path) = find_thread_path_by_id_str(
        store.config.codex_home.as_path(),
        &thread_id.to_string(),
        state_db_ctx.as_deref(),
    )
    .await
    .map_err(|err| ThreadStoreError::InvalidRequest {
        message: format!("failed to locate thread id {thread_id}: {err}"),
    })? && let Some(resolved) =
        resolve_path_in_scope(store, thread_id, path, scope, budget.as_deref_mut()).await?
    {
        return Ok(Some(resolved));
    }
    if !scope.accepts(RolloutLocation::Archived) {
        return Ok(None);
    }
    let path = find_archived_thread_path_by_id_str(
        store.config.codex_home.as_path(),
        &thread_id.to_string(),
        state_db_ctx.as_deref(),
    )
    .await
    .map_err(|err| ThreadStoreError::InvalidRequest {
        message: format!("failed to locate archived thread id {thread_id}: {err}"),
    })?;
    match path {
        Some(path) => {
            resolve_path_in_scope(store, thread_id, path, scope, budget.as_deref_mut()).await
        }
        None => Ok(None),
    }
}

async fn resolve_path_in_scope(
    store: &LocalThreadStore,
    thread_id: ThreadId,
    path: PathBuf,
    scope: LookupScope,
    budget: Option<&mut RolloutReadBudget>,
) -> ThreadStoreResult<Option<ResolvedThreadRollout>> {
    let location = location_for_path(store, path.as_path());
    if !scope.accepts(location) {
        return Ok(None);
    }
    resolve_path(thread_id, path, location, budget)
        .await
        .map(Some)
}

fn location_for_path(store: &LocalThreadStore, path: &std::path::Path) -> RolloutLocation {
    if rollout_path_is_archived(store.config.codex_home.as_path(), path) {
        RolloutLocation::Archived
    } else {
        RolloutLocation::Unarchived
    }
}

async fn resolve_path(
    thread_id: ThreadId,
    path: PathBuf,
    location: RolloutLocation,
    budget: Option<&mut RolloutReadBudget>,
) -> ThreadStoreResult<ResolvedThreadRollout> {
    let rollout_id = match codex_rollout::rollout_id_from_path(path.as_path()) {
        Some(rollout_id) => rollout_id,
        None => match read_session_meta_line(path.as_path(), budget).await {
            Ok(session_meta) => rollout_id_from_path_or_legacy_thread_id(
                path.as_path(),
                thread_id,
                session_meta.meta.history_mode,
            )?,
            // A tail with no remaining source budget cannot classify a legacy filename. Retain
            // the logical ID only until the bounded scanner reports an incomplete result; do not
            // make the resolver read beyond the operation limit.
            Err(err) if is_source_limit_exhausted(&err) => thread_id,
            Err(err) => {
                return Err(ThreadStoreError::Internal {
                    message: format!("failed to read session metadata {}: {err}", path.display()),
                });
            }
        },
    };
    Ok(ResolvedThreadRollout {
        thread_id,
        rollout_id,
        path,
        location,
    })
}

async fn read_session_meta_line(
    path: &Path,
    budget: Option<&mut RolloutReadBudget>,
) -> io::Result<codex_protocol::protocol::SessionMetaLine> {
    let Some(budget) = budget else {
        return codex_rollout::read_session_meta_line(path).await;
    };
    let path = path.to_path_buf();
    let max_bytes = budget.remaining_bytes();
    let max_records = budget.remaining_records();
    let read = tokio::task::spawn_blocking(move || {
        read_session_meta_line_with_limits(path.as_path(), max_bytes, max_records)
    })
    .await
    .map_err(io::Error::other)?;
    budget.record(read.accounting);
    read.result
}

struct MetadataReadResult {
    result: io::Result<codex_protocol::protocol::SessionMetaLine>,
    accounting: MetadataReadAccounting,
}

#[derive(Default)]
struct MetadataReadAccounting {
    bytes_read: u64,
    records_read: usize,
}

fn read_session_meta_line_with_limits(
    path: &Path,
    max_source_bytes: u64,
    max_records: usize,
) -> MetadataReadResult {
    if max_source_bytes == 0 || max_records == 0 {
        return MetadataReadResult {
            result: Err(source_limit_exhausted()),
            accounting: MetadataReadAccounting::default(),
        };
    }
    match codex_rollout::open_rollout_seekable_reader_with_source_byte_limit(path, max_source_bytes)
    {
        Ok(SourceByteLimitedSeekableReader::Direct(file)) => {
            let source_len = match file.metadata() {
                Ok(metadata) => metadata.len(),
                Err(err) => {
                    return MetadataReadResult {
                        result: Err(err),
                        accounting: MetadataReadAccounting::default(),
                    };
                }
            };
            let mut reader =
                BufReader::new(ByteLimitedReader::new(file, max_source_bytes, source_len));
            let result = read_session_meta_from_reader(&mut reader, max_records);
            MetadataReadResult {
                result,
                accounting: MetadataReadAccounting {
                    bytes_read: reader.get_ref().bytes_read,
                    records_read: reader.get_ref().records_read,
                },
            }
        }
        Ok(SourceByteLimitedSeekableReader::Decoded {
            file,
            source_bytes_read,
        }) => {
            let mut reader = BufReader::new(RecordCountingReader::new(file));
            let result = read_session_meta_from_reader(&mut reader, max_records);
            MetadataReadResult {
                result,
                accounting: MetadataReadAccounting {
                    bytes_read: source_bytes_read,
                    records_read: reader.get_ref().records_read,
                },
            }
        }
        Ok(SourceByteLimitedSeekableReader::LimitExceeded) => MetadataReadResult {
            result: Err(source_limit_exhausted()),
            accounting: MetadataReadAccounting::default(),
        },
        Err(err) => MetadataReadResult {
            result: Err(err),
            accounting: MetadataReadAccounting::default(),
        },
    }
}

fn read_session_meta_from_reader<R>(
    reader: &mut BufReader<R>,
    max_records: usize,
) -> io::Result<codex_protocol::protocol::SessionMetaLine>
where
    R: Read + RecordReadAccounting,
{
    let mut line = Vec::new();
    loop {
        if reader.get_ref().records_read() == max_records {
            return Err(source_limit_exhausted());
        }
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 {
            if reader.get_ref().source_limit_reached() {
                return Err(source_limit_exhausted());
            }
            return Err(io::Error::other("rollout metadata source is empty"));
        }
        if reader.get_ref().source_limit_reached() && !line.ends_with(b"\n") {
            return Err(source_limit_exhausted());
        }
        let trimmed = trim_ascii_whitespace(line.as_slice());
        if trimmed.is_empty() {
            if source_limit_exhausted_without_buffered_records(reader) {
                return Err(source_limit_exhausted());
            }
            continue;
        }
        reader.get_mut().record_nonblank();
        let Ok(rollout_line) = codex_rollout::parse_rollout_line_bytes(trimmed) else {
            if source_limit_exhausted_without_buffered_records(reader) {
                return Err(source_limit_exhausted());
            }
            continue;
        };
        match rollout_line.item {
            codex_rollout::RolloutItem::SessionMeta(session_meta_line) => {
                return Ok(session_meta_line);
            }
            codex_rollout::RolloutItem::ResponseItem(_)
            | codex_rollout::RolloutItem::InterAgentCommunication(_) => {
                return Err(io::Error::other(
                    "rollout does not start with session metadata",
                ));
            }
            _ if source_limit_exhausted_without_buffered_records(reader) => {
                return Err(source_limit_exhausted());
            }
            _ => {}
        }
    }
}

fn source_limit_exhausted_without_buffered_records<R>(reader: &BufReader<R>) -> bool
where
    R: RecordReadAccounting,
{
    reader.get_ref().source_limit_reached() && reader.buffer().is_empty()
}

fn trim_ascii_whitespace(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);
    &bytes[start..end]
}

fn source_limit_exhausted() -> io::Error {
    io::Error::other(MetadataSourceLimitExhausted)
}

fn is_source_limit_exhausted(err: &io::Error) -> bool {
    err.get_ref()
        .is_some_and(|source| source.is::<MetadataSourceLimitExhausted>())
}

#[derive(Debug)]
struct MetadataSourceLimitExhausted;

impl std::fmt::Display for MetadataSourceLimitExhausted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("rollout metadata source limit exhausted")
    }
}

impl std::error::Error for MetadataSourceLimitExhausted {}

trait RecordReadAccounting {
    fn record_nonblank(&mut self);
    fn records_read(&self) -> usize;
    fn source_limit_reached(&self) -> bool;
}

struct ByteLimitedReader<R> {
    inner: R,
    remaining: u64,
    source_len: u64,
    bytes_read: u64,
    records_read: usize,
}

impl<R> ByteLimitedReader<R> {
    fn new(inner: R, max_bytes: u64, source_len: u64) -> Self {
        Self {
            inner,
            remaining: max_bytes,
            source_len,
            bytes_read: 0,
            records_read: 0,
        }
    }
}

impl<R: Read> Read for ByteLimitedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read_len = buffer
            .len()
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        if read_len == 0 {
            return Ok(0);
        }
        let read = self.inner.read(&mut buffer[..read_len])?;
        self.remaining = self.remaining.saturating_sub(read as u64);
        self.bytes_read = self.bytes_read.saturating_add(read as u64);
        Ok(read)
    }
}

impl<R> RecordReadAccounting for ByteLimitedReader<R> {
    fn record_nonblank(&mut self) {
        self.records_read += 1;
    }

    fn records_read(&self) -> usize {
        self.records_read
    }

    fn source_limit_reached(&self) -> bool {
        self.remaining == 0 && self.bytes_read < self.source_len
    }
}

struct RecordCountingReader<R> {
    inner: R,
    records_read: usize,
}

impl<R> RecordCountingReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            records_read: 0,
        }
    }
}

impl<R: Read> Read for RecordCountingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buffer)
    }
}

impl<R> RecordReadAccounting for RecordCountingReader<R> {
    fn record_nonblank(&mut self) {
        self.records_read += 1;
    }

    fn records_read(&self) -> usize {
        self.records_read
    }

    fn source_limit_reached(&self) -> bool {
        false
    }
}

/// Returns the immutable rollout ID for a path while preserving legacy noncanonical filenames.
pub(super) fn rollout_id_from_path_or_legacy_thread_id(
    path: &std::path::Path,
    thread_id: ThreadId,
    history_mode: ThreadHistoryMode,
) -> ThreadStoreResult<ThreadId> {
    Ok(match codex_rollout::rollout_id_from_path(path) {
        Some(rollout_id) => rollout_id,
        None => {
            if history_mode == ThreadHistoryMode::Paginated {
                return Err(ThreadStoreError::InvalidRequest {
                    message: format!(
                        "paginated rollout path `{}` does not have a canonical rollout filename",
                        path.display()
                    ),
                });
            }
            thread_id
        }
    })
}
