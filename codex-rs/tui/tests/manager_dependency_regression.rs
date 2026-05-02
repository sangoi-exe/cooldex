use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

fn rust_sources_under(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let entries =
        fs::read_dir(dir).unwrap_or_else(|err| panic!("failed to read {}: {err}", dir.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|err| panic!("failed to read dir entry: {err}"));
        let path = entry.path();
        if path.is_dir() {
            files.extend(rust_sources_under(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
    files.sort();
    files
}

#[test]
fn tui_manager_escape_hatches_stay_in_known_owner_seams() {
    let src_file = codex_utils_cargo_bin::find_resource!("src/chatwidget.rs")
        .unwrap_or_else(|err| panic!("failed to resolve src runfile: {err}"));
    let src_dir = src_file
        .parent()
        .unwrap_or_else(|| panic!("source file has no parent: {}", src_file.display()));
    let sources = rust_sources_under(src_dir);
    let forbidden = [
        "AuthManager",
        "ThreadManager",
        "auth_manager(",
        "thread_manager(",
    ];
    // Merge-safety anchor: the workspace-local TUI still owns explicit
    // AuthManager/ThreadManager seams for /accounts, embedded app-server
    // account projection, and legacy agent runtime startup. Keep this guard as
    // a known-seam allowlist so future manager escape hatches still fail loud.
    let allowed_manager_uses: BTreeSet<(&str, &str)> = [
        ("account_projection.rs", "AuthManager"),
        ("app.rs", "AuthManager"),
        ("app.rs", "auth_manager("),
        ("app/event_dispatch.rs", "AuthManager"),
        ("app/event_dispatch.rs", "auth_manager("),
        ("app/thread_routing.rs", "auth_manager("),
        ("bottom_pane/chatgpt_add_account_view.rs", "AuthManager"),
        ("chatwidget.rs", "AuthManager"),
        ("chatwidget/agent.rs", "ThreadManager"),
        ("lib.rs", "AuthManager"),
        ("lib.rs", "auth_manager("),
        ("onboarding/auth.rs", "AuthManager"),
    ]
    .into_iter()
    .collect();

    let mut violations = Vec::new();
    for path in sources {
        let contents = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        let relative_path = path
            .strip_prefix(src_dir)
            .unwrap_or_else(|err| {
                panic!(
                    "failed to strip source dir {} from {}: {err}",
                    src_dir.display(),
                    path.display()
                )
            })
            .display()
            .to_string()
            .replace('\\', "/");
        if relative_path.contains("/tests/") {
            continue;
        }
        for needle in forbidden {
            if contents.contains(needle)
                && !allowed_manager_uses.contains(&(relative_path.as_str(), needle))
            {
                violations.push(format!("{relative_path} contains `{needle}`"));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "unexpected manager dependency regression(s):\n{}",
        violations.join("\n")
    );
}
