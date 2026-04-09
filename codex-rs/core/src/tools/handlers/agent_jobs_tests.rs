// Merge-safety anchor: agent job arg-parsing tests must keep max_concurrency as the only accepted concurrency field so stale aliases fail loudly.

use super::*;
use crate::tools::handlers::multi_agents_common::thread_spawn_source;
use codex_protocol::ThreadId;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn spawn_agents_on_csv_args_reject_max_workers_alias() {
    let err = serde_json::from_value::<SpawnAgentsOnCsvArgs>(json!({
        "csv_path": "input.csv",
        "instruction": "Return {path}",
        "max_workers": 1,
    }))
    .expect_err("max_workers alias should fail loudly");

    assert!(err.to_string().contains("unknown field `max_workers`"));
}

#[test]
fn agent_job_worker_source_keeps_thread_spawn_depth() {
    let parent_thread_id = ThreadId::new();
    let session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: ThreadId::new(),
        depth: 2,
        agent_path: None,
        agent_nickname: None,
        agent_role: None,
    });
    let child_depth = next_thread_spawn_depth(&session_source);

    let worker_source = thread_spawn_source(
        parent_thread_id,
        &session_source,
        child_depth,
        /*agent_role*/ None,
        /*task_name*/ None,
    )
    .expect("agent-job workers should derive thread-spawn metadata");

    assert_eq!(
        worker_source,
        SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id,
            depth: child_depth,
            agent_path: None,
            agent_nickname: None,
            agent_role: None,
        })
    );
}

#[test]
fn parse_csv_supports_quotes_and_commas() {
    let input = "id,name\n1,\"alpha, beta\"\n2,gamma\n";
    let (headers, rows) = parse_csv(input).expect("csv parse");
    assert_eq!(headers, vec!["id".to_string(), "name".to_string()]);
    assert_eq!(
        rows,
        vec![
            vec!["1".to_string(), "alpha, beta".to_string()],
            vec!["2".to_string(), "gamma".to_string()]
        ]
    );
}

#[test]
fn csv_escape_quotes_when_needed() {
    assert_eq!(csv_escape("simple"), "simple");
    assert_eq!(csv_escape("a,b"), "\"a,b\"");
    assert_eq!(csv_escape("a\"b"), "\"a\"\"b\"");
}

#[test]
fn render_instruction_template_expands_placeholders_and_escapes_braces() {
    let row = json!({
        "path": "src/lib.rs",
        "area": "test",
        "file path": "docs/readme.md",
    });
    let rendered = render_instruction_template(
        "Review {path} in {area}. Also see {file path}. Use {{literal}}.",
        &row,
    );
    assert_eq!(
        rendered,
        "Review src/lib.rs in test. Also see docs/readme.md. Use {literal}."
    );
}

#[test]
fn render_instruction_template_leaves_unknown_placeholders() {
    let row = json!({
        "path": "src/lib.rs",
    });
    let rendered = render_instruction_template("Check {path} then {missing}", &row);
    assert_eq!(rendered, "Check src/lib.rs then {missing}");
}

#[test]
fn ensure_unique_headers_rejects_duplicates() {
    let headers = vec!["path".to_string(), "path".to_string()];
    let Err(err) = ensure_unique_headers(headers.as_slice()) else {
        panic!("expected duplicate header error");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel("csv header path is duplicated".to_string())
    );
}
