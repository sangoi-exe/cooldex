use super::*;
use pretty_assertions::assert_eq;
use serde::Serialize;

#[test]
fn id_token_info_parses_email_and_plan() {
    #[derive(Serialize)]
    struct Header {
        alg: &'static str,
        typ: &'static str,
    }
    let header = Header {
        alg: "none",
        typ: "JWT",
    };
    let payload = serde_json::json!({
        "email": "user@example.com",
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": "pro"
        }
    });

    fn b64url_no_pad(bytes: &[u8]) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    let header_b64 = b64url_no_pad(&serde_json::to_vec(&header).unwrap());
    let payload_b64 = b64url_no_pad(&serde_json::to_vec(&payload).unwrap());
    let signature_b64 = b64url_no_pad(b"sig");
    let fake_jwt = format!("{header_b64}.{payload_b64}.{signature_b64}");

    let info = parse_chatgpt_jwt_claims(&fake_jwt).expect("should parse");
    assert_eq!(info.email.as_deref(), Some("user@example.com"));
    assert_eq!(info.get_chatgpt_plan_type().as_deref(), Some("Pro"));
}

#[test]
fn id_token_info_parses_go_plan() {
    #[derive(Serialize)]
    struct Header {
        alg: &'static str,
        typ: &'static str,
    }
    let header = Header {
        alg: "none",
        typ: "JWT",
    };
    let payload = serde_json::json!({
        "email": "user@example.com",
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": "go"
        }
    });

    fn b64url_no_pad(bytes: &[u8]) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    let header_b64 = b64url_no_pad(&serde_json::to_vec(&header).unwrap());
    let payload_b64 = b64url_no_pad(&serde_json::to_vec(&payload).unwrap());
    let signature_b64 = b64url_no_pad(b"sig");
    let fake_jwt = format!("{header_b64}.{payload_b64}.{signature_b64}");

    let info = parse_chatgpt_jwt_claims(&fake_jwt).expect("should parse");
    assert_eq!(info.email.as_deref(), Some("user@example.com"));
    assert_eq!(info.get_chatgpt_plan_type().as_deref(), Some("Go"));
}

#[test]
fn id_token_info_handles_missing_fields() {
    #[derive(Serialize)]
    struct Header {
        alg: &'static str,
        typ: &'static str,
    }
    let header = Header {
        alg: "none",
        typ: "JWT",
    };
    let payload = serde_json::json!({ "sub": "123" });

    fn b64url_no_pad(bytes: &[u8]) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    let header_b64 = b64url_no_pad(&serde_json::to_vec(&header).unwrap());
    let payload_b64 = b64url_no_pad(&serde_json::to_vec(&payload).unwrap());
    let signature_b64 = b64url_no_pad(b"sig");
    let fake_jwt = format!("{header_b64}.{payload_b64}.{signature_b64}");

    let info = parse_chatgpt_jwt_claims(&fake_jwt).expect("should parse");
    assert!(info.email.is_none());
    assert!(info.get_chatgpt_plan_type().is_none());
}

#[test]
fn workspace_account_detection_matches_workspace_plans() {
    let workspace = IdTokenInfo {
        chatgpt_plan_type: Some(PlanType::Known(KnownPlan::Business)),
        ..IdTokenInfo::default()
    };
    assert_eq!(workspace.is_workspace_account(), true);

    let personal = IdTokenInfo {
        chatgpt_plan_type: Some(PlanType::Known(KnownPlan::Pro)),
        ..IdTokenInfo::default()
    };
    assert_eq!(personal.is_workspace_account(), false);
}

#[test]
fn supported_chatgpt_auth_plan_matches_current_supported_plans() {
    let supported_plans = [
        KnownPlan::Plus,
        KnownPlan::Pro,
        KnownPlan::Team,
        KnownPlan::Business,
        KnownPlan::Enterprise,
        KnownPlan::Edu,
    ];

    for plan in supported_plans {
        let info = IdTokenInfo {
            chatgpt_plan_type: Some(PlanType::Known(plan)),
            ..IdTokenInfo::default()
        };
        assert_eq!(info.is_supported_chatgpt_auth_plan(), true);
    }

    let unsupported_plans = [KnownPlan::Free, KnownPlan::Go];
    for plan in unsupported_plans {
        let info = IdTokenInfo {
            chatgpt_plan_type: Some(PlanType::Known(plan)),
            ..IdTokenInfo::default()
        };
        assert_eq!(info.is_supported_chatgpt_auth_plan(), false);
    }

    let unknown = IdTokenInfo {
        chatgpt_plan_type: Some(PlanType::Unknown("mystery-tier".to_string())),
        ..IdTokenInfo::default()
    };
    assert_eq!(unknown.is_supported_chatgpt_auth_plan(), false);

    assert_eq!(
        IdTokenInfo::default().is_supported_chatgpt_auth_plan(),
        false
    );
}

#[test]
fn preferred_store_account_id_uses_user_and_workspace() {
    let info = IdTokenInfo {
        chatgpt_user_id: Some("user-123".to_string()),
        chatgpt_account_id: Some("org-456".to_string()),
        ..IdTokenInfo::default()
    };

    assert_eq!(
        info.preferred_store_account_id().as_deref(),
        Some("chatgpt-user:user-123:workspace:org-456")
    );
}

#[test]
fn migrated_store_account_id_only_rekeys_legacy_workspace_backed_ids() {
    let token_data = TokenData {
        id_token: IdTokenInfo {
            chatgpt_user_id: Some("user-123".to_string()),
            chatgpt_account_id: Some("org-456".to_string()),
            ..IdTokenInfo::default()
        },
        access_token: "access".to_string(),
        refresh_token: "refresh".to_string(),
        account_id: Some("org-456".to_string()),
    };

    assert_eq!(
        token_data.migrated_store_account_id("org-456").as_deref(),
        Some("chatgpt-user:user-123:workspace:org-456")
    );
    assert_eq!(token_data.migrated_store_account_id("custom-id"), None);
}
