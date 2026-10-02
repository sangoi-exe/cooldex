use std::fs;
use std::io::Write;
use std::path::Path;

use codex_protocol::ThreadId;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::HistoryPosition;
use codex_protocol::protocol::SessionMeta;
use codex_protocol::protocol::SessionMetaLine;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_protocol::protocol::TurnStartedEvent;
use codex_rollout::RolloutItem;
use codex_rollout::RolloutLine;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::super::LocalThreadStore;
use super::super::test_support::test_config;
use super::RolloutLineageSegment;
use crate::LoadThreadHistoryParams;
use crate::StoredThreadHistory;
use crate::ThreadStoreError;

#[tokio::test]
async fn load_history_replays_nested_archived_lineage_with_frozen_ordinals() {
    let home = TempDir::new().expect("temp dir");
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
    let root = ThreadId::default();
    let middle = ThreadId::default();
    let child = ThreadId::default();
    let root_item = turn_started("root");
    let middle_item = turn_started("middle");
    let child_item = turn_started("child");
    let root_path = write_rollout_under(
        home.path().join("archived_sessions"),
        root,
        /*history_base*/ None,
        /*next_ordinal*/ 1,
    );
    append_rollout_lines(
        root_path.as_path(),
        [
            (1, root_item.clone()),
            (7, turn_started("excluded root suffix")),
        ],
    );
    let root_end = history_position(root_path.as_path(), root, /*end_ordinal_exclusive*/ 4);
    let middle_path = write_rollout(home.path(), middle, Some(root_end), /*next_ordinal*/ 1);
    append_rollout_lines(
        middle_path.as_path(),
        [
            (6, middle_item.clone()),
            (9, turn_started("excluded middle suffix")),
        ],
    );
    let middle_end = history_position(
        middle_path.as_path(),
        middle,
        /*end_ordinal_exclusive*/ 7,
    );
    let child_path = write_rollout(
        home.path(),
        child,
        Some(middle_end),
        /*next_ordinal*/ 1,
    );
    append_rollout_lines(child_path.as_path(), [(8, child_item.clone())]);
    let session_meta = codex_rollout::read_session_meta_line(child_path.as_path())
        .await
        .expect("read current session metadata");
    let expected = StoredThreadHistory {
        thread_id: child,
        revision: None,
        items: vec![
            RolloutItem::SessionMeta(session_meta),
            root_item,
            middle_item,
            child_item,
        ],
    };

    let history = store
        .load_history(LoadThreadHistoryParams {
            thread_id: child,
            include_archived: false,
        })
        .await
        .expect("load complete history with archived ancestry");
    assert_eq!(
        serde_json::to_value(history).expect("serialize history"),
        serde_json::to_value(&expected).expect("serialize expected history")
    );

    let archived_path = home
        .path()
        .join("archived_sessions")
        .join(child_path.file_name().expect("child rollout filename"));
    fs::rename(child_path, &archived_path).expect("archive current rollout");
    let err = store
        .load_history(LoadThreadHistoryParams {
            thread_id: child,
            include_archived: false,
        })
        .await
        .expect_err("active-only history must reject an archived current rollout");
    assert!(matches!(err, ThreadStoreError::InvalidRequest { .. }));
    let history = store
        .load_history(LoadThreadHistoryParams {
            thread_id: child,
            include_archived: true,
        })
        .await
        .expect("load archived complete history");
    assert_eq!(
        serde_json::to_value(history).expect("serialize archived history"),
        serde_json::to_value(&expected).expect("serialize expected history")
    );

    for path in [&root_path, &middle_path, &archived_path] {
        let input = fs::File::open(path).expect("open rollout");
        let output =
            fs::File::create(path.with_extension("jsonl.zst")).expect("create compressed rollout");
        zstd::stream::copy_encode(input, output, /*level*/ 3).expect("compress rollout");
        fs::remove_file(path).expect("remove plain rollout");
    }
    let history = store
        .load_history(LoadThreadHistoryParams {
            thread_id: child,
            include_archived: true,
        })
        .await
        .expect("load complete history from compressed lineage");
    assert_eq!(
        serde_json::to_value(history).expect("serialize compressed history"),
        serde_json::to_value(expected).expect("serialize expected history")
    );
    assert!(
        [root_path, middle_path, archived_path]
            .iter()
            .all(|path| !path.exists())
    );
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_reference_lineage_escaping_symlinked_sessions_root() {
    let home = TempDir::new().expect("temp dir");
    let external = TempDir::new().expect("external temp dir");
    let external_sessions = external.path().join("sessions");
    fs::create_dir_all(external_sessions.as_path()).expect("create external sessions");
    std::os::unix::fs::symlink(external_sessions.as_path(), home.path().join("sessions"))
        .expect("symlink sessions");
    let escaped = TempDir::new().expect("escaped temp dir");
    let escaped_rollouts = escaped.path().join("rollouts");
    fs::create_dir_all(escaped_rollouts.as_path()).expect("create escaped rollouts");
    std::os::unix::fs::symlink(escaped_rollouts.as_path(), external_sessions.join("2026"))
        .expect("symlink nested sessions directory");
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
    let thread_id = ThreadId::default();
    write_rollout(
        home.path(),
        thread_id,
        /*history_base*/ None,
        /*next_ordinal*/ 3,
    );

    let error = store
        .resolve_rollout_lineage_for_reference(thread_id)
        .await
        .expect_err("escaping reference lineage should be rejected");

    assert!(error.to_string().contains("must be in Codex home"));
}

#[tokio::test]
async fn resolves_nested_lineage_with_empty_intermediate_segments() {
    let home = TempDir::new().expect("temp dir");
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
    let root = ThreadId::default();
    let middle = ThreadId::default();
    let child = ThreadId::default();
    let root_path = write_rollout(
        home.path(),
        root,
        /*history_base*/ None,
        /*next_ordinal*/ 6,
    );
    let root_end = history_position(root_path.as_path(), root, /*end_ordinal_exclusive*/ 4);
    let middle_path = write_rollout(home.path(), middle, Some(root_end), /*next_ordinal*/ 1);
    let middle_end = history_position(
        middle_path.as_path(),
        middle,
        /*end_ordinal_exclusive*/ 5,
    );
    let child_path = write_rollout(
        home.path(),
        child,
        Some(middle_end),
        /*next_ordinal*/ 3,
    );

    let lineage = store
        .resolve_rollout_lineage(child, /*initial_path*/ None)
        .await
        .expect("resolve nested lineage");

    assert_eq!(
        lineage.segments,
        vec![
            RolloutLineageSegment {
                rollout_id: root,
                rollout_path: root_path.clone(),
                start_ordinal: 1,
                end: Some(root_end),
            },
            RolloutLineageSegment {
                rollout_id: middle,
                rollout_path: middle_path.clone(),
                start_ordinal: 5,
                end: Some(middle_end),
            },
            RolloutLineageSegment {
                rollout_id: child,
                rollout_path: child_path,
                start_ordinal: 6,
                end: None,
            },
        ]
    );
}

#[tokio::test]
async fn resolves_archived_ancestors() {
    let home = TempDir::new().expect("temp dir");
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
    let root = ThreadId::default();
    let child = ThreadId::default();
    let root_path = write_rollout_under(
        home.path().join("archived_sessions"),
        root,
        /*history_base*/ None,
        /*next_ordinal*/ 3,
    );
    write_rollout(
        home.path(),
        child,
        Some(history_position(
            root_path.as_path(),
            root,
            /*end_ordinal_exclusive*/ 3,
        )),
        /*next_ordinal*/ 2,
    );

    let lineage = store
        .resolve_rollout_lineage(child, /*initial_path*/ None)
        .await
        .expect("resolve archived ancestor");

    assert_eq!(lineage.segments[0].rollout_path, root_path);
}

#[tokio::test]
async fn resolves_lineage_at_explicit_history_position() {
    let home = TempDir::new().expect("temp dir");
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
    let root = ThreadId::default();
    let child = ThreadId::default();
    let root_path = write_rollout(
        home.path(),
        root,
        /*history_base*/ None,
        /*next_ordinal*/ 6,
    );
    let root_end = history_position(root_path.as_path(), root, /*end_ordinal_exclusive*/ 4);
    let child_path = write_rollout(home.path(), child, Some(root_end), /*next_ordinal*/ 4);
    let end = history_position(
        child_path.as_path(),
        child,
        /*end_ordinal_exclusive*/ 6,
    );

    let lineage = store
        .resolve_rollout_lineage(child, /*initial_path*/ None)
        .await
        .expect("resolve child lineage")
        .truncate_at(end)
        .await
        .expect("resolve explicit position");

    assert_eq!(
        lineage.segments,
        vec![
            RolloutLineageSegment {
                rollout_id: root,
                rollout_path: root_path.clone(),
                start_ordinal: 1,
                end: Some(root_end),
            },
            RolloutLineageSegment {
                rollout_id: child,
                rollout_path: child_path.clone(),
                start_ordinal: 5,
                end: Some(end),
            },
        ]
    );
}

#[tokio::test]
async fn rejects_missing_cycles_and_out_of_bounds_offsets() {
    let home = TempDir::new().expect("temp dir");
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
    let missing_parent = ThreadId::default();
    let missing_child = ThreadId::default();
    write_rollout(
        home.path(),
        missing_child,
        Some(unchecked_history_position(
            missing_parent,
            /*end_ordinal_exclusive*/ 1,
        )),
        /*next_ordinal*/ 2,
    );
    assert_invalid_lineage(&store, missing_child, "missing source rollout").await;

    let cycle_a = ThreadId::default();
    let cycle_b = ThreadId::default();
    write_rollout(
        home.path(),
        cycle_a,
        Some(unchecked_history_position(
            cycle_b, /*end_ordinal_exclusive*/ 1,
        )),
        /*next_ordinal*/ 2,
    );
    write_rollout(
        home.path(),
        cycle_b,
        Some(unchecked_history_position(
            cycle_a, /*end_ordinal_exclusive*/ 1,
        )),
        /*next_ordinal*/ 2,
    );
    assert_invalid_lineage(&store, cycle_a, "cycle detected").await;

    let root = ThreadId::default();
    let invalid_child = ThreadId::default();
    let root_path = write_rollout(
        home.path(),
        root,
        /*history_base*/ None,
        /*next_ordinal*/ 2,
    );
    write_rollout(
        home.path(),
        invalid_child,
        Some(HistoryPosition {
            thread_id: root,
            end_ordinal_exclusive: 2,
            end_byte_offset: fs::metadata(root_path).expect("root metadata").len() + 1,
        }),
        /*next_ordinal*/ 2,
    );
    assert_invalid_lineage(
        &store,
        invalid_child,
        "cutoff byte offset is past the source rollout",
    )
    .await;
}

async fn assert_invalid_lineage(store: &LocalThreadStore, thread_id: ThreadId, detail: &str) {
    let err = store
        .resolve_rollout_lineage(thread_id, /*initial_path*/ None)
        .await
        .expect_err("lineage should be invalid");
    assert!(err.to_string().contains(detail), "{err}");
    let err = store
        .load_history(LoadThreadHistoryParams {
            thread_id,
            include_archived: false,
        })
        .await
        .expect_err("complete history should reject invalid lineage");
    assert!(err.to_string().contains(detail), "{err}");
}

fn append_rollout_lines(path: &Path, items: impl IntoIterator<Item = (u64, RolloutItem)>) {
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open rollout for append");
    for (ordinal, item) in items {
        writeln!(file, "{}", rollout_line(ordinal, item)).expect("append rollout line");
    }
}

fn turn_started(turn_id: &str) -> RolloutItem {
    RolloutItem::EventMsg(EventMsg::TurnStarted(TurnStartedEvent {
        turn_id: turn_id.to_string(),
        root_turn_id: None,
        trace_id: None,
        started_at: None,
        model_context_window: None,
        collaboration_mode_kind: Default::default(),
    }))
}

fn write_rollout(
    home: &Path,
    thread_id: ThreadId,
    history_base: Option<HistoryPosition>,
    next_ordinal: u64,
) -> std::path::PathBuf {
    write_rollout_under(
        home.join("sessions/2026/07/16"),
        thread_id,
        history_base,
        next_ordinal,
    )
}

fn write_rollout_under(
    directory: std::path::PathBuf,
    thread_id: ThreadId,
    history_base: Option<HistoryPosition>,
    next_ordinal: u64,
) -> std::path::PathBuf {
    fs::create_dir_all(directory.as_path()).expect("create rollout directory");
    let path = directory.join(format!("rollout-2026-07-16T00-00-00-{thread_id}.jsonl"));
    let initial_ordinal = history_base.map_or(0, |base| base.end_ordinal_exclusive);
    let mut lines = vec![rollout_line(
        initial_ordinal,
        RolloutItem::SessionMeta(SessionMetaLine {
            meta: SessionMeta {
                session_id: thread_id.into(),
                id: thread_id,
                history_mode: ThreadHistoryMode::Paginated,
                history_base,
                ..SessionMeta::default()
            },
            git: None,
        }),
    )];
    for offset in 1..next_ordinal {
        let ordinal = initial_ordinal
            .checked_add(offset)
            .expect("fixture ordinal");
        lines.push(rollout_line(
            ordinal,
            RolloutItem::EventMsg(codex_protocol::protocol::EventMsg::ShutdownComplete),
        ));
    }
    fs::write(path.as_path(), format!("{}\n", lines.join("\n"))).expect("write rollout");
    path
}

fn rollout_line(ordinal: u64, item: RolloutItem) -> String {
    serde_json::to_string(&RolloutLine {
        timestamp: "2026-07-16T00:00:00.000Z".to_string(),
        ordinal: Some(ordinal),
        item,
    })
    .expect("serialize rollout line")
}

fn history_position(
    path: &Path,
    thread_id: ThreadId,
    end_ordinal_exclusive: u64,
) -> HistoryPosition {
    HistoryPosition {
        thread_id,
        end_ordinal_exclusive,
        end_byte_offset: rollout_end_byte_offset(path, end_ordinal_exclusive),
    }
}

fn rollout_end_byte_offset(path: &Path, end_ordinal_exclusive: u64) -> u64 {
    let bytes = fs::read(path).expect("read rollout");
    let end_byte_offset = bytes
        .split_inclusive(|byte| *byte == b'\n')
        .take_while(|line| {
            codex_rollout::parse_rollout_line_bytes(line)
                .expect("parse rollout fixture")
                .ordinal
                .expect("paginated rollout ordinal")
                < end_ordinal_exclusive
        })
        .map(<[u8]>::len)
        .sum::<usize>();
    u64::try_from(end_byte_offset).expect("rollout byte offset fits u64")
}

fn unchecked_history_position(thread_id: ThreadId, end_ordinal_exclusive: u64) -> HistoryPosition {
    HistoryPosition {
        thread_id,
        end_ordinal_exclusive,
        end_byte_offset: 0,
    }
}
