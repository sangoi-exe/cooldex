use codex_login::AuthManager;
use codex_login::AuthManagerConfig;
use codex_login::token_data::TokenData;
use std::sync::LazyLock;
use std::sync::RwLock;

static CHATGPT_TOKEN: LazyLock<RwLock<Option<TokenData>>> = LazyLock::new(|| RwLock::new(None));

pub fn get_chatgpt_token_data() -> Option<TokenData> {
    CHATGPT_TOKEN.read().ok()?.clone()
}

pub fn set_chatgpt_token_data(value: TokenData) {
    if let Ok(mut guard) = CHATGPT_TOKEN.write() {
        *guard = Some(value);
    }
}

/// Initialize the ChatGPT token from auth.json file
pub async fn init_chatgpt_token_from_auth(config: &impl AuthManagerConfig) -> std::io::Result<()> {
    // Merge-safety anchor: ChatGPT token bootstrap receives resolved config and
    // must preserve sqlite_home plus forced workspace before account-runtime
    // state hydrates, so request tokens follow the same owner as TUI/CLI auth.
    let auth_manager =
        AuthManager::shared_from_config(config, /*enable_codex_api_key_env*/ false);
    if let Some(auth) = auth_manager.auth().await {
        let token_data = auth.get_token_data()?;
        set_chatgpt_token_data(token_data);
    }
    Ok(())
}
