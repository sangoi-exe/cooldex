use base64::Engine;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Default)]
pub struct TokenData {
    /// Flat info parsed from the JWT in auth.json.
    #[serde(
        deserialize_with = "deserialize_id_token",
        serialize_with = "serialize_id_token"
    )]
    pub id_token: IdTokenInfo,

    /// This is a JWT.
    pub access_token: String,

    pub refresh_token: String,

    pub account_id: Option<String>,
}

impl TokenData {
    pub fn preferred_store_account_id(&self) -> Option<String> {
        self.id_token
            .preferred_store_account_id()
            .or_else(|| self.account_id.clone())
    }

    pub fn migrated_store_account_id(&self, current_store_account_id: &str) -> Option<String> {
        let preferred_store_account_id = self.id_token.preferred_store_account_id()?;
        if self.account_id.as_deref() == Some(current_store_account_id)
            && preferred_store_account_id != current_store_account_id
        {
            return Some(preferred_store_account_id);
        }
        None
    }
}

/// Flat subset of useful claims in id_token from auth.json.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct IdTokenInfo {
    pub email: Option<String>,
    /// The ChatGPT subscription plan type
    /// (e.g., "free", "plus", "pro", "business", "enterprise", "edu").
    /// (Note: values may vary by backend.)
    pub(crate) chatgpt_plan_type: Option<PlanType>,
    /// ChatGPT user identifier associated with the token, if present.
    pub chatgpt_user_id: Option<String>,
    /// Organization/workspace identifier associated with the token, if present.
    pub chatgpt_account_id: Option<String>,
    pub raw_jwt: String,
}

impl IdTokenInfo {
    pub fn get_chatgpt_plan_type(&self) -> Option<String> {
        self.chatgpt_plan_type.as_ref().map(|t| match t {
            PlanType::Known(plan) => format!("{plan:?}"),
            PlanType::Unknown(s) => s.clone(),
        })
    }

    // Merge-safety anchor: ChatGPT auth admission must stay aligned across saved-account
    // filtering, browser/device login persistence, and external-token login.
    pub fn is_supported_chatgpt_auth_plan(&self) -> bool {
        matches!(
            self.chatgpt_plan_type,
            Some(PlanType::Known(
                KnownPlan::Plus
                    | KnownPlan::Pro
                    | KnownPlan::Team
                    | KnownPlan::Business
                    | KnownPlan::Enterprise
                    | KnownPlan::Edu
            ))
        )
    }

    pub fn is_workspace_account(&self) -> bool {
        matches!(
            self.chatgpt_plan_type,
            Some(PlanType::Known(
                KnownPlan::Team | KnownPlan::Business | KnownPlan::Enterprise | KnownPlan::Edu
            ))
        )
    }

    pub fn preferred_store_account_id(&self) -> Option<String> {
        let user_id = self.chatgpt_user_id.as_deref()?;
        Some(match self.chatgpt_account_id.as_deref() {
            Some(chatgpt_account_id) => {
                format!("chatgpt-user:{user_id}:workspace:{chatgpt_account_id}")
            }
            None => format!("chatgpt-user:{user_id}"),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum PlanType {
    Known(KnownPlan),
    Unknown(String),
}

impl PlanType {
    pub(crate) fn from_raw_value(raw: &str) -> Self {
        match raw.to_ascii_lowercase().as_str() {
            "free" => Self::Known(KnownPlan::Free),
            "go" => Self::Known(KnownPlan::Go),
            "plus" => Self::Known(KnownPlan::Plus),
            "pro" => Self::Known(KnownPlan::Pro),
            "team" => Self::Known(KnownPlan::Team),
            "business" => Self::Known(KnownPlan::Business),
            "enterprise" => Self::Known(KnownPlan::Enterprise),
            "education" | "edu" => Self::Known(KnownPlan::Edu),
            _ => Self::Unknown(raw.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum KnownPlan {
    Free,
    Go,
    Plus,
    Pro,
    Team,
    Business,
    Enterprise,
    Edu,
}

#[derive(Deserialize)]
struct IdClaims {
    #[serde(default)]
    email: Option<String>,
    #[serde(rename = "https://api.openai.com/profile", default)]
    profile: Option<ProfileClaims>,
    #[serde(rename = "https://api.openai.com/auth", default)]
    auth: Option<AuthClaims>,
}

#[derive(Deserialize)]
struct ProfileClaims {
    #[serde(default)]
    email: Option<String>,
}

#[derive(Deserialize)]
struct AuthClaims {
    #[serde(default)]
    chatgpt_plan_type: Option<PlanType>,
    #[serde(default)]
    chatgpt_user_id: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    chatgpt_account_id: Option<String>,
}

#[derive(Debug, Error)]
pub enum IdTokenInfoError {
    #[error("invalid ID token format")]
    InvalidFormat,
    #[error(transparent)]
    Base64(#[from] base64::DecodeError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub fn parse_chatgpt_jwt_claims(jwt: &str) -> Result<IdTokenInfo, IdTokenInfoError> {
    // JWT format: header.payload.signature
    let mut parts = jwt.split('.');
    let (_header_b64, payload_b64, _sig_b64) = match (parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(p), Some(s)) if !h.is_empty() && !p.is_empty() && !s.is_empty() => (h, p, s),
        _ => return Err(IdTokenInfoError::InvalidFormat),
    };

    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload_b64)?;
    let claims: IdClaims = serde_json::from_slice(&payload_bytes)?;
    let email = claims
        .email
        .or_else(|| claims.profile.and_then(|profile| profile.email));

    match claims.auth {
        Some(auth) => Ok(IdTokenInfo {
            email,
            raw_jwt: jwt.to_string(),
            chatgpt_plan_type: auth.chatgpt_plan_type,
            chatgpt_user_id: auth.chatgpt_user_id.or(auth.user_id),
            chatgpt_account_id: auth.chatgpt_account_id,
        }),
        None => Ok(IdTokenInfo {
            email,
            raw_jwt: jwt.to_string(),
            chatgpt_plan_type: None,
            chatgpt_user_id: None,
            chatgpt_account_id: None,
        }),
    }
}

fn deserialize_id_token<'de, D>(deserializer: D) -> Result<IdTokenInfo, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    parse_chatgpt_jwt_claims(&s).map_err(serde::de::Error::custom)
}

fn serialize_id_token<S>(id_token: &IdTokenInfo, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&id_token.raw_jwt)
}

#[cfg(test)]
mod tests {
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
}
