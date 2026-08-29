use std::fs;
use std::path::Path;
use std::path::PathBuf;

use codex_protocol::ThreadId;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Settings;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::HistoryPosition;
use codex_protocol::protocol::SessionMeta;
use codex_protocol::protocol::SessionMetaLine;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_protocol::protocol::ThreadSettingsAppliedEvent;
use codex_protocol::protocol::ThreadSettingsSnapshot;
use codex_rollout::RolloutItem;
use codex_rollout::RolloutLine;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::*;
use crate::LoadThreadHistoryParams;
use crate::ThreadStore;
use crate::local::test_support::test_config;

#[tokio::test]
async fn loads_latest_snapshot_from_leaf_segment() {
    let home = TempDir::new().expect("temp dir");
    let root_id = ThreadId::new();
    let root_path = write_paginated_rollout(
        home.path(),
        root_id,
        None,
        vec![thread_settings_applied_item(settings_snapshot(
            home.path(),
            "root-model",
            Some(false),
        ))],
    );
    let child_id = ThreadId::new();
    write_paginated_rollout(
        home.path(),
        child_id,
        Some(history_position(
            root_path.as_path(),
            root_id,
            /*end_ordinal_exclusive*/ 2,
        )),
        vec![thread_settings_applied_item(settings_snapshot(
            home.path(),
            "child-model",
            Some(true),
        ))],
    );
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);

    let snapshot = store
        .load_latest_thread_settings_snapshot(LoadThreadHistoryParams {
            thread_id: child_id,
            include_archived: false,
        })
        .await
        .expect("load latest settings snapshot")
        .expect("child snapshot");

    assert_eq!(snapshot.model, "child-model");
    assert_eq!(snapshot.shell_tool_enabled, Some(true));
}

#[tokio::test]
async fn loads_snapshot_from_compressed_ancestor_without_crossing_cutoff() {
    let home = TempDir::new().expect("temp dir");
    let root_id = ThreadId::new();
    let root_path = write_paginated_rollout(
        home.path(),
        root_id,
        None,
        vec![
            thread_settings_applied_item(settings_snapshot(
                home.path(),
                "before-cutoff",
                Some(false),
            )),
            thread_settings_applied_item(settings_snapshot(
                home.path(),
                "after-cutoff",
                Some(true),
            )),
        ],
    );
    let original = fs::read_to_string(root_path.as_path()).expect("read root rollout");
    let mut original_lines = original.lines();
    let session_meta = original_lines.next().expect("root session metadata");
    let before_cutoff = original_lines.next().expect("before-cutoff snapshot");
    let after_cutoff = original_lines.next().expect("after-cutoff snapshot");
    let rejected_before_cutoff = serde_json::json!({
        "timestamp": "2026-08-26T00:00:00Z",
        "type": "future_item",
        "payload": {},
    });
    let rejected_at_cutoff = serde_json::json!({
        "timestamp": "2026-08-26T00:00:00Z",
        "ordinal": 2,
        "type": "future_item",
        "payload": {},
    });
    fs::write(
        root_path.as_path(),
        format!(
            "{session_meta}\n{{\n{rejected_before_cutoff}\n{before_cutoff}\n{rejected_at_cutoff}\n{after_cutoff}\n"
        ),
    )
    .expect("insert rejected rollout records");
    let inherited_prefix = history_position(
        root_path.as_path(),
        root_id,
        /*end_ordinal_exclusive*/ 2,
    );
    compress_rollout(root_path.as_path());

    let child_id = ThreadId::new();
    write_paginated_rollout(home.path(), child_id, Some(inherited_prefix), Vec::new());
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);

    let snapshot = store
        .load_latest_thread_settings_snapshot(LoadThreadHistoryParams {
            thread_id: child_id,
            include_archived: false,
        })
        .await
        .expect("load latest settings snapshot")
        .expect("inherited snapshot");

    assert_eq!(snapshot.model, "before-cutoff");
    assert_eq!(snapshot.shell_tool_enabled, Some(false));
}

#[tokio::test]
async fn returns_none_after_complete_lineage_has_no_snapshot() {
    let home = TempDir::new().expect("temp dir");
    let root_id = ThreadId::new();
    let root_path = write_paginated_rollout(home.path(), root_id, None, Vec::new());
    let child_id = ThreadId::new();
    write_paginated_rollout(
        home.path(),
        child_id,
        Some(history_position(
            root_path.as_path(),
            root_id,
            /*end_ordinal_exclusive*/ 1,
        )),
        Vec::new(),
    );
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);

    let snapshot = store
        .load_latest_thread_settings_snapshot(LoadThreadHistoryParams {
            thread_id: child_id,
            include_archived: false,
        })
        .await
        .expect("scan complete lineage");

    assert_eq!(snapshot, None);
}

#[tokio::test]
async fn returns_found_legacy_snapshot_when_shell_state_is_missing() {
    let home = TempDir::new().expect("temp dir");
    let thread_id = ThreadId::new();
    write_paginated_rollout(
        home.path(),
        thread_id,
        None,
        vec![thread_settings_applied_item(settings_snapshot(
            home.path(),
            "legacy-model",
            None,
        ))],
    );
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);

    let snapshot = store
        .load_latest_thread_settings_snapshot(LoadThreadHistoryParams {
            thread_id,
            include_archived: false,
        })
        .await
        .expect("load legacy snapshot")
        .expect("found legacy snapshot");

    assert_eq!(snapshot.model, "legacy-model");
    assert_eq!(snapshot.shell_tool_enabled, None);
}

fn write_paginated_rollout(
    home: &Path,
    thread_id: ThreadId,
    history_base: Option<HistoryPosition>,
    items: Vec<RolloutItem>,
) -> PathBuf {
    let directory = home.join("sessions/2026/08/26");
    fs::create_dir_all(directory.as_path()).expect("create rollout directory");
    let path = directory.join(format!("rollout-2026-08-26T00-00-00-{thread_id}.jsonl"));
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
    lines.extend(items.into_iter().enumerate().map(|(index, item)| {
        rollout_line(
            initial_ordinal
                .checked_add(u64::try_from(index).expect("fixture ordinal"))
                .and_then(|ordinal| ordinal.checked_add(1))
                .expect("fixture ordinal"),
            item,
        )
    }));
    fs::write(path.as_path(), format!("{}\n", lines.join("\n"))).expect("write rollout");
    path
}

fn history_position(
    path: &Path,
    thread_id: ThreadId,
    end_ordinal_exclusive: u64,
) -> HistoryPosition {
    let expected_ordinal = end_ordinal_exclusive
        .checked_sub(1)
        .expect("fixture cutoff must include a source record");
    let contents = fs::read_to_string(path).expect("read rollout");
    let mut end_byte_offset = 0_u64;
    for raw_line in contents.split_inclusive('\n') {
        let line_len = u64::try_from(raw_line.len()).expect("fixture line length");
        let raw_line = raw_line.trim();
        if raw_line.is_empty() {
            end_byte_offset += line_len;
            continue;
        }
        end_byte_offset += line_len;
        let Ok(value) = serde_json::from_str(raw_line) else {
            continue;
        };
        let Ok(line) = codex_rollout::decode_rollout_line(value) else {
            continue;
        };
        if line.ordinal == Some(expected_ordinal) {
            return HistoryPosition {
                thread_id,
                end_ordinal_exclusive,
                end_byte_offset,
            };
        }
    }
    panic!("missing fixture rollout ordinal {expected_ordinal}");
}

fn rollout_line(ordinal: u64, item: RolloutItem) -> String {
    serde_json::to_string(&RolloutLine {
        timestamp: "2026-08-26T00:00:00Z".to_string(),
        ordinal: Some(ordinal),
        item,
    })
    .expect("serialize rollout line")
}

fn thread_settings_applied_item(snapshot: ThreadSettingsSnapshot) -> RolloutItem {
    RolloutItem::EventMsg(EventMsg::ThreadSettingsApplied(
        ThreadSettingsAppliedEvent {
            thread_settings: snapshot,
        },
    ))
}

fn settings_snapshot(
    cwd: &Path,
    model: &str,
    shell_tool_enabled: Option<bool>,
) -> ThreadSettingsSnapshot {
    ThreadSettingsSnapshot {
        model: model.to_string(),
        model_provider_id: "test-provider".to_string(),
        service_tier: None,
        approval_policy: AskForApproval::Never,
        approvals_reviewer: ApprovalsReviewer::User,
        permission_profile: PermissionProfile::read_only(),
        active_permission_profile: None,
        cwd: serde_json::from_value(serde_json::json!(cwd)).expect("absolute cwd"),
        reasoning_effort: None,
        reasoning_summary: None,
        personality: None,
        collaboration_mode: CollaborationMode {
            mode: ModeKind::Default,
            settings: Settings {
                model: model.to_string(),
                reasoning_effort: None,
                developer_instructions: None,
            },
        },
        shell_tool_enabled,
    }
}

fn compress_rollout(path: &Path) {
    let contents = fs::read(path).expect("read rollout");
    let compressed =
        zstd::stream::encode_all(contents.as_slice(), /*level*/ 3).expect("compress rollout");
    fs::write(path.with_extension("jsonl.zst"), compressed).expect("write compressed rollout");
    fs::remove_file(path).expect("remove plain rollout");
}
