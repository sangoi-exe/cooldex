use super::*;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_absolute_path::AbsolutePathBufGuard;
use pretty_assertions::assert_eq;
use std::num::NonZeroU64;
use tempfile::tempdir;

#[test]
fn test_deserialize_ollama_model_provider_toml() {
    let azure_provider_toml = r#"
name = "Ollama"
base_url = "http://localhost:11434/v1"
        "#;
    let expected_provider = ModelProviderInfo {
        name: "Ollama".into(),
        base_url: Some("http://localhost:11434/v1".into()),
        env_key: None,
        env_key_instructions: None,
        experimental_bearer_token: None,
        auth: None,
        aws: None,
        cursor_agent_service: None,
        wire_api: WireApi::Responses,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        request_max_retries: None,
        stream_max_retries: None,
        stream_idle_timeout_ms: None,
        websocket_connect_timeout_ms: None,
        requires_openai_auth: false,
        supports_websockets: false,
        supports_standalone_web_search: false,
    };

    let provider: ModelProviderInfo = toml::from_str(azure_provider_toml).unwrap();
    assert_eq!(expected_provider, provider);
}

#[test]
fn test_deserialize_azure_model_provider_toml() {
    let azure_provider_toml = r#"
name = "Azure"
base_url = "https://xxxxx.openai.azure.com/openai"
env_key = "AZURE_OPENAI_API_KEY"
query_params = { api-version = "2025-04-01-preview" }
        "#;
    let expected_provider = ModelProviderInfo {
        name: "Azure".into(),
        base_url: Some("https://xxxxx.openai.azure.com/openai".into()),
        env_key: Some("AZURE_OPENAI_API_KEY".into()),
        env_key_instructions: None,
        experimental_bearer_token: None,
        auth: None,
        aws: None,
        cursor_agent_service: None,
        wire_api: WireApi::Responses,
        query_params: Some(maplit::hashmap! {
            "api-version".to_string() => "2025-04-01-preview".to_string(),
        }),
        http_headers: None,
        env_http_headers: None,
        request_max_retries: None,
        stream_max_retries: None,
        stream_idle_timeout_ms: None,
        websocket_connect_timeout_ms: None,
        requires_openai_auth: false,
        supports_websockets: false,
        supports_standalone_web_search: false,
    };

    let provider: ModelProviderInfo = toml::from_str(azure_provider_toml).unwrap();
    assert_eq!(expected_provider, provider);
}

#[test]
fn test_deserialize_example_model_provider_toml() {
    let azure_provider_toml = r#"
name = "Example"
base_url = "https://example.com"
env_key = "API_KEY"
http_headers = { "X-Example-Header" = "example-value" }
env_http_headers = { "X-Example-Env-Header" = "EXAMPLE_ENV_VAR" }
supports_standalone_web_search = true
        "#;
    let expected_provider = ModelProviderInfo {
        name: "Example".into(),
        base_url: Some("https://example.com".into()),
        env_key: Some("API_KEY".into()),
        env_key_instructions: None,
        experimental_bearer_token: None,
        auth: None,
        aws: None,
        cursor_agent_service: None,
        wire_api: WireApi::Responses,
        query_params: None,
        http_headers: Some(maplit::hashmap! {
            "X-Example-Header".to_string() => "example-value".to_string(),
        }),
        env_http_headers: Some(maplit::hashmap! {
            "X-Example-Env-Header".to_string() => "EXAMPLE_ENV_VAR".to_string(),
        }),
        request_max_retries: None,
        stream_max_retries: None,
        stream_idle_timeout_ms: None,
        websocket_connect_timeout_ms: None,
        requires_openai_auth: false,
        supports_websockets: false,
        supports_standalone_web_search: true,
    };

    let provider: ModelProviderInfo = toml::from_str(azure_provider_toml).unwrap();
    assert_eq!(expected_provider, provider);
}

#[test]
fn test_deserialize_chat_wire_api_shows_helpful_error() {
    let provider_toml = r#"
name = "OpenAI using Chat Completions"
base_url = "https://api.openai.com/v1"
env_key = "OPENAI_API_KEY"
wire_api = "chat"
        "#;

    let err = toml::from_str::<ModelProviderInfo>(provider_toml).unwrap_err();
    assert!(err.to_string().contains(CHAT_WIRE_API_REMOVED_ERROR));
}

#[test]
fn test_deserialize_websocket_connect_timeout() {
    let provider_toml = r#"
name = "OpenAI"
base_url = "https://api.openai.com/v1"
websocket_connect_timeout_ms = 15000
supports_websockets = true
        "#;

    let provider: ModelProviderInfo = toml::from_str(provider_toml).unwrap();
    assert_eq!(provider.websocket_connect_timeout_ms, Some(15_000));
}

#[test]
fn test_supports_remote_compaction_for_openai() {
    let provider = ModelProviderInfo::create_openai_provider(/*base_url*/ None);

    assert!(provider.supports_remote_compaction());
}

#[test]
fn test_personal_access_token_uses_chatgpt_codex_base_url() {
    let api_provider = ModelProviderInfo::create_openai_provider(/*base_url*/ None)
        .to_api_provider(Some(AuthMode::PersonalAccessToken))
        .expect("OpenAI provider should build API provider");

    assert_eq!(api_provider.base_url, CHATGPT_CODEX_BASE_URL);
}

#[test]
fn test_header_auth_uses_chatgpt_codex_base_url() {
    let api_provider = ModelProviderInfo::create_openai_provider(/*base_url*/ None)
        .to_api_provider(Some(AuthMode::Headers))
        .expect("OpenAI provider should build API provider");

    assert_eq!(api_provider.base_url, CHATGPT_CODEX_BASE_URL);
}

#[test]
fn test_supports_remote_compaction_for_azure_name() {
    let provider = ModelProviderInfo {
        name: "Azure".into(),
        base_url: Some("https://example.com/openai".into()),
        env_key: Some("AZURE_OPENAI_API_KEY".into()),
        env_key_instructions: None,
        experimental_bearer_token: None,
        auth: None,
        aws: None,
        cursor_agent_service: None,
        wire_api: WireApi::Responses,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        request_max_retries: None,
        stream_max_retries: None,
        stream_idle_timeout_ms: None,
        websocket_connect_timeout_ms: None,
        requires_openai_auth: false,
        supports_websockets: false,
        supports_standalone_web_search: false,
    };

    assert!(provider.supports_remote_compaction());
}

#[test]
fn test_supports_remote_compaction_for_non_openai_non_azure_provider() {
    let provider = ModelProviderInfo {
        name: "Example".into(),
        base_url: Some("https://example.com/v1".into()),
        env_key: Some("API_KEY".into()),
        env_key_instructions: None,
        experimental_bearer_token: None,
        auth: None,
        aws: None,
        cursor_agent_service: None,
        wire_api: WireApi::Responses,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        request_max_retries: None,
        stream_max_retries: None,
        stream_idle_timeout_ms: None,
        websocket_connect_timeout_ms: None,
        requires_openai_auth: false,
        supports_websockets: false,
        supports_standalone_web_search: false,
    };

    assert!(!provider.supports_remote_compaction());
}

#[test]
fn test_uses_openai_actor_authorization() {
    let mut provider = ModelProviderInfo {
        http_headers: Some(maplit::hashmap! {
            "X-OpenAI-Actor-Authorization".to_string() => "actor-token".to_string(),
        }),
        ..ModelProviderInfo::default()
    };
    assert!(provider.uses_openai_actor_authorization());

    provider.http_headers = None;
    assert!(!provider.uses_openai_actor_authorization());

    provider.http_headers = Some(maplit::hashmap! {
        OPENAI_ACTOR_AUTHORIZATION_HEADER.to_string() => "  ".to_string(),
    });
    assert!(!provider.uses_openai_actor_authorization());

    provider.http_headers = Some(maplit::hashmap! {
        OPENAI_ACTOR_AUTHORIZATION_HEADER.to_string() => "actor-token".to_string(),
    });
    provider.requires_openai_auth = true;
    assert!(!provider.uses_openai_actor_authorization());
}

#[test]
fn test_deserialize_provider_auth_config_defaults() {
    let base_dir = tempdir().unwrap();
    let provider_toml = r#"
name = "Corp"

[auth]
command = "./scripts/print-token"
args = ["--format=text"]
        "#;

    let provider: ModelProviderInfo = {
        let _guard = AbsolutePathBufGuard::new(base_dir.path());
        toml::from_str(provider_toml).unwrap()
    };

    assert_eq!(
        provider.auth,
        Some(ModelProviderAuthInfo {
            command: "./scripts/print-token".to_string(),
            args: vec!["--format=text".to_string()],
            timeout_ms: NonZeroU64::new(5_000).unwrap(),
            refresh_interval_ms: 300_000,
            cwd: AbsolutePathBuf::resolve_path_against_base(".", base_dir.path()),
        })
    );
}

#[test]
fn test_deserialize_provider_aws_config() {
    let provider_toml = r#"
name = "Amazon Bedrock"
base_url = "https://bedrock.example.com/v1"

[aws]
profile = "codex-bedrock"
region = "us-west-2"
        "#;

    let provider: ModelProviderInfo = toml::from_str(provider_toml).unwrap();

    assert_eq!(
        provider.aws,
        Some(ModelProviderAwsAuthInfo {
            profile: Some("codex-bedrock".to_string()),
            region: Some("us-west-2".to_string()),
        })
    );
}

#[test]
fn test_create_amazon_bedrock_provider() {
    assert_eq!(
        ModelProviderInfo::create_amazon_bedrock_provider(/*aws*/ None),
        ModelProviderInfo {
            name: "Amazon Bedrock".to_string(),
            base_url: None,
            env_key: None,
            env_key_instructions: None,
            experimental_bearer_token: None,
            auth: None,
            aws: Some(ModelProviderAwsAuthInfo {
                profile: None,
                region: None,
            }),
            cursor_agent_service: None,
            wire_api: WireApi::Responses,
            query_params: None,
            http_headers: Some(maplit::hashmap! {
                AMAZON_BEDROCK_MANTLE_CLIENT_AGENT_HEADER.to_string() =>
                    AMAZON_BEDROCK_MANTLE_CLIENT_AGENT_VALUE.to_string(),
            }),
            env_http_headers: None,
            request_max_retries: None,
            stream_max_retries: None,
            stream_idle_timeout_ms: None,
            websocket_connect_timeout_ms: None,
            requires_openai_auth: false,
            supports_websockets: false,
            supports_standalone_web_search: false,
        }
    );
}

fn provider_auth_for_test() -> ModelProviderAuthInfo {
    ModelProviderAuthInfo {
        command: "token-fetcher".to_string(),
        args: vec!["fetch".to_string()],
        timeout_ms: NonZeroU64::new(5_000).expect("timeout should be non-zero"),
        refresh_interval_ms: 300_000,
        cwd: std::env::current_dir()
            .expect("current directory should be available")
            .try_into()
            .expect("current directory should be absolute"),
    }
}

#[test]
fn test_amazon_bedrock_provider_adds_mantle_client_agent_header() {
    let api_provider = ModelProviderInfo::create_amazon_bedrock_provider(/*aws*/ None)
        .to_api_provider(/*auth_mode*/ None)
        .expect("Amazon Bedrock provider should build API provider");

    assert_eq!(
        api_provider
            .headers
            .get(AMAZON_BEDROCK_MANTLE_CLIENT_AGENT_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some(AMAZON_BEDROCK_MANTLE_CLIENT_AGENT_VALUE)
    );
}

#[test]
fn test_built_in_model_providers_include_amazon_bedrock() {
    let providers = built_in_model_providers(/*openai_base_url*/ None);

    assert_eq!(
        providers
            .get(AMAZON_BEDROCK_PROVIDER_ID)
            .map(ModelProviderInfo::is_amazon_bedrock),
        Some(true)
    );
}

#[test]
fn test_merge_configured_model_providers_adds_custom_provider() {
    let custom_provider = ModelProviderInfo {
        name: "Custom".to_string(),
        base_url: Some("https://example.com/v1".to_string()),
        ..ModelProviderInfo::default()
    };
    let configured_model_providers =
        std::collections::HashMap::from([("custom".to_string(), custom_provider.clone())]);

    let mut expected = built_in_model_providers(/*openai_base_url*/ None);
    expected.insert("custom".to_string(), custom_provider);

    assert_eq!(
        merge_configured_model_providers(
            built_in_model_providers(/*openai_base_url*/ None),
            configured_model_providers,
        ),
        Ok(expected)
    );
}

#[test]
fn test_merge_configured_model_providers_rejects_invalid_cursor_provider() {
    let mut cursor_provider = cursor_agent_service_provider();
    cursor_provider.env_key = Some("CURSOR_TOKEN".to_string());
    let configured_model_providers =
        std::collections::HashMap::from([("cursor-corporate".to_string(), cursor_provider)]);

    assert_eq!(
        merge_configured_model_providers(
            built_in_model_providers(/*openai_base_url*/ None),
            configured_model_providers,
        ),
        Err(
            "model_providers.cursor-corporate: provider cursor_agent_service cannot be combined with env_key"
                .to_string()
        )
    );
}

#[test]
fn test_merge_configured_model_providers_applies_amazon_bedrock_profile_override() {
    let configured_model_providers = std::collections::HashMap::from([(
        AMAZON_BEDROCK_PROVIDER_ID.to_string(),
        ModelProviderInfo {
            aws: Some(ModelProviderAwsAuthInfo {
                profile: Some("codex-bedrock".to_string()),
                region: Some("us-west-2".to_string()),
            }),
            ..ModelProviderInfo::default()
        },
    )]);

    let mut expected = built_in_model_providers(/*openai_base_url*/ None);
    expected
        .get_mut(AMAZON_BEDROCK_PROVIDER_ID)
        .expect("Amazon Bedrock provider should be built in")
        .aws = Some(ModelProviderAwsAuthInfo {
        profile: Some("codex-bedrock".to_string()),
        region: Some("us-west-2".to_string()),
    });

    assert_eq!(
        merge_configured_model_providers(
            built_in_model_providers(/*openai_base_url*/ None),
            configured_model_providers,
        ),
        Ok(expected)
    );
}

#[test]
fn test_merge_configured_model_providers_applies_amazon_bedrock_transport_overrides() {
    let auth = provider_auth_for_test();
    let configured_model_providers = std::collections::HashMap::from([(
        AMAZON_BEDROCK_PROVIDER_ID.to_string(),
        ModelProviderInfo {
            base_url: Some("https://proxy.example.com/v1".to_string()),
            auth: Some(auth.clone()),
            aws: Some(ModelProviderAwsAuthInfo {
                profile: Some("codex-bedrock".to_string()),
                region: Some("us-west-2".to_string()),
            }),
            http_headers: Some(maplit::hashmap! {
                "x-example-header".to_string() => "value".to_string(),
            }),
            ..ModelProviderInfo::default()
        },
    )]);

    let mut expected = built_in_model_providers(/*openai_base_url*/ None);
    let expected_provider = expected
        .get_mut(AMAZON_BEDROCK_PROVIDER_ID)
        .expect("Amazon Bedrock provider should be built in");
    expected_provider.base_url = Some("https://proxy.example.com/v1".to_string());
    expected_provider.auth = Some(auth);
    expected_provider.aws = Some(ModelProviderAwsAuthInfo {
        profile: Some("codex-bedrock".to_string()),
        region: Some("us-west-2".to_string()),
    });
    expected_provider
        .http_headers
        .get_or_insert_default()
        .insert("x-example-header".to_string(), "value".to_string());

    assert_eq!(
        merge_configured_model_providers(
            built_in_model_providers(/*openai_base_url*/ None),
            configured_model_providers,
        ),
        Ok(expected)
    );
}

#[test]
fn test_merge_configured_model_providers_rejects_amazon_bedrock_non_default_fields() {
    let configured_model_providers = std::collections::HashMap::from([(
        AMAZON_BEDROCK_PROVIDER_ID.to_string(),
        ModelProviderInfo {
            name: "Custom Bedrock".to_string(),
            aws: Some(ModelProviderAwsAuthInfo {
                profile: Some("codex-bedrock".to_string()),
                region: None,
            }),
            ..ModelProviderInfo::default()
        },
    )]);

    assert_eq!(
        merge_configured_model_providers(
            built_in_model_providers(/*openai_base_url*/ None),
            configured_model_providers,
        ),
        Err(
            "model_providers.amazon-bedrock only supports changing `base_url`, `auth`, `http_headers`, `aws.profile`, and `aws.region`; other non-default provider fields are not supported"
                .to_string()
        )
    );
}

#[test]
fn test_merge_configured_model_providers_allows_amazon_bedrock_default_fields() {
    let configured_model_providers = std::collections::HashMap::from([(
        AMAZON_BEDROCK_PROVIDER_ID.to_string(),
        ModelProviderInfo {
            aws: Some(ModelProviderAwsAuthInfo {
                profile: None,
                region: None,
            }),
            wire_api: WireApi::Responses,
            ..ModelProviderInfo::default()
        },
    )]);

    assert_eq!(
        merge_configured_model_providers(
            built_in_model_providers(/*openai_base_url*/ None),
            configured_model_providers,
        ),
        Ok(built_in_model_providers(/*openai_base_url*/ None))
    );
}

#[test]
fn test_validate_provider_aws_rejects_conflicting_auth() {
    let provider = ModelProviderInfo {
        aws: Some(ModelProviderAwsAuthInfo {
            profile: None,
            region: None,
        }),
        env_key: Some("AWS_BEARER_TOKEN_BEDROCK".to_string()),
        supports_websockets: false,
        ..ModelProviderInfo::create_openai_provider(/*base_url*/ None)
    };

    assert_eq!(
        provider.validate(),
        Err("provider aws cannot be combined with env_key, requires_openai_auth".to_string())
    );
}

#[test]
fn test_validate_provider_aws_rejects_websockets() {
    let provider = ModelProviderInfo {
        aws: Some(ModelProviderAwsAuthInfo {
            profile: None,
            region: None,
        }),
        requires_openai_auth: false,
        supports_websockets: true,
        ..ModelProviderInfo::create_openai_provider(/*base_url*/ None)
    };

    assert_eq!(
        provider.validate(),
        Err("provider aws cannot be combined with supports_websockets".to_string())
    );
}

#[test]
fn test_deserialize_provider_auth_config_allows_zero_refresh_interval() {
    let base_dir = tempdir().unwrap();
    let provider_toml = r#"
name = "Corp"

[auth]
command = "./scripts/print-token"
refresh_interval_ms = 0
        "#;

    let provider: ModelProviderInfo = {
        let _guard = AbsolutePathBufGuard::new(base_dir.path());
        toml::from_str(provider_toml).unwrap()
    };

    let auth = provider.auth.expect("auth config should deserialize");
    assert_eq!(auth.refresh_interval_ms, 0);
    assert_eq!(auth.refresh_interval(), None);
}

#[test]
fn deserialize_cursor_agent_service_provider_requires_the_nested_contract() {
    let provider_toml = r#"
name = "Cursor Corporate"
wire_api = "cursor_agent_service"
requires_openai_auth = false

[cursor_agent_service]
expected_user_id = 390777501
expected_team_id = 12565657
expected_service_origin = "https://agentn.global.api5.cursor.sh"
context_window_tokens = 65536
effective_context_window_percent = 75
max_pending_tool_actions = 8
        "#;

    let provider: ModelProviderInfo = toml::from_str(provider_toml).unwrap();

    assert_eq!(provider, cursor_agent_service_provider());
    assert_eq!(provider.validate(), Ok(()));
    assert_eq!(provider.request_max_retries(), 0);
    assert_eq!(provider.stream_max_retries(), 0);
    let explicit_zero_retries = ModelProviderInfo {
        request_max_retries: Some(0),
        stream_max_retries: Some(0),
        ..provider.clone()
    };
    assert_eq!(explicit_zero_retries.validate(), Ok(()));
    assert!(
        toml::to_string(&provider)
            .unwrap()
            .contains("wire_api = \"cursor_agent_service\"")
    );
}

#[test]
fn cursor_agent_service_provider_rejects_each_generic_provider_surface() {
    let provider = cursor_agent_service_provider();
    let conflicting_providers = vec![
        (
            "base_url",
            ModelProviderInfo {
                base_url: Some("https://example.com".to_string()),
                ..provider.clone()
            },
        ),
        (
            "env_key",
            ModelProviderInfo {
                env_key: Some("CURSOR_TOKEN".to_string()),
                ..provider.clone()
            },
        ),
        (
            "env_key_instructions",
            ModelProviderInfo {
                env_key_instructions: Some("export a token".to_string()),
                ..provider.clone()
            },
        ),
        (
            "experimental_bearer_token",
            ModelProviderInfo {
                experimental_bearer_token: Some("secret".to_string()),
                ..provider.clone()
            },
        ),
        (
            "auth",
            ModelProviderInfo {
                auth: Some(provider_auth_for_test()),
                ..provider.clone()
            },
        ),
        (
            "aws",
            ModelProviderInfo {
                aws: Some(ModelProviderAwsAuthInfo {
                    profile: None,
                    region: None,
                }),
                ..provider.clone()
            },
        ),
        (
            "query_params",
            ModelProviderInfo {
                query_params: Some(HashMap::from([("trace".to_string(), "1".to_string())])),
                ..provider.clone()
            },
        ),
        (
            "http_headers",
            ModelProviderInfo {
                http_headers: Some(HashMap::from([(
                    "authorization".to_string(),
                    "Bearer secret".to_string(),
                )])),
                ..provider.clone()
            },
        ),
        (
            "env_http_headers",
            ModelProviderInfo {
                env_http_headers: Some(HashMap::from([(
                    "authorization".to_string(),
                    "CURSOR_TOKEN".to_string(),
                )])),
                ..provider.clone()
            },
        ),
        (
            "requires_openai_auth",
            ModelProviderInfo {
                requires_openai_auth: true,
                ..provider.clone()
            },
        ),
        (
            "supports_websockets",
            ModelProviderInfo {
                supports_websockets: true,
                ..provider.clone()
            },
        ),
        (
            "supports_standalone_web_search",
            ModelProviderInfo {
                supports_standalone_web_search: true,
                ..provider.clone()
            },
        ),
        (
            "request_max_retries",
            ModelProviderInfo {
                request_max_retries: Some(1),
                ..provider.clone()
            },
        ),
        (
            "stream_max_retries",
            ModelProviderInfo {
                stream_max_retries: Some(1),
                ..provider
            },
        ),
    ];

    assert_eq!(conflicting_providers.len(), 14);
    for (field, conflicting_provider) in conflicting_providers {
        assert_eq!(
            conflicting_provider.validate(),
            Err(format!(
                "provider cursor_agent_service cannot be combined with {field}"
            ))
        );
    }
}

#[test]
fn cursor_agent_service_provider_rejects_missing_or_mismatched_nested_config() {
    let missing = ModelProviderInfo {
        name: "Cursor Corporate".to_string(),
        wire_api: WireApi::CursorAgentService,
        ..ModelProviderInfo::default()
    };
    assert_eq!(
        missing.validate(),
        Err(
            "wire_api cursor_agent_service requires cursor_agent_service configuration".to_string()
        )
    );

    let mismatched = ModelProviderInfo {
        cursor_agent_service: cursor_agent_service_provider().cursor_agent_service,
        ..ModelProviderInfo::default()
    };
    assert_eq!(
        mismatched.validate(),
        Err(
            "cursor_agent_service configuration requires wire_api cursor_agent_service".to_string()
        )
    );
}

#[test]
fn cursor_agent_service_provider_rejects_invalid_nested_values() {
    let provider = cursor_agent_service_provider();
    let config = provider.cursor_agent_service.clone().unwrap();
    let invalid_configs = vec![
        (
            "expected_user_id must be greater than zero",
            CursorAgentServiceProviderInfo {
                expected_user_id: 0,
                ..config.clone()
            },
        ),
        (
            "expected_team_id must be greater than zero",
            CursorAgentServiceProviderInfo {
                expected_team_id: 0,
                ..config.clone()
            },
        ),
        (
            "expected_service_origin must equal https://agentn.global.api5.cursor.sh",
            CursorAgentServiceProviderInfo {
                expected_service_origin: "http://agentn.global.api5.cursor.sh".to_string(),
                ..config.clone()
            },
        ),
        (
            "expected_service_origin must equal https://agentn.global.api5.cursor.sh",
            CursorAgentServiceProviderInfo {
                expected_service_origin: "https://agentn.global.api5.cursor.sh/path".to_string(),
                ..config.clone()
            },
        ),
        (
            "expected_service_origin must equal https://agentn.global.api5.cursor.sh",
            CursorAgentServiceProviderInfo {
                expected_service_origin: "https://attacker.example".to_string(),
                ..config.clone()
            },
        ),
        (
            "context_window_tokens must be greater than zero",
            CursorAgentServiceProviderInfo {
                context_window_tokens: 0,
                ..config.clone()
            },
        ),
        (
            "context_window_tokens must be at most 65536",
            CursorAgentServiceProviderInfo {
                context_window_tokens: 65_537,
                ..config.clone()
            },
        ),
        (
            "effective_context_window_percent must be between 1 and 75",
            CursorAgentServiceProviderInfo {
                effective_context_window_percent: 0,
                ..config.clone()
            },
        ),
        (
            "effective_context_window_percent must be between 1 and 75",
            CursorAgentServiceProviderInfo {
                effective_context_window_percent: 76,
                ..config.clone()
            },
        ),
        (
            "max_pending_tool_actions must be greater than zero",
            CursorAgentServiceProviderInfo {
                max_pending_tool_actions: 0,
                ..config.clone()
            },
        ),
        (
            "max_pending_tool_actions must be at most 8",
            CursorAgentServiceProviderInfo {
                max_pending_tool_actions: 9,
                ..config
            },
        ),
    ];

    assert_eq!(invalid_configs.len(), 11);
    for (expected_error, cursor_agent_service) in invalid_configs {
        let invalid_provider = ModelProviderInfo {
            cursor_agent_service: Some(cursor_agent_service),
            ..provider.clone()
        };
        assert_eq!(
            invalid_provider.validate(),
            Err(format!("cursor_agent_service.{expected_error}"))
        );
    }
}

#[test]
fn cursor_agent_service_provider_does_not_build_an_openai_api_provider() {
    assert_eq!(
        cursor_agent_service_provider()
            .to_api_provider(/*auth_mode*/ None)
            .unwrap_err()
            .to_string(),
        "cursor_agent_service providers do not use the OpenAI-compatible API client"
    );
}

fn cursor_agent_service_provider() -> ModelProviderInfo {
    ModelProviderInfo {
        name: "Cursor Corporate".to_string(),
        wire_api: WireApi::CursorAgentService,
        cursor_agent_service: Some(CursorAgentServiceProviderInfo {
            expected_user_id: 390777501,
            expected_team_id: 12565657,
            expected_service_origin: "https://agentn.global.api5.cursor.sh".to_string(),
            context_window_tokens: 65_536,
            effective_context_window_percent: 75,
            max_pending_tool_actions: 8,
        }),
        ..ModelProviderInfo::default()
    }
}
