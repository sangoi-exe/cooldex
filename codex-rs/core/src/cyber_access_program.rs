//! Forward explicit programs while leaving entitlement and model policy to the server.

use codex_api::AccessPrograms;
use codex_features::Feature;
use codex_login::CodexAuth;
use codex_model_provider_info::OPENAI_PROVIDER_ID;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::turn_input::CyberAccessProgram;

/// Configured support for explicit Cyber programs on API-key requests.
#[derive(Clone, Copy, Debug)]
pub enum ApiKeyCyberAccessPrograms {
    UnsupportedProvider,
    Disabled,
    Enabled,
}

impl ApiKeyCyberAccessPrograms {
    /// Captures the same provider and feature policy used by ordinary model requests.
    pub fn from_config(config: &crate::config::Config) -> Self {
        if config.model_provider_id != OPENAI_PROVIDER_ID {
            Self::UnsupportedProvider
        } else if config.features.enabled(Feature::ApiKeyCyberAccessPrograms) {
            Self::Enabled
        } else {
            Self::Disabled
        }
    }
}

pub(crate) fn for_provider(
    provider_id: &str,
    program: Option<CyberAccessProgram>,
) -> Option<CyberAccessProgram> {
    program.filter(|_| provider_id == OPENAI_PROVIDER_ID)
}

// Merge-safety anchor: recipient role policy outranks the incoming program at the effective child-model boundary.
pub(crate) fn for_recipient_role(
    policy: Option<bool>,
    provider_id: &str,
    model: &ModelInfo,
    incoming: Option<CyberAccessProgram>,
) -> Result<Option<CyberAccessProgram>> {
    match policy {
        None => Ok(for_provider(provider_id, incoming)),
        Some(false) => Ok(for_provider(
            provider_id,
            Some(CyberAccessProgram::Standard),
        )),
        Some(true) => {
            if provider_id != OPENAI_PROVIDER_ID {
                return Err(CodexErr::InvalidRequest(format!(
                    "Daybreak is not supported by model provider `{provider_id}` for model `{}`.",
                    model.slug,
                )));
            }
            model
                .available_access_programs
                .as_ref()
                .and_then(|programs| programs.daybreak())
                .map(Some)
                .ok_or_else(|| {
                    CodexErr::InvalidRequest(format!(
                        "Daybreak is not supported by model `{}`.",
                        model.slug,
                    ))
                })
        }
    }
}

/// Resolves an explicit program using the request's current authentication.
pub fn for_auth(
    auth: Option<&CodexAuth>,
    program: Option<CyberAccessProgram>,
    policy: ApiKeyCyberAccessPrograms,
) -> Result<Option<AccessPrograms>> {
    let Some(program) = program else {
        return Ok(None);
    };
    let Some(auth) = auth else {
        return Ok(None);
    };
    if auth.is_chatgpt_auth() {
        return Ok(Some(program.into()));
    }
    if !auth.is_api_key_auth() {
        return Ok(None);
    }
    match policy {
        ApiKeyCyberAccessPrograms::UnsupportedProvider => Ok(None),
        ApiKeyCyberAccessPrograms::Disabled => Err(CodexErr::InvalidRequest(
            "Cyber access programs are disabled for this API-key session.".to_owned(),
        )),
        ApiKeyCyberAccessPrograms::Enabled => Ok(Some(program.into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::openai_models::ModelAccessPrograms;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn recipient_role_selects_native_daybreak_instead_of_incoming_program() {
        let config = crate::config::test_config().await;
        let mut model = codex_models_manager::test_support::construct_model_info_offline_for_tests(
            "test-model",
            &config.to_models_manager_config(),
        );
        for native in [
            CyberAccessProgram::DaybreakBlue,
            CyberAccessProgram::DaybreakRed,
        ] {
            model.available_access_programs = Some(ModelAccessPrograms {
                cyber: vec![native],
            });
            assert_eq!(
                for_recipient_role(
                    Some(true),
                    OPENAI_PROVIDER_ID,
                    &model,
                    Some(CyberAccessProgram::Standard)
                )
                .expect("supported native program"),
                Some(native)
            );
            assert_eq!(
                for_recipient_role(Some(false), OPENAI_PROVIDER_ID, &model, Some(native))
                    .expect("explicit standard"),
                Some(CyberAccessProgram::Standard)
            );
            assert_eq!(
                for_recipient_role(None, OPENAI_PROVIDER_ID, &model, Some(native))
                    .expect("inherit program"),
                Some(native)
            );
            assert_eq!(
                for_recipient_role(None, OPENAI_PROVIDER_ID, &model, None)
                    .expect("inherit absence"),
                None
            );
        }
    }

    #[tokio::test]
    async fn recipient_role_rejects_unsupported_daybreak_without_fallback() {
        let config = crate::config::test_config().await;
        let mut model = codex_models_manager::test_support::construct_model_info_offline_for_tests(
            "test-model",
            &config.to_models_manager_config(),
        );
        for programs in [
            None,
            Some(ModelAccessPrograms { cyber: Vec::new() }),
            Some(ModelAccessPrograms {
                cyber: vec![CyberAccessProgram::Standard],
            }),
        ] {
            model.available_access_programs = programs;
            let error = for_recipient_role(
                Some(true),
                OPENAI_PROVIDER_ID,
                &model,
                Some(CyberAccessProgram::Standard),
            )
            .expect_err("unsupported selection must fail");
            assert!(error.to_string().contains("Daybreak"));
            assert!(error.to_string().contains("test-model"));
        }
        model.available_access_programs = Some(ModelAccessPrograms {
            cyber: vec![CyberAccessProgram::DaybreakBlue],
        });
        assert!(for_recipient_role(Some(true), "custom", &model, None).is_err());
        assert_eq!(
            for_recipient_role(
                None,
                "custom",
                &model,
                Some(CyberAccessProgram::DaybreakBlue)
            )
            .expect("preserve provider policy"),
            None
        );
    }
}
