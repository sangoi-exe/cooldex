use super::*;

#[test]
fn running_daemon_replacement_prompt() {
    let request = InstallRequest {
        source: "/cli/package".into(),
        version: "0.0.0".into(),
        destination: "/home/packages/app-server-daemon".into(),
        installed_version: Some("0.152.0".into()),
        restart_required: true,
    };
    insta::assert_snapshot!(describe_install_for_local_package_lane(&request, None), @r"
    Replace installed daemon version 0.152.0 with CLI version 0.0.0 from /cli/package.
    The daemon package will be installed in /home/packages/app-server-daemon.
    The selected package will be pinned. Run `codex app-server daemon update` to return to production updates.
    The running daemon will restart; active or queued work may be interrupted.
    ");
}

#[test]
fn local_lane_replacement_prompt_requires_a_newly_promoted_complete_package() {
    let request = InstallRequest {
        source: "/cli/package".into(),
        version: "0.0.0".into(),
        destination: "/home/packages/local-codex".into(),
        installed_version: Some("0.152.0".into()),
        restart_required: false,
    };
    insta::assert_snapshot!(
        describe_install_for_local_package_lane(
            &request,
            Some(codex_install_context::LocalPackageLane::Codex)
        ),
        @r"
    Replace installed daemon version 0.152.0 with CLI version 0.0.0 from /cli/package.
    The daemon package will be installed in /home/packages/local-codex.
    The selected package will remain promotion-owned and pinned. Promote a newly complete package to update this daemon again.
    "
    );
}
