#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use pretty_assertions::assert_eq;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tempfile::TempDir;
use tonic::Code;

use crate::test_support::DashboardReply;
use crate::test_support::FakeCursorServices;

const ACCESS_TOKEN: &str = "fake-access-token";
const REFRESH_TOKEN: &str = "fake-refresh-token";

#[test]
fn resolves_xdg_config_home_before_home() {
    assert_eq!(
        resolve_store_path(Some("/xdg".into()), Some("/home/operator".into())).unwrap(),
        PathBuf::from("/xdg/cursor/auth.json")
    );
    assert_eq!(
        resolve_store_path(None, Some("/home/operator".into())).unwrap(),
        PathBuf::from("/home/operator/.config/cursor/auth.json")
    );
    assert!(matches!(
        resolve_store_path(None, None),
        Err(CursorCredentialStoreError::MissingConfigHome)
    ));
}

#[test]
fn loads_the_exact_secure_cursor_store_schema() {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("auth.json");
    write_store(&path, valid_store_json(), 0o600);

    let credentials = CursorCredentialStore::new(path).load().unwrap();

    assert_eq!(credentials.access_token(), ACCESS_TOKEN);
    assert_eq!(credentials.refresh_token(), REFRESH_TOKEN);
}

#[test]
fn redacts_both_tokens_from_debug_output() {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("auth.json");
    write_store(&path, valid_store_json(), 0o600);

    let debug = format!("{:?}", CursorCredentialStore::new(path).load().unwrap());

    assert_eq!(
        debug,
        "CursorCredentials { access_token: <redacted>, refresh_token: <redacted> }"
    );
    assert!(!debug.contains(ACCESS_TOKEN));
    assert!(!debug.contains(REFRESH_TOKEN));
}

#[test]
fn rejects_a_symlink_store() {
    let temp_dir = TempDir::new().unwrap();
    let target = temp_dir.path().join("target.json");
    let link = temp_dir.path().join("auth.json");
    write_store(&target, valid_store_json(), 0o600);
    std::os::unix::fs::symlink(&target, &link).unwrap();

    assert!(matches!(
        CursorCredentialStore::new(link).load(),
        Err(CursorCredentialStoreError::Symlink { .. })
    ));
}

#[test]
fn rejects_a_non_regular_store() {
    let temp_dir = TempDir::new().unwrap();

    assert!(matches!(
        CursorCredentialStore::new(temp_dir.path().to_path_buf()).load(),
        Err(CursorCredentialStoreError::NotRegular { .. })
    ));
}

#[test]
fn rejects_group_or_world_permission_bits() {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("auth.json");
    write_store(&path, valid_store_json(), 0o640);

    assert!(matches!(
        CursorCredentialStore::new(path).load(),
        Err(CursorCredentialStoreError::InsecurePermissions { mode: 0o640, .. })
    ));
}

#[test]
fn rejects_a_store_owned_by_another_uid() {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("auth.json");
    write_store(&path, valid_store_json(), 0o600);
    let metadata = File::open(&path).unwrap().metadata().unwrap();
    let actual_uid = std::os::unix::fs::MetadataExt::uid(&metadata);

    assert!(matches!(
        validate_metadata(&path, &metadata, actual_uid ^ 1),
        Err(CursorCredentialStoreError::WrongOwner { .. })
    ));
}

#[test]
fn rejects_missing_and_unknown_store_fields() {
    let cases = [
        r#"{"accessToken":"fake-access-token"}"#,
        r#"{"accessToken":"fake-access-token","refreshToken":"fake-refresh-token","tokenType":"Bearer"}"#,
    ];

    for contents in cases {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("auth.json");
        write_store(&path, contents, 0o600);

        assert!(matches!(
            CursorCredentialStore::new(path).load(),
            Err(CursorCredentialStoreError::InvalidJson { .. })
        ));
    }
}

#[test]
fn rejects_empty_tokens() {
    let cases = [
        r#"{"accessToken":"","refreshToken":"fake-refresh-token"}"#,
        r#"{"accessToken":"fake-access-token","refreshToken":"  "}"#,
    ];

    for contents in cases {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("auth.json");
        write_store(&path, contents, 0o600);

        assert!(matches!(
            CursorCredentialStore::new(path).load(),
            Err(CursorCredentialStoreError::EmptyToken { .. })
        ));
    }
}

#[test]
fn rejects_an_oversized_store() {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("auth.json");
    write_store(&path, &"x".repeat(MAX_CREDENTIAL_STORE_BYTES + 1), 0o600);

    assert!(matches!(
        CursorCredentialStore::new(path).load(),
        Err(CursorCredentialStoreError::TooLarge { .. })
    ));
}

#[tokio::test]
async fn verifies_the_pinned_identity_with_control_request_metadata() {
    let credentials: CursorCredentials = serde_json::from_str(valid_store_json()).unwrap();
    let mut services = FakeCursorServices::spawn(
        vec![DashboardReply::Identity {
            user_id: 390_777_501,
            team_id: Some(12_565_657),
        }],
        Vec::new(),
    )
    .await;

    verify_cursor_identity_at(
        services.endpoint(),
        &credentials,
        390_777_501,
        12_565_657,
    )
    .await
    .unwrap();

    let metadata = services.next_dashboard_request().await;
    assert_eq!(
        metadata.get("authorization").unwrap().to_str().unwrap(),
        "Bearer fake-access-token"
    );
    assert_eq!(
        metadata.get("x-cursor-client-version").unwrap(),
        "cli-2026.07.23-e383d2b"
    );
    assert_eq!(metadata.get("x-cursor-client-type").unwrap(), "cli");
    assert_eq!(metadata.get("x-ghost-mode").unwrap(), "true");
    assert!(metadata.get("x-request-id").is_some());
    assert!(metadata.get("x-cursor-streaming").is_none());
    services.shutdown().await;
}

#[tokio::test]
async fn rejects_identity_and_authenticated_control_failures() {
    let cases = [
        (
            DashboardReply::Identity {
                user_id: 390_777_502,
                team_id: Some(12_565_657),
            },
            CursorIdentityError::UserMismatch {
                expected: 390_777_501,
                actual: 390_777_502,
            },
        ),
        (
            DashboardReply::Identity {
                user_id: 390_777_501,
                team_id: Some(12_565_658),
            },
            CursorIdentityError::TeamMismatch {
                expected: 12_565_657,
                actual: 12_565_658,
            },
        ),
        (
            DashboardReply::Identity {
                user_id: 390_777_501,
                team_id: None,
            },
            CursorIdentityError::MissingTeamId,
        ),
        (
            DashboardReply::Identity {
                user_id: -1,
                team_id: Some(12_565_657),
            },
            CursorIdentityError::InvalidUserId(-1),
        ),
        (
            DashboardReply::Identity {
                user_id: 390_777_501,
                team_id: Some(-1),
            },
            CursorIdentityError::InvalidTeamId(-1),
        ),
        (
            DashboardReply::Error(Code::Unauthenticated),
            CursorIdentityError::Rpc(Code::Unauthenticated),
        ),
    ];

    for (reply, expected_error) in cases {
        let credentials: CursorCredentials = serde_json::from_str(valid_store_json()).unwrap();
        let mut services = FakeCursorServices::spawn(vec![reply], Vec::new()).await;
        let error = verify_cursor_identity_at(
            services.endpoint(),
            &credentials,
            390_777_501,
            12_565_657,
        )
        .await
        .unwrap_err();
        assert_eq!(error, expected_error);
        let _ = services.next_dashboard_request().await;
        services.shutdown().await;
    }
}

fn valid_store_json() -> &'static str {
    r#"{"accessToken":"fake-access-token","refreshToken":"fake-refresh-token"}"#
}

fn write_store(path: &Path, contents: &str, mode: u32) {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(mode)
        .open(path)
        .unwrap();
    file.write_all(contents.as_bytes()).unwrap();
    file.set_permissions(std::fs::Permissions::from_mode(mode))
        .unwrap();
}
