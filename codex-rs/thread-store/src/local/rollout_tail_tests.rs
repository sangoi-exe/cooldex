use std::fs;
use std::fs::OpenOptions;
use std::io::Write;

use codex_history::RolloutItem;
use codex_history::RolloutLine;
use codex_protocol::ThreadId;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::HistoryPosition;
use codex_protocol::protocol::ThreadHistoryMode;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use uuid::Uuid;

use super::*;
use crate::ThreadStore;
use crate::local::test_support::test_config;
use crate::local::test_support::write_session_file_with_history_mode;

fn message(text: &str) -> RolloutItem {
    RolloutItem::ResponseItem(
        ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: text.to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }
        .into(),
    )
}

fn append_line(path: &std::path::Path, ordinal: u64, item: RolloutItem) {
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open rollout");
    let line = RolloutLine {
        timestamp: "2025-01-03T13:00:00Z".to_string(),
        ordinal: Some(ordinal),
        item,
    };
    writeln!(
        file,
        "{}",
        serde_json::to_string(&line).expect("serialize rollout line")
    )
    .expect("append rollout line");
}

fn append_value(path: &std::path::Path, value: serde_json::Value) -> u64 {
    let byte_offset = fs::metadata(path).expect("rollout metadata").len();
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open rollout");
    writeln!(file, "{value}").expect("append rollout value");
    byte_offset
}

fn compress_rollout(path: &std::path::Path) -> u64 {
    let source = fs::read(path).expect("read rollout before compression");
    let compressed =
        zstd::stream::encode_all(source.as_slice(), /*level*/ 3).expect("compress rollout");
    fs::write(path.with_extension("jsonl.zst"), &compressed).expect("write compressed rollout");
    fs::remove_file(path).expect("remove plain rollout");
    u64::try_from(compressed.len()).expect("compressed rollout size")
}

#[tokio::test]
async fn loads_complete_rollout_in_replay_order() {
    let home = TempDir::new().expect("temp dir");
    let uuid = Uuid::from_u128(/*v*/ 3001);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("thread id");
    let path = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T13-02-00",
        uuid,
        ThreadHistoryMode::Paginated,
    )
    .expect("write rollout");
    append_line(path.as_path(), 1, message("first"));
    append_line(path.as_path(), 2, message("second"));
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);

    let tail = store
        .load_rollout_tail(LoadRolloutTailParams {
            thread_id,
            include_archived: false,
            max_bytes: 1024 * 1024,
            max_records: 16,
        })
        .await
        .expect("load rollout tail");

    assert_eq!(
        serde_json::to_value(tail.items).expect("serialize actual tail"),
        serde_json::to_value(vec![message("first"), message("second")])
            .expect("serialize expected tail")
    );
    assert!(tail.reached_start);
    assert_eq!(tail.records_read, 3);
    assert_eq!(tail.segments_read, 1);
    assert!(tail.bytes_read > 0);
}

#[tokio::test]
async fn loads_legacy_fork_with_copied_source_session_metadata() {
    let home = TempDir::new().expect("temp dir");
    let parent_uuid = Uuid::from_u128(/*v*/ 3020);
    let parent_path = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T13-04-00",
        parent_uuid,
        ThreadHistoryMode::Legacy,
    )
    .expect("write parent rollout");
    let parent_meta = fs::read_to_string(parent_path)
        .expect("read parent rollout")
        .lines()
        .next()
        .map(|line| serde_json::from_str::<RolloutLine>(line).expect("parse parent metadata"))
        .map(|line| line.item)
        .expect("parent metadata line");

    let child_uuid = Uuid::from_u128(/*v*/ 3021);
    let child_id = ThreadId::from_string(&child_uuid.to_string()).expect("child id");
    let child_path = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T13-04-01",
        child_uuid,
        ThreadHistoryMode::Legacy,
    )
    .expect("write child rollout");
    let mut expected_items = fs::read_to_string(child_path.as_path())
        .expect("read child rollout")
        .lines()
        .skip(1)
        .map(|line| serde_json::from_str::<RolloutLine>(line).expect("parse child rollout item"))
        .map(|line| line.item)
        .collect::<Vec<_>>();
    append_line(child_path.as_path(), 1, parent_meta);
    append_line(child_path.as_path(), 2, message("child visible"));
    expected_items.push(message("child visible"));
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);

    let tail = store
        .load_rollout_tail(LoadRolloutTailParams {
            thread_id: child_id,
            include_archived: false,
            max_bytes: 1024 * 1024,
            max_records: 16,
        })
        .await
        .expect("load legacy fork tail");

    assert_eq!(
        serde_json::to_value(tail.items).expect("serialize actual tail"),
        serde_json::to_value(expected_items).expect("serialize expected tail")
    );
    assert!(tail.reached_start);
    assert_eq!(tail.records_read, 4);
    assert_eq!(tail.segments_read, 1);
}

#[tokio::test]
async fn loads_frozen_parent_lineage_before_the_child_delta() {
    let home = TempDir::new().expect("temp dir");
    let parent_uuid = Uuid::from_u128(/*v*/ 3010);
    let parent_id = ThreadId::from_string(&parent_uuid.to_string()).expect("parent id");
    let parent_path = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T13-03-00",
        parent_uuid,
        ThreadHistoryMode::Paginated,
    )
    .expect("write parent rollout");
    append_line(parent_path.as_path(), 1, message("parent first"));
    append_line(parent_path.as_path(), 2, message("parent frozen"));
    let parent_end = HistoryPosition {
        thread_id: parent_id,
        end_ordinal_exclusive: 3,
        end_byte_offset: fs::metadata(parent_path.as_path())
            .expect("parent metadata")
            .len(),
    };

    let child_uuid = Uuid::from_u128(/*v*/ 3011);
    let child_id = ThreadId::from_string(&child_uuid.to_string()).expect("child id");
    let child_path = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T13-03-01",
        child_uuid,
        ThreadHistoryMode::Paginated,
    )
    .expect("write child rollout");
    set_history_base(child_path.as_path(), parent_end);
    append_line(child_path.as_path(), 1, message("child delta"));
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);

    let tail = store
        .load_rollout_tail(LoadRolloutTailParams {
            thread_id: child_id,
            include_archived: false,
            max_bytes: 1024 * 1024,
            max_records: 16,
        })
        .await
        .expect("load fork lineage");

    assert_eq!(
        serde_json::to_value(tail.items).expect("serialize actual lineage"),
        serde_json::to_value(vec![
            message("parent first"),
            message("parent frozen"),
            message("child delta"),
        ])
        .expect("serialize expected lineage")
    );
    assert!(tail.reached_start);
    assert_eq!(tail.records_read, 5);
    assert_eq!(tail.segments_read, 2);
}

#[tokio::test]
async fn loads_compressed_current_rollout_with_physical_byte_accounting() {
    let home = TempDir::new().expect("temp dir");
    let uuid = Uuid::from_u128(/*v*/ 3022);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("thread id");
    let path = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T13-04-02",
        uuid,
        ThreadHistoryMode::Paginated,
    )
    .expect("write rollout");
    append_line(path.as_path(), 1, message("compressed first"));
    append_line(path.as_path(), 2, message("compressed second"));
    let compressed_bytes = compress_rollout(path.as_path());
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);

    let params = LoadRolloutTailParams {
        thread_id,
        include_archived: false,
        max_bytes: compressed_bytes,
        max_records: 16,
    };
    let strict = store
        .load_rollout_tail(params.clone())
        .await
        .expect("load strict compressed rollout tail");
    let recall = store
        .load_recall_rollout_tail(params)
        .await
        .expect("load recall compressed rollout tail");

    let expected = serde_json::to_value(vec![
        message("compressed first"),
        message("compressed second"),
    ])
    .expect("serialize expected items");
    assert_eq!(
        serde_json::to_value(&strict.items).expect("serialize strict items"),
        expected
    );
    assert_eq!(
        serde_json::to_value(&recall.items).expect("serialize recall items"),
        expected
    );
    assert!(strict.reached_start);
    assert!(recall.reached_start);
    assert_eq!(strict.bytes_read, compressed_bytes);
    assert_eq!(recall.bytes_read, compressed_bytes);
    assert_eq!(strict.records_read, 3);
    assert_eq!(recall.records_read, 3);
    assert_eq!(strict.segments_read, 1);
    assert_eq!(recall.segments_read, 1);
    assert_eq!(recall.source_issue, None);

    let limited = store
        .load_recall_rollout_tail(LoadRolloutTailParams {
            thread_id,
            include_archived: false,
            max_bytes: compressed_bytes - 1,
            max_records: 16,
        })
        .await
        .expect("report compressed source byte limit");
    assert!(limited.items.is_empty());
    assert!(!limited.reached_start);
    assert_eq!(limited.bytes_read, 0);
    assert_eq!(limited.records_read, 0);
    assert_eq!(limited.segments_read, 1);
    assert_eq!(limited.source_issue, None);
}

#[tokio::test]
async fn loads_plain_child_with_compressed_parent_lineage() {
    let home = TempDir::new().expect("temp dir");
    let parent_uuid = Uuid::from_u128(/*v*/ 3023);
    let parent_id = ThreadId::from_string(&parent_uuid.to_string()).expect("parent id");
    let parent_path = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T13-04-03",
        parent_uuid,
        ThreadHistoryMode::Paginated,
    )
    .expect("write parent rollout");
    append_line(parent_path.as_path(), 1, message("compressed parent"));
    let parent_end = HistoryPosition {
        thread_id: parent_id,
        end_ordinal_exclusive: 2,
        end_byte_offset: fs::metadata(parent_path.as_path())
            .expect("parent metadata")
            .len(),
    };

    let child_uuid = Uuid::from_u128(/*v*/ 3024);
    let child_id = ThreadId::from_string(&child_uuid.to_string()).expect("child id");
    let child_path = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T13-04-04",
        child_uuid,
        ThreadHistoryMode::Paginated,
    )
    .expect("write child rollout");
    set_history_base(child_path.as_path(), parent_end);
    append_line(child_path.as_path(), 1, message("plain child"));
    let child_bytes = fs::metadata(child_path.as_path())
        .expect("child metadata")
        .len();
    let compressed_parent_bytes = compress_rollout(parent_path.as_path());
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);

    let tail = store
        .load_recall_rollout_tail(LoadRolloutTailParams {
            thread_id: child_id,
            include_archived: false,
            max_bytes: child_bytes + compressed_parent_bytes,
            max_records: 16,
        })
        .await
        .expect("load compressed parent lineage");

    assert_eq!(
        serde_json::to_value(&tail.items).expect("serialize actual lineage"),
        serde_json::to_value(vec![message("compressed parent"), message("plain child")])
            .expect("serialize expected lineage")
    );
    assert!(tail.reached_start);
    assert_eq!(tail.bytes_read, child_bytes + compressed_parent_bytes);
    assert_eq!(tail.records_read, 4);
    assert_eq!(tail.segments_read, 2);
    assert_eq!(tail.source_issue, None);

    let limited = store
        .load_recall_rollout_tail(LoadRolloutTailParams {
            thread_id: child_id,
            include_archived: false,
            max_bytes: child_bytes + compressed_parent_bytes - 1,
            max_records: 16,
        })
        .await
        .expect("report compressed ancestor source byte limit");
    assert_eq!(
        serde_json::to_value(&limited.items).expect("serialize limited lineage"),
        serde_json::to_value(vec![message("plain child")])
            .expect("serialize expected limited lineage")
    );
    assert!(!limited.reached_start);
    assert_eq!(limited.bytes_read, child_bytes);
    assert_eq!(limited.records_read, 2);
    assert_eq!(limited.segments_read, 2);
    assert_eq!(limited.source_issue, None);
}

#[tokio::test]
async fn reports_byte_limited_incomplete_source_without_parsing_a_partial_record() {
    let home = TempDir::new().expect("temp dir");
    let uuid = Uuid::from_u128(/*v*/ 3002);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("thread id");
    let path = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T13-02-01",
        uuid,
        ThreadHistoryMode::Paginated,
    )
    .expect("write rollout");
    append_line(path.as_path(), 1, message(&"x".repeat(256)));
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);

    let tail = store
        .load_rollout_tail(LoadRolloutTailParams {
            thread_id,
            include_archived: false,
            max_bytes: 32,
            max_records: 16,
        })
        .await
        .expect("bounded partial read");

    assert!(tail.items.is_empty());
    assert!(!tail.reached_start);
    assert_eq!(tail.bytes_read, 32);
    assert_eq!(tail.records_read, 0);
    assert_eq!(tail.segments_read, 1);
}

#[tokio::test]
async fn reports_record_limited_newest_complete_record() {
    let home = TempDir::new().expect("temp dir");
    let uuid = Uuid::from_u128(/*v*/ 3004);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("thread id");
    let path = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T13-02-03",
        uuid,
        ThreadHistoryMode::Paginated,
    )
    .expect("write rollout");
    append_line(path.as_path(), 1, message("older"));
    append_line(path.as_path(), 2, message("newest"));
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);

    let tail = store
        .load_rollout_tail(LoadRolloutTailParams {
            thread_id,
            include_archived: false,
            max_bytes: 1024 * 1024,
            max_records: 1,
        })
        .await
        .expect("record-bounded tail");

    assert_eq!(
        serde_json::to_value(tail.items).expect("serialize actual tail"),
        serde_json::to_value(vec![message("newest")]).expect("serialize expected tail")
    );
    assert!(!tail.reached_start);
    assert_eq!(tail.records_read, 1);
    assert_eq!(tail.segments_read, 1);
}

#[tokio::test]
async fn rejects_malformed_rollout_records() {
    let home = TempDir::new().expect("temp dir");
    let uuid = Uuid::from_u128(/*v*/ 3003);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("thread id");
    let path = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T13-02-02",
        uuid,
        ThreadHistoryMode::Paginated,
    )
    .expect("write rollout");
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open rollout");
    writeln!(file, "not-json").expect("append malformed line");
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);

    let err = store
        .load_rollout_tail(LoadRolloutTailParams {
            thread_id,
            include_archived: false,
            max_bytes: 1024 * 1024,
            max_records: 16,
        })
        .await
        .expect_err("malformed record should fail");

    assert!(matches!(err, ThreadStoreError::Internal { .. }));
    assert!(err.to_string().contains("rejected rollout record"));
}

#[tokio::test]
async fn recall_projection_ignores_redacted_historical_token_count_record() {
    let home = TempDir::new().expect("temp dir");
    let uuid = Uuid::from_u128(/*v*/ 3006);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("thread id");
    let path = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T13-02-05",
        uuid,
        ThreadHistoryMode::Paginated,
    )
    .expect("write rollout");
    append_value(
        path.as_path(),
        // Redacted from the historical token-count record that reproduced the
        // strict flattened RolloutLine deserialization failure in the operator rollout.
        serde_json::json!({
            "timestamp": "2025-01-03T13:00:01Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "total_token_usage": {
                        "input_tokens": 100,
                        "cached_input_tokens": 80,
                        "cache_write_input_tokens": 0,
                        "output_tokens": 20,
                        "reasoning_output_tokens": 10,
                        "total_tokens": 120
                    },
                    "last_token_usage": {
                        "input_tokens": 10,
                        "cached_input_tokens": 8,
                        "cache_write_input_tokens": 0,
                        "output_tokens": 2,
                        "reasoning_output_tokens": 1,
                        "total_tokens": 12
                    },
                    "model_context_window": 272000
                },
                "rate_limits": {
                    "limit_id": "redacted",
                    "limit_name": null,
                    "primary": {
                        "used_percent": 97.5,
                        "window_minutes": 300,
                        "resets_at": 1
                    },
                    "secondary": null,
                    "credits": {
                        "has_credits": true,
                        "unlimited": false,
                        "balance": "redacted"
                    },
                    "individual_limit": null,
                    "spend_control_reached": null,
                    "plan_type": "pro",
                    "rate_limit_reached_type": null
                }
            }
        }),
    );
    append_line(path.as_path(), 2, message("relevant"));
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);

    let strict_error = store
        .load_rollout_tail(LoadRolloutTailParams {
            thread_id,
            include_archived: false,
            max_bytes: 1024 * 1024,
            max_records: 16,
        })
        .await
        .expect_err("strict reader should retain full-schema validation");
    assert!(strict_error.to_string().contains("expected f64"));
    assert!(strict_error.to_string().contains("byte offset"));

    let projected = store
        .load_recall_rollout_tail(LoadRolloutTailParams {
            thread_id,
            include_archived: false,
            max_bytes: 1024 * 1024,
            max_records: 16,
        })
        .await
        .expect("load recall projection");

    assert_eq!(
        serde_json::to_value(projected.items).expect("serialize projected items"),
        serde_json::to_value(vec![message("relevant")]).expect("serialize expected items")
    );
    assert!(projected.reached_start);
    assert_eq!(projected.records_read, 3);
    assert_eq!(projected.source_issue, None);
}

#[tokio::test]
async fn recall_projection_reports_relevant_historical_schema_drift() {
    let home = TempDir::new().expect("temp dir");
    let uuid = Uuid::from_u128(/*v*/ 3007);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("thread id");
    let path = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T13-02-06",
        uuid,
        ThreadHistoryMode::Paginated,
    )
    .expect("write rollout");
    let byte_offset = append_value(
        path.as_path(),
        serde_json::json!({
            "timestamp": "2025-01-03T13:00:01Z",
            "ordinal": 7,
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": {},
                "content": []
            }
        }),
    );
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);

    let projected = store
        .load_recall_rollout_tail(LoadRolloutTailParams {
            thread_id,
            include_archived: false,
            max_bytes: 1024 * 1024,
            max_records: 16,
        })
        .await
        .expect("schema drift should be reported as projection data");
    let issue = projected.source_issue.expect("projected source issue");

    assert_eq!(
        issue.kind,
        crate::RecallRolloutSourceIssueKind::UnsupportedSchema
    );
    assert_eq!(issue.path.as_deref(), Some(path.as_path()));
    assert_eq!(issue.line, None);
    assert_eq!(issue.byte_offset, Some(byte_offset));
    assert_eq!(issue.ordinal, Some(7));
    assert_eq!(issue.record_type.as_deref(), Some("response_item"));
    assert_eq!(issue.event_type, None);
    assert!(issue.message.contains("unsupported schema"));
}

#[test]
#[ignore = "requires CODEX_RECALL_SMOKE_ROLLOUT_COPY"]
fn recall_projection_smoke_reads_an_operator_supplied_rollout_copy() {
    let path = std::env::var_os("CODEX_RECALL_SMOKE_ROLLOUT_COPY")
        .map(std::path::PathBuf::from)
        .expect("CODEX_RECALL_SMOKE_ROLLOUT_COPY must name a copied rollout");

    let strict_error = match scan_strict_rollout_segment(
        path.as_path(),
        /*end*/ None,
        16 * 1024 * 1024,
        8_192,
    ) {
        Ok(_) => {
            panic!("the operator-supplied copy should preserve the strict-reader regression")
        }
        Err(error) => error,
    };
    assert!(strict_error.to_string().contains("expected f64"));
    assert!(strict_error.to_string().contains("byte offset"));

    let scan =
        scan_recall_rollout_segment(path.as_path(), /*end*/ None, 16 * 1024 * 1024, 8_192).expect(
            "bounded recall projection should not fail on an operator-supplied rollout copy",
        );

    assert!(scan.records_read > 0);
    assert_eq!(scan.source_issue, None);
}

#[tokio::test]
async fn rejects_rollout_whose_physical_first_record_is_not_session_metadata() {
    let home = TempDir::new().expect("temp dir");
    let uuid = Uuid::from_u128(/*v*/ 3005);
    let thread_id = ThreadId::from_string(&uuid.to_string()).expect("thread id");
    let path = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T13-02-04",
        uuid,
        ThreadHistoryMode::Paginated,
    )
    .expect("write rollout");
    let canonical_meta = fs::read_to_string(path.as_path())
        .expect("read rollout")
        .lines()
        .next()
        .map(str::to_string)
        .expect("session metadata line");
    let misplaced_message = RolloutLine {
        timestamp: "2025-01-03T13:00:00Z".to_string(),
        ordinal: Some(1),
        item: message("misplaced head"),
    };
    fs::write(
        path.as_path(),
        format!(
            "{}\n{canonical_meta}\n",
            serde_json::to_string(&misplaced_message).expect("serialize misplaced message")
        ),
    )
    .expect("write malformed rollout");
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);

    let err = store
        .load_rollout_tail(LoadRolloutTailParams {
            thread_id,
            include_archived: false,
            max_bytes: 1024 * 1024,
            max_records: 16,
        })
        .await
        .expect_err("non-metadata head should fail");

    assert!(matches!(err, ThreadStoreError::Internal { .. }));
    assert!(
        err.to_string()
            .contains("session metadata is not the first record")
    );
}

fn set_history_base(path: &std::path::Path, history_base: HistoryPosition) {
    let contents = fs::read_to_string(path).expect("read child rollout");
    let (head, tail) = contents.split_once('\n').expect("session metadata line");
    let mut line: RolloutLine = serde_json::from_str(head).expect("parse session metadata line");
    let RolloutItem::SessionMeta(session_meta) = &mut line.item else {
        panic!("expected session metadata head");
    };
    session_meta.meta.history_base = Some(history_base);
    fs::write(
        path,
        format!(
            "{}\n{tail}",
            serde_json::to_string(&line).expect("serialize session metadata line")
        ),
    )
    .expect("rewrite child metadata");
}
